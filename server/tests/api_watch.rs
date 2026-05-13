mod common;

use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use reqwest::Method;
use serde_json::{json, Value};
use serial_test::serial;
use tokio::time::timeout;

#[derive(Debug, Default, Clone)]
struct SseEvent {
    event: Option<String>,
    id: Option<String>,
    data: String,
}

/// Reads chunks from the response body and yields parsed SSE events as they
/// complete. Stops once `wanted` events have been collected or `deadline` has
/// elapsed.
async fn collect_sse(resp: reqwest::Response, wanted: usize, deadline: Duration) -> Vec<SseEvent> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut out: Vec<SseEvent> = Vec::new();

    let fut = async {
        while let Some(chunk) = stream.next().await {
            let chunk: Bytes = chunk.unwrap();
            buf.extend_from_slice(&chunk);
            loop {
                let Some((end, sep)) = find_separator(&buf) else {
                    break;
                };
                let raw_bytes: Vec<u8> = buf.drain(..end + sep).collect();
                let raw = String::from_utf8_lossy(&raw_bytes[..end]);
                if let Some(ev) = parse_event(&raw) {
                    out.push(ev);
                    if out.len() >= wanted {
                        return;
                    }
                }
            }
        }
    };
    let _ = timeout(deadline, fut).await;
    out
}

fn find_separator(buf: &[u8]) -> Option<(usize, usize)> {
    for i in 0..buf.len() {
        if buf[i..].starts_with(b"\r\n\r\n") {
            return Some((i, 4));
        }
        if buf[i..].starts_with(b"\n\n") {
            return Some((i, 2));
        }
    }
    None
}

fn parse_event(raw: &str) -> Option<SseEvent> {
    let mut ev = SseEvent::default();
    let mut had = false;
    for line in raw.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        had = true;
        match field {
            "event" => ev.event = Some(value.into()),
            "id" => ev.id = Some(value.into()),
            "data" => {
                if !ev.data.is_empty() {
                    ev.data.push('\n');
                }
                ev.data.push_str(value);
            }
            _ => {}
        }
    }
    if !had {
        return None;
    }
    Some(ev)
}

async fn put(s: &common::TestServer, key: &str, value: Value) {
    s.req(Method::PUT, &format!("/v1/kv/{key}"))
        .json(&json!({ "value": value }))
        .send()
        .await
        .unwrap();
}

#[tokio::test]
#[serial]
async fn watch_replays_history_with_since_zero() {
    let s = common::start().await;
    put(&s, "services/payment/a", json!(1)).await;
    put(&s, "services/payment/b", json!(2)).await;
    put(&s, "services/ride/c", json!(3)).await; // outside prefix

    let resp = s
        .req(Method::GET, "/v1/watch?since=0&prefix=services/payment/")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let evs = collect_sse(resp, 2, Duration::from_secs(3)).await;
    assert_eq!(evs.len(), 2);
    for e in &evs {
        assert_eq!(e.event.as_deref(), Some("kv_changed"));
        assert!(e.data.contains("services/payment/"));
    }
    assert_eq!(evs[0].id.as_deref(), Some("1"));
    assert_eq!(evs[1].id.as_deref(), Some("2"));
}

#[tokio::test]
#[serial]
async fn watch_streams_live_events_after_catchup() {
    // Long heartbeat so it can't race the live events we're producing.
    let opts = common::StartOptions {
        max_value_bytes: 1024,
        heartbeat: Duration::from_secs(30),
    };
    let s = common::start_with(opts).await;
    let s2_addr = s.addr;

    let resp = s
        .req(Method::GET, "/v1/watch?since=0&prefix=svc/")
        .send()
        .await
        .unwrap();

    // Producer: write keys after the watcher is connected.
    tokio::spawn(async move {
        let c = reqwest::Client::new();
        tokio::time::sleep(Duration::from_millis(150)).await;
        for i in 0..2u32 {
            let _ = c
                .put(format!("http://{s2_addr}/v1/kv/svc/k{i}"))
                .json(&json!({ "value": i }))
                .send()
                .await;
        }
    });

    let evs = collect_sse(resp, 2, Duration::from_secs(3)).await;
    assert_eq!(evs.len(), 2);
    assert_eq!(evs[0].event.as_deref(), Some("kv_changed"));
    let payload: Value = serde_json::from_str(&evs[0].data).unwrap();
    assert_eq!(payload["operation"], "set");
    assert!(payload["key"].as_str().unwrap().starts_with("svc/"));
}

#[tokio::test]
#[serial]
async fn watch_emits_heartbeat_when_idle() {
    let opts = common::StartOptions {
        max_value_bytes: 1024,
        heartbeat: Duration::from_millis(120),
    };
    let s = common::start_with(opts).await;

    let resp = s
        .req(Method::GET, "/v1/watch?since=0&prefix=zzz/")
        .send()
        .await
        .unwrap();

    let evs = collect_sse(resp, 1, Duration::from_secs(2)).await;
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].event.as_deref(), Some("heartbeat"));
    assert_eq!(evs[0].data, "{}");
}

#[tokio::test]
#[serial]
async fn watch_filters_by_prefix() {
    let s = common::start().await;
    put(&s, "outside/a", json!(1)).await;
    put(&s, "inside/a", json!(2)).await;
    put(&s, "inside/b", json!(3)).await;

    let resp = s
        .req(Method::GET, "/v1/watch?since=0&prefix=inside/")
        .send()
        .await
        .unwrap();

    let evs = collect_sse(resp, 2, Duration::from_secs(3)).await;
    assert_eq!(evs.len(), 2);
    for e in &evs {
        let p: Value = serde_json::from_str(&e.data).unwrap();
        assert!(p["key"].as_str().unwrap().starts_with("inside/"));
    }
}
