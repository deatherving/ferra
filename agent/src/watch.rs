use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde::Deserialize;
use tracing::{info, warn};

use crate::cache::{CacheEntry, PrefixState};
use crate::sse::SseParser;
use crate::upstream::UpstreamClient;

#[derive(Debug, Deserialize)]
struct WatchPayload {
    event_id: i64,
    key: String,
    operation: String,
}

/// One watch task per prefix. Loops forever:
///   - if not yet ready, pull a snapshot
///   - open the SSE watch and apply events to the cache
///   - on any transport error, exponential-backoff reconnect
///
/// Reads against the cache continue serving last-known-good values
/// throughout reconnect / backoff.
pub async fn run_loop(
    state: Arc<PrefixState>,
    upstream: UpstreamClient,
    min_backoff: Duration,
    max_backoff: Duration,
) {
    let mut backoff = min_backoff;
    loop {
        match run_once(&state, &upstream).await {
            Ok(()) => {
                info!(prefix = %state.prefix, "watch stream ended cleanly; reconnecting");
                backoff = min_backoff;
            }
            Err(e) => {
                warn!(
                    prefix = %state.prefix,
                    error = %e,
                    backoff_ms = backoff.as_millis() as u64,
                    "watch failed; backing off",
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
            }
        }
    }
}

async fn run_once(state: &PrefixState, upstream: &UpstreamClient) -> anyhow::Result<()> {
    if !state.ready.load(Ordering::Relaxed) {
        load_snapshot(state, upstream).await?;
    }

    let since = state.latest_event_id.load(Ordering::Relaxed);
    let resp = upstream.open_watch(&state.prefix, since).await?;

    let mut stream = resp.bytes_stream();
    let mut sse = SseParser::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let evs = sse.feed(&chunk);
        for ev in evs {
            // Any handler error bubbles up so the outer loop reconnects.
            handle_event(state, upstream, ev).await?;
        }
    }
    Ok(())
}

async fn load_snapshot(state: &PrefixState, upstream: &UpstreamClient) -> anyhow::Result<()> {
    let snap = upstream.snapshot(&state.prefix).await?;
    let mut items = HashMap::with_capacity(snap.items.len());
    for it in snap.items {
        items.insert(
            it.key,
            CacheEntry {
                value: it.value,
                event_id: it.event_id,
            },
        );
    }
    info!(
        prefix = %state.prefix,
        size = items.len(),
        latest_event_id = snap.latest_event_id,
        "snapshot loaded",
    );
    state.replace_snapshot(items, snap.latest_event_id);
    Ok(())
}

async fn handle_event(
    state: &PrefixState,
    upstream: &UpstreamClient,
    ev: crate::sse::SseEvent,
) -> anyhow::Result<()> {
    match ev.event.as_deref() {
        Some("kv_changed") => {
            let payload: WatchPayload = match serde_json::from_str(&ev.data) {
                Ok(p) => p,
                Err(e) => {
                    warn!(error = %e, data = %ev.data, "malformed kv_changed payload");
                    return Ok(());
                }
            };
            match payload.operation.as_str() {
                "set" => {
                    if let Some(got) = upstream.get_key(&payload.key).await? {
                        // Use the freshly-fetched value's event_id as the
                        // cursor advance (it may be > the watch event's id
                        // if a more recent write raced us).
                        let event_id = got.event_id.max(payload.event_id);
                        state.upsert(payload.key, got.value, event_id);
                    } else {
                        // 404: deleted between the event and our GET; the
                        // next event will catch us up.
                        state.advance_only(payload.event_id);
                    }
                }
                "delete" => {
                    state.remove(&payload.key, payload.event_id);
                }
                op => warn!(op, "unknown operation"),
            }
        }
        Some("reload") => {
            warn!(prefix = %state.prefix, data = %ev.data, "server requested reload");
            load_snapshot(state, upstream).await?;
        }
        Some("heartbeat") | None => {}
        Some("error") => warn!(data = %ev.data, "server error event"),
        Some(other) => tracing::debug!(other, "unknown event type"),
    }
    Ok(())
}

// Helper added to PrefixState below — declared as a trait extension so we
// don't need to weaken cache.rs's encapsulation.
trait AdvanceOnly {
    fn advance_only(&self, event_id: i64);
}

impl AdvanceOnly for PrefixState {
    fn advance_only(&self, event_id: i64) {
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
                let _ = self.notify.send(());
                return;
            }
        }
    }
}
