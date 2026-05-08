use std::convert::Infallible;

use axum::{
    extract::{Query, State},
    response::sse::{Event, Sse},
};
use futures::Stream;
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use tokio::sync::broadcast::error::RecvError;

use crate::{db::escape_like, state::SharedState};

#[derive(Deserialize)]
pub struct WatchQuery {
    #[serde(default)]
    pub since: i64,
    pub prefix: Option<String>,
}

pub async fn watch(
    State(state): State<SharedState>,
    Query(q): Query<WatchQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let pool = state.pool.clone();
    let mut rx = state.events_tx.subscribe();
    let heartbeat = state.config.watch_heartbeat;
    let prefix = q.prefix.unwrap_or_default();
    let mut last_emitted = q.since.max(0);

    let stream = async_stream::stream! {
        // Phase 1: catch up from kv_events.
        let pat = format!("{}%", escape_like(&prefix));
        let catchup = sqlx::query(
            "SELECT id, key, operation FROM kv_events \
             WHERE id > $1 AND key LIKE $2 ESCAPE '\\' \
             ORDER BY id ASC",
        )
        .bind(last_emitted)
        .bind(&pat)
        .fetch_all(&pool)
        .await;

        match catchup {
            Ok(rows) => {
                for r in rows {
                    let id: i64 = match r.try_get("id") { Ok(v) => v, Err(_) => continue };
                    let key: String = match r.try_get("key") { Ok(v) => v, Err(_) => continue };
                    let op: String = match r.try_get("operation") { Ok(v) => v, Err(_) => continue };
                    last_emitted = id;
                    let payload = json!({
                        "event_id": id,
                        "key": key,
                        "operation": op,
                    });
                    let evt = Event::default()
                        .event("kv_changed")
                        .id(id.to_string())
                        .data(payload.to_string());
                    yield Ok::<Event, Infallible>(evt);
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "watch: catch-up query failed");
                let evt = Event::default()
                    .event("error")
                    .data(json!({"message": "catch-up failed"}).to_string());
                yield Ok(evt);
                return;
            }
        }

        // Phase 2: live events.
        let mut hb = tokio::time::interval(heartbeat);
        hb.tick().await; // skip the immediate first tick

        loop {
            tokio::select! {
                _ = hb.tick() => {
                    let evt = Event::default().event("heartbeat").data("{}");
                    yield Ok(evt);
                }
                recv = rx.recv() => {
                    match recv {
                        Ok(ev) => {
                            if !prefix.is_empty() && !ev.key.starts_with(&prefix) {
                                continue;
                            }
                            if ev.event_id <= last_emitted {
                                continue;
                            }
                            last_emitted = ev.event_id;
                            let payload = json!({
                                "event_id": ev.event_id,
                                "key": ev.key,
                                "operation": ev.operation.as_str(),
                            });
                            let evt = Event::default()
                                .event("kv_changed")
                                .id(ev.event_id.to_string())
                                .data(payload.to_string());
                            yield Ok(evt);
                        }
                        Err(RecvError::Lagged(n)) => {
                            tracing::warn!(missed = n, "watch: subscriber lagged");
                            let evt = Event::default()
                                .event("reload")
                                .data(json!({"reason": "lagged"}).to_string());
                            yield Ok(evt);
                            return;
                        }
                        Err(RecvError::Closed) => return,
                    }
                }
            }
        }
    };

    Sse::new(stream)
}
