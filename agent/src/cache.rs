use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::RwLock;

use serde_json::Value;
use tokio::sync::broadcast;

/// One key's value plus the event_id at which we last updated it.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub value: Value,
    pub event_id: i64,
}

/// State for one watched prefix: the cache subtree, the watch cursor, and a
/// notifier that wakes long-pollers whenever anything under this prefix
/// changes.
pub struct PrefixState {
    pub prefix: String,
    items: RwLock<HashMap<String, CacheEntry>>,
    pub latest_event_id: AtomicI64,
    pub ready: AtomicBool,
    /// Sends a `()` whenever any item under this prefix changes (set,
    /// delete, or snapshot replace). Long-pollers subscribe to this and
    /// re-check after each notification.
    pub notify: broadcast::Sender<()>,
}

impl PrefixState {
    pub fn new(prefix: String) -> Self {
        let (tx, _) = broadcast::channel(64);
        Self {
            prefix,
            items: RwLock::new(HashMap::new()),
            latest_event_id: AtomicI64::new(0),
            ready: AtomicBool::new(false),
            notify: tx,
        }
    }

    pub fn get(&self, key: &str) -> Option<CacheEntry> {
        self.items.read().expect("cache rwlock").get(key).cloned()
    }

    pub fn list(&self) -> Vec<(String, CacheEntry)> {
        let g = self.items.read().expect("cache rwlock");
        let mut v: Vec<_> = g.iter().map(|(k, e)| (k.clone(), e.clone())).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    pub fn upsert(&self, key: String, value: Value, event_id: i64) {
        {
            let mut g = self.items.write().expect("cache rwlock");
            g.insert(key, CacheEntry { value, event_id });
        }
        self.advance(event_id);
        let _ = self.notify.send(());
    }

    pub fn remove(&self, key: &str, event_id: i64) {
        {
            let mut g = self.items.write().expect("cache rwlock");
            g.remove(key);
        }
        self.advance(event_id);
        let _ = self.notify.send(());
    }

    pub fn replace_snapshot(&self, items: HashMap<String, CacheEntry>, latest_event_id: i64) {
        {
            let mut g = self.items.write().expect("cache rwlock");
            *g = items;
        }
        // Snapshot resets the cursor, even if downward (the server told us
        // older event_ids are no longer valid).
        self.latest_event_id.store(latest_event_id, Ordering::Relaxed);
        self.ready.store(true, Ordering::Relaxed);
        let _ = self.notify.send(());
    }

    fn advance(&self, event_id: i64) {
        // Monotonic: only increase.
        loop {
            let cur = self.latest_event_id.load(Ordering::Relaxed);
            if event_id <= cur {
                return;
            }
            if self
                .latest_event_id
                .compare_exchange(cur, event_id, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn upsert_and_get() {
        let p = PrefixState::new("services/payment/".into());
        p.upsert("services/payment/timeout_ms".into(), json!(3000), 5);

        let e = p.get("services/payment/timeout_ms").unwrap();
        assert_eq!(e.value, json!(3000));
        assert_eq!(e.event_id, 5);
        assert_eq!(p.latest_event_id.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn remove_drops_entry() {
        let p = PrefixState::new("p/".into());
        p.upsert("p/a".into(), json!(1), 1);
        p.remove("p/a", 2);
        assert!(p.get("p/a").is_none());
        assert_eq!(p.latest_event_id.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn replace_snapshot_resets_cursor_and_marks_ready() {
        let p = PrefixState::new("p/".into());
        p.upsert("p/old".into(), json!(1), 7);
        assert!(!p.ready.load(Ordering::Relaxed));

        let mut snap = HashMap::new();
        snap.insert(
            "p/new".into(),
            CacheEntry {
                value: json!(2),
                event_id: 9,
            },
        );
        p.replace_snapshot(snap, 9);

        assert!(p.get("p/old").is_none());
        assert_eq!(p.get("p/new").unwrap().value, json!(2));
        assert_eq!(p.latest_event_id.load(Ordering::Relaxed), 9);
        assert!(p.ready.load(Ordering::Relaxed));
    }

    #[test]
    fn cursor_only_advances_forward() {
        let p = PrefixState::new("p/".into());
        p.upsert("p/a".into(), json!(1), 5);
        // An older event_id should not push the cursor backwards.
        p.upsert("p/b".into(), json!(2), 3);
        assert_eq!(p.latest_event_id.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn list_is_sorted() {
        let p = PrefixState::new("p/".into());
        p.upsert("p/c".into(), json!(3), 3);
        p.upsert("p/a".into(), json!(1), 1);
        p.upsert("p/b".into(), json!(2), 2);
        let keys: Vec<_> = p.list().into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, vec!["p/a", "p/b", "p/c"]);
    }
}
