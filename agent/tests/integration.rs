//! End-to-end test for ferra-agent against a hand-rolled fake ferra-server.
//!
//! Each test spins up a small axum HTTP server that mimics the subset of the
//! ferra-server protocol the agent uses (snapshot, single-key GET, SSE
//! watch), starts a real ferra-agent against it, and asserts on the agent's
//! local HTTP API.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{Event, Sse},
    routing::get,
    Json, Router,
};
use ferra_agent::{api, cache::PrefixState, run, upstream::UpstreamClient, Args};
use futures::{stream, Stream};
use serde::Deserialize;
use serde_json::{json, Value};
use serial_test::serial;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
struct Fake {
    /// `key -> value` source of truth on the fake.
    state: Arc<Mutex<std::collections::HashMap<String, Value>>>,
    /// Event id high-water mark — bumped on every set/delete.
    latest: Arc<AtomicI64>,
    /// SSE events the watch endpoint should emit on its first connection.
    watch_events: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn new() -> Self {
        Self::default()
    }

    async fn put(&self, key: &str, value: Value) {
        self.state.lock().await.insert(key.to_string(), value);
        self.latest.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Deserialize)]
struct ListQ {
    prefix: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct WatchQ {
    #[serde(default)]
    since: i64,
    prefix: Option<String>,
}

async fn list_handler(State(f): State<Fake>, Query(q): Query<ListQ>) -> Json<Value> {
    let map = f.state.lock().await;
    let items: Vec<_> = map
        .iter()
        .filter(|(k, _)| k.starts_with(&q.prefix))
        .map(|(k, v)| {
            json!({"key": k, "value": v.clone(), "event_id": f.latest.load(Ordering::SeqCst)})
        })
        .collect();
    Json(json!({
        "prefix": q.prefix,
        "latest_event_id": f.latest.load(Ordering::SeqCst),
        "items": items,
    }))
}

async fn get_handler(State(f): State<Fake>, Path(key): Path<String>) -> Result<Json<Value>, StatusCode> {
    let map = f.state.lock().await;
    match map.get(&key) {
        Some(v) => Ok(Json(json!({
            "key": key,
            "value": v.clone(),
            "event_id": f.latest.load(Ordering::SeqCst),
            "updated_at": "2026-05-08T00:00:00Z",
        }))),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn watch_handler(
    State(f): State<Fake>,
    Query(_q): Query<WatchQ>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // Drain any pre-staged events, emit them, then end the stream so the
    // agent's watch loop reconnects (and the test gets to control how many
    // events it sees deterministically).
    let lines = f.watch_events.lock().await.drain(..).collect::<Vec<_>>();
    let events = lines
        .into_iter()
        .map(|line| {
            // Each `line` is a pre-formatted "event:X\nid:N\ndata:{...}" payload.
            let mut ev = Event::default();
            for raw in line.split('\n') {
                if let Some(v) = raw.strip_prefix("event:") {
                    ev = ev.event(v.trim().to_string());
                } else if let Some(v) = raw.strip_prefix("id:") {
                    ev = ev.id(v.trim().to_string());
                } else if let Some(v) = raw.strip_prefix("data:") {
                    ev = ev.data(v.trim().to_string());
                }
            }
            Ok::<_, Infallible>(ev)
        })
        .collect::<Vec<_>>();
    Sse::new(stream::iter(events))
}

async fn start_fake() -> (SocketAddr, Fake) {
    let f = Fake::new();
    let app = Router::new()
        .route("/v1/kv", get(list_handler))
        .route("/v1/kv/*key", get(get_handler))
        .route("/v1/watch", get(watch_handler))
        .with_state(f.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, f)
}

async fn start_agent(server_addr: SocketAddr, prefix: &str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener); // release for the agent to rebind

    let args = Args {
        server: format!("http://{server_addr}"),
        prefix: vec![prefix.to_string()],
        listen: addr.to_string(),
        min_backoff: Duration::from_millis(50),
        max_backoff: Duration::from_millis(200),
    };
    tokio::spawn(async move {
        let _ = run(args).await;
    });
    addr
}

async fn wait_for_ready(addr: SocketAddr) {
    let client = reqwest::Client::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if tokio::time::Instant::now() > deadline {
            panic!("agent /readyz never returned 200");
        }
        if let Ok(resp) = client.get(format!("http://{addr}/readyz")).send().await {
            if resp.status() == 200 {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
#[serial]
async fn agent_loads_snapshot_and_serves_cfg() {
    let (server_addr, fake) = start_fake().await;
    fake.put("services/payment/timeout_ms", json!(3000)).await;
    fake.put("services/payment/retry", json!("exp")).await;
    fake.put("services/auth/jwt", json!("not visible")).await;

    let agent_addr = start_agent(server_addr, "services/payment/").await;
    wait_for_ready(agent_addr).await;

    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://{agent_addr}/cfg/services/payment/timeout_ms"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v, json!(3000));

    let resp = client
        .get(format!("http://{agent_addr}/cfg/services/payment/retry"))
        .send()
        .await
        .unwrap();
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v, json!("exp"));

    // Key under a non-watched prefix → 404 with explicit error code.
    let resp = client
        .get(format!("http://{agent_addr}/cfg/services/auth/jwt"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "key_not_in_watched_prefix");

    // Key in watched prefix but missing → 404 not_found.
    let resp = client
        .get(format!("http://{agent_addr}/cfg/services/payment/missing"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
#[serial]
async fn agent_lists_prefix() {
    let (server_addr, fake) = start_fake().await;
    fake.put("services/payment/a", json!(1)).await;
    fake.put("services/payment/b", json!(2)).await;
    fake.put("flags/enabled", json!(true)).await;

    let agent_addr = start_agent(server_addr, "services/payment/").await;
    wait_for_ready(agent_addr).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!(
            "http://{agent_addr}/cfg?prefix=services/payment/"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);

    // Sub-prefix narrowing works.
    let resp = client
        .get(format!(
            "http://{agent_addr}/cfg?prefix=services/payment/a"
        ))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["key"], "services/payment/a");

    // Prefix outside the agent's watched set → 404.
    let resp = client
        .get(format!("http://{agent_addr}/cfg?prefix=flags/"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
#[serial]
async fn agent_handles_kv_changed_event() {
    let (server_addr, fake) = start_fake().await;
    fake.put("p/x", json!("initial")).await;

    let agent_addr = start_agent(server_addr, "p/").await;
    wait_for_ready(agent_addr).await;

    // Confirm the snapshot got "initial" first.
    let client = reqwest::Client::new();
    let v: Value = client
        .get(format!("http://{agent_addr}/cfg/p/x"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v, json!("initial"));

    // Now update the server-side value, *then* stage a kv_changed event.
    // The agent's first watch connection drained an empty queue and will
    // be reconnecting (min_backoff = 50ms); the next connection will pick
    // up the staged event, re-fetch p/x, see "updated", and update the
    // cache.
    fake.put("p/x", json!("updated")).await;
    fake.watch_events.lock().await.push(
        "event:kv_changed\nid:99\ndata:{\"event_id\":99,\"key\":\"p/x\",\"operation\":\"set\"}"
            .to_string(),
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let v: Value = client
            .get(format!("http://{agent_addr}/cfg/p/x"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if v == json!("updated") {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("agent never picked up the kv_changed update; got {v}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
#[serial]
async fn long_poll_returns_immediately_when_value_already_advanced() {
    let (server_addr, fake) = start_fake().await;
    fake.put("p/k", json!("v1")).await;

    let agent_addr = start_agent(server_addr, "p/").await;
    wait_for_ready(agent_addr).await;

    let client = reqwest::Client::new();
    // since=0 < current event_id, so this should return immediately.
    let resp = client
        .get(format!("http://{agent_addr}/cfg/p/k?wait=10s&since=0"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let index_header = resp
        .headers()
        .get("X-Ferra-Index")
        .map(|h| h.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v, json!("v1"));
    assert!(!index_header.is_empty() && index_header != "0");
}

#[tokio::test]
#[serial]
async fn long_poll_times_out_when_no_change() {
    let (server_addr, fake) = start_fake().await;
    fake.put("p/k", json!("v1")).await;

    let agent_addr = start_agent(server_addr, "p/").await;
    wait_for_ready(agent_addr).await;

    let client = reqwest::Client::new();
    // since=999999999 >> current event_id, so this should hit the wait
    // timeout and return the current value.
    let started = tokio::time::Instant::now();
    let resp = client
        .get(format!(
            "http://{agent_addr}/cfg/p/k?wait=400ms&since=999999999"
        ))
        .send()
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v, json!("v1"));
    assert!(elapsed >= Duration::from_millis(300), "elapsed: {:?}", elapsed);
    assert!(elapsed < Duration::from_secs(2), "elapsed: {:?}", elapsed);
}

// Touch the modules so unused-import checks stay quiet.
#[allow(dead_code)]
fn _unused() {
    let _ = api::AgentState {
        upstream: UpstreamClient::new("x".into()),
        prefixes: vec![Arc::new(PrefixState::new("x".into()))],
    };
}
