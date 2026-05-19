//! Cross-replica event fan-out via Postgres LISTEN/NOTIFY.
//!
//! Each ferra-server replica runs one of these tasks. Writes on *any*
//! replica issue `NOTIFY ferra_kv_events, <json>` in the same transaction
//! as the `kv_events` INSERT; every replica's listener picks up the
//! notification and forwards it into the local broadcast channel that
//! feeds SSE subscribers. The net effect: an agent connected to replica 1
//! sees writes that happened on replica 2 with the same sub-second
//! latency it would see local writes.
//!
//! Durability: `NOTIFY` is per-connection — if the listener connection is
//! dropped, notifications during the gap are lost. We bridge the gap by
//! replaying from `kv_events` on reconnect.

use std::time::Duration;

use serde::Deserialize;
use sqlx::postgres::PgListener;
use sqlx::{PgPool, Row};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::events::{ChangeEvent, Operation};

pub const CHANNEL: &str = "ferra_kv_events";

#[derive(Deserialize)]
struct NotifyPayload {
    event_id: i64,
    key: String,
    operation: String,
}

/// Long-running task. Caller is expected to `tokio::spawn` it once per
/// process during startup. The task never returns by design — on any
/// underlying error it logs and reconnects after a short backoff.
pub async fn run(pool: PgPool, events_tx: broadcast::Sender<ChangeEvent>) {
    let backoff = Duration::from_secs(1);
    loop {
        match run_once(&pool, &events_tx).await {
            Ok(()) => {
                warn!("kv_events listener loop exited cleanly; restarting");
            }
            Err(e) => {
                warn!(
                    error = %e,
                    backoff_ms = backoff.as_millis() as u64,
                    "kv_events listener failed; reconnecting",
                );
            }
        }
        tokio::time::sleep(backoff).await;
    }
}

async fn run_once(
    pool: &PgPool,
    events_tx: &broadcast::Sender<ChangeEvent>,
) -> anyhow::Result<()> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen(CHANNEL).await?;

    // Initialize at the current tip. Historical events are served by
    // /v1/watch's own catch-up against kv_events, not by re-broadcasting
    // them through the fanout channel on every startup.
    let mut last_seen: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM kv_events")
        .fetch_one(pool)
        .await?;
    info!(channel = CHANNEL, last_seen, "kv_events listener ready");

    loop {
        match listener.try_recv().await? {
            Some(notif) => {
                if let Some(ev) = parse_payload(notif.payload()) {
                    if ev.event_id <= last_seen {
                        // Already broadcast via catch-up. Possible when a
                        // NOTIFY for a recent write arrives in the listener
                        // buffer while we were running the catch-up SELECT.
                        continue;
                    }
                    last_seen = ev.event_id;
                    let _ = events_tx.send(ev);
                }
            }
            None => {
                // PgListener dropped and reconnected. Notifications during
                // the gap were lost; replay them from kv_events.
                info!(last_seen, "kv_events listener reconnected; replaying gap");
                last_seen = replay_gap(pool, last_seen, events_tx).await?;
            }
        }
    }
}

fn parse_payload(raw: &str) -> Option<ChangeEvent> {
    let p: NotifyPayload = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, payload = raw, "malformed kv_events notification");
            return None;
        }
    };
    let operation = match p.operation.as_str() {
        "set" => Operation::Set,
        "delete" => Operation::Delete,
        other => {
            warn!(operation = other, "unknown operation in notification");
            return None;
        }
    };
    Some(ChangeEvent {
        event_id: p.event_id,
        key: p.key,
        operation,
    })
}

async fn replay_gap(
    pool: &PgPool,
    since: i64,
    events_tx: &broadcast::Sender<ChangeEvent>,
) -> anyhow::Result<i64> {
    let rows = sqlx::query(
        "SELECT id, key, operation FROM kv_events WHERE id > $1 ORDER BY id ASC",
    )
    .bind(since)
    .fetch_all(pool)
    .await?;

    let mut last = since;
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let key: String = row.try_get("key")?;
        let op: String = row.try_get("operation")?;
        let operation = match op.as_str() {
            "set" => Operation::Set,
            "delete" => Operation::Delete,
            other => {
                warn!(operation = other, "unknown operation in gap replay");
                continue;
            }
        };
        last = id;
        let _ = events_tx.send(ChangeEvent {
            event_id: id,
            key,
            operation,
        });
    }
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::parse_payload;
    use crate::events::Operation;

    #[test]
    fn parses_set_payload() {
        let ev = parse_payload(r#"{"event_id":42,"key":"k","operation":"set"}"#).unwrap();
        assert_eq!(ev.event_id, 42);
        assert_eq!(ev.key, "k");
        assert!(matches!(ev.operation, Operation::Set));
    }

    #[test]
    fn parses_delete_payload() {
        let ev = parse_payload(r#"{"event_id":7,"key":"x","operation":"delete"}"#).unwrap();
        assert!(matches!(ev.operation, Operation::Delete));
    }

    #[test]
    fn returns_none_on_unknown_operation() {
        assert!(parse_payload(r#"{"event_id":1,"key":"k","operation":"upsert"}"#).is_none());
    }

    #[test]
    fn returns_none_on_malformed_json() {
        assert!(parse_payload("not json").is_none());
    }
}
