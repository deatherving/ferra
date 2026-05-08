use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::trace::TraceLayer;

use crate::cache::PrefixState;
use crate::upstream::UpstreamClient;

pub struct AgentState {
    pub upstream: UpstreamClient,
    pub prefixes: Vec<Arc<PrefixState>>,
}

impl AgentState {
    pub fn find_prefix(&self, key: &str) -> Option<&Arc<PrefixState>> {
        // Linear scan; prefix counts are typically tens, not thousands.
        // Pick the longest matching prefix so `/cfg/services/payment/foo`
        // prefers `services/payment/` over `services/`.
        self.prefixes
            .iter()
            .filter(|p| key.starts_with(&p.prefix))
            .max_by_key(|p| p.prefix.len())
    }

    pub fn find_prefix_for_listing<'a>(&'a self, prefix: &str) -> Option<&'a Arc<PrefixState>> {
        // For list queries the *requested* prefix must equal or extend one of
        // our watched prefixes; we then filter the relevant cache subtree.
        self.prefixes
            .iter()
            .filter(|p| prefix.starts_with(&p.prefix))
            .max_by_key(|p| p.prefix.len())
    }

    pub fn all_ready(&self) -> bool {
        self.prefixes
            .iter()
            .all(|p| p.ready.load(Ordering::Relaxed))
    }
}

pub fn router(state: Arc<AgentState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/cfg", get(list_handler))
        .route("/cfg/*key", get(cfg_handler))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status": "ok"})))
}

async fn readyz(State(state): State<Arc<AgentState>>) -> impl IntoResponse {
    if state.all_ready() {
        (StatusCode::OK, Json(json!({"status": "ok"})))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status": "loading"})),
        )
    }
}

#[derive(Deserialize)]
struct CfgQuery {
    /// If set, hold the request for at most this long while waiting for
    /// `key`'s `event_id` to exceed `since`. Parsed via `humantime`,
    /// e.g. `30s`, `1m`, `500ms`. Capped at 5 minutes.
    wait: Option<String>,
    /// Cursor — return immediately when key's `event_id > since`.
    #[serde(default)]
    since: i64,
}

async fn cfg_handler(
    State(state): State<Arc<AgentState>>,
    Path(key): Path<String>,
    Query(q): Query<CfgQuery>,
) -> Response {
    let Some(prefix) = state.find_prefix(&key) else {
        return error_response(
            StatusCode::NOT_FOUND,
            "key_not_in_watched_prefix",
            &format!("no watched prefix matches key {:?}", key),
        );
    };

    let wait = q.wait.as_deref().and_then(parse_wait);

    match wait {
        None => immediate_response(prefix, &key),
        Some(wait) => long_poll(prefix.clone(), key, q.since, wait).await,
    }
}

fn immediate_response(prefix: &PrefixState, key: &str) -> Response {
    match prefix.get(key) {
        Some(entry) => value_response(&entry.value, entry.event_id),
        None => error_response(StatusCode::NOT_FOUND, "not_found", "key not in cache"),
    }
}

async fn long_poll(prefix: Arc<PrefixState>, key: String, since: i64, wait: Duration) -> Response {
    let deadline = Instant::now() + wait;

    loop {
        let cur = prefix.get(&key);
        let cur_id = cur.as_ref().map(|e| e.event_id).unwrap_or(0);
        if cur_id > since {
            // Value (or absence) has advanced past `since`. Return now.
            return match cur {
                Some(e) => value_response(&e.value, e.event_id),
                None => {
                    // Key was deleted; surface as 404 with the prefix's
                    // current cursor so the caller can advance their `since`.
                    error_response_with_index(
                        StatusCode::NOT_FOUND,
                        "not_found",
                        "key not in cache",
                        prefix.latest_event_id.load(Ordering::Relaxed),
                    )
                }
            };
        }

        let now = Instant::now();
        if now >= deadline {
            // Timed out without seeing this key change.
            return match cur {
                Some(e) => value_response(&e.value, e.event_id),
                None => error_response_with_index(
                    StatusCode::NOT_FOUND,
                    "not_found",
                    "key not in cache",
                    prefix.latest_event_id.load(Ordering::Relaxed),
                ),
            };
        }
        let remaining = deadline - now;

        let mut rx = prefix.notify.subscribe();
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(_) => {
                // Got a notification (Ok) or the broadcast lagged (Err);
                // either way re-check.
                continue;
            }
            Err(_) => {
                // Timed out — fall through and return the current state.
                continue;
            }
        }
    }
}

fn parse_wait(s: &str) -> Option<Duration> {
    let d = humantime::parse_duration(s).ok()?;
    Some(d.min(Duration::from_secs(5 * 60)))
}

#[derive(Deserialize)]
struct ListQuery {
    prefix: String,
}

async fn list_handler(
    State(state): State<Arc<AgentState>>,
    Query(q): Query<ListQuery>,
) -> Response {
    let Some(prefix_state) = state.find_prefix_for_listing(&q.prefix) else {
        return error_response(
            StatusCode::NOT_FOUND,
            "prefix_not_watched",
            &format!("requested prefix {:?} is not under any watched prefix", q.prefix),
        );
    };

    let items: Vec<_> = prefix_state
        .list()
        .into_iter()
        .filter(|(k, _)| k.starts_with(&q.prefix))
        .map(|(k, e)| {
            json!({
                "key": k,
                "value": e.value,
                "event_id": e.event_id,
            })
        })
        .collect();

    let body = json!({
        "prefix": q.prefix,
        "latest_event_id": prefix_state.latest_event_id.load(Ordering::Relaxed),
        "items": items,
    });
    let mut headers = HeaderMap::new();
    headers.insert(
        "X-Ferra-Index",
        HeaderValue::from_str(
            &prefix_state
                .latest_event_id
                .load(Ordering::Relaxed)
                .to_string(),
        )
        .unwrap(),
    );
    (StatusCode::OK, headers, Json(body)).into_response()
}

fn value_response(value: &Value, event_id: i64) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        "X-Ferra-Index",
        HeaderValue::from_str(&event_id.to_string()).unwrap(),
    );
    (StatusCode::OK, headers, Json(value.clone())).into_response()
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({
            "error": code,
            "message": message,
        })),
    )
        .into_response()
}

fn error_response_with_index(
    status: StatusCode,
    code: &str,
    message: &str,
    event_id: i64,
) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        "X-Ferra-Index",
        HeaderValue::from_str(&event_id.to_string()).unwrap(),
    );
    (
        status,
        headers,
        Json(json!({
            "error": code,
            "message": message,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wait_caps_at_5min() {
        assert_eq!(parse_wait("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_wait("10m"), Some(Duration::from_secs(5 * 60)));
        assert_eq!(parse_wait("garbage"), None);
    }

    #[test]
    fn find_prefix_picks_longest_match() {
        let upstream = UpstreamClient::new("http://x".into());
        let s = AgentState {
            upstream,
            prefixes: vec![
                Arc::new(PrefixState::new("services/".into())),
                Arc::new(PrefixState::new("services/payment/".into())),
            ],
        };
        let p = s.find_prefix("services/payment/timeout_ms").unwrap();
        assert_eq!(p.prefix, "services/payment/");
        let p = s.find_prefix("services/auth/jwt").unwrap();
        assert_eq!(p.prefix, "services/");
        assert!(s.find_prefix("flags/x").is_none());
    }
}
