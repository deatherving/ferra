mod common;

use reqwest::Method;
use serde_json::{json, Value};
use serial_test::serial;

async fn put(s: &common::TestServer, key: &str, value: Value) {
    s.req(Method::PUT, &format!("/v1/kv/{key}"))
        .json(&json!({ "value": value }))
        .send()
        .await
        .unwrap();
}

async fn delete(s: &common::TestServer, key: &str) {
    s.req(Method::DELETE, &format!("/v1/kv/{key}"))
        .send()
        .await
        .unwrap();
}

#[tokio::test]
#[serial]
async fn empty_returns_zeroed_response() {
    let s = common::start().await;
    let body: Value = s
        .req(Method::GET, "/v1/events?since=0")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["from_event_id"], 0);
    assert_eq!(body["to_event_id"], 0);
    assert_eq!(body["events"].as_array().unwrap().len(), 0);
}

#[tokio::test]
#[serial]
async fn returns_set_and_delete_events_in_order() {
    let s = common::start().await;
    put(&s, "a", json!(1)).await;
    put(&s, "b", json!(2)).await;
    delete(&s, "a").await;

    let body: Value = s
        .req(Method::GET, "/v1/events?since=0")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ev = body["events"].as_array().unwrap();
    assert_eq!(ev.len(), 3);
    assert_eq!(ev[0]["operation"], "set");
    assert_eq!(ev[0]["key"], "a");
    assert_eq!(ev[1]["operation"], "set");
    assert_eq!(ev[1]["key"], "b");
    assert_eq!(ev[2]["operation"], "delete");
    assert_eq!(ev[2]["key"], "a");
}

#[tokio::test]
#[serial]
async fn since_filters_to_newer_events() {
    let s = common::start().await;
    put(&s, "a", json!(1)).await;
    put(&s, "b", json!(2)).await;
    put(&s, "c", json!(3)).await;

    let body: Value = s
        .req(Method::GET, "/v1/events?since=1")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ev = body["events"].as_array().unwrap();
    assert_eq!(ev.len(), 2);
    assert_eq!(ev[0]["event_id"], 2);
    assert_eq!(ev[1]["event_id"], 3);
    assert_eq!(body["from_event_id"], 1);
    assert_eq!(body["to_event_id"], 3);
}

#[tokio::test]
#[serial]
async fn prefix_filter_applies() {
    let s = common::start().await;
    put(&s, "services/a", json!(1)).await;
    put(&s, "ops/x", json!(2)).await;
    put(&s, "services/b", json!(3)).await;

    let body: Value = s
        .req(Method::GET, "/v1/events?since=0&prefix=services/")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ev = body["events"].as_array().unwrap();
    assert_eq!(ev.len(), 2);
    for e in ev {
        assert!(e["key"].as_str().unwrap().starts_with("services/"));
    }
}

#[tokio::test]
#[serial]
async fn negative_since_is_400() {
    let s = common::start().await;
    let r = s
        .req(Method::GET, "/v1/events?since=-1")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
}

#[tokio::test]
#[serial]
async fn limit_clamps_to_max() {
    let s = common::start().await;
    for i in 0..3 {
        put(&s, &format!("k{i}"), json!(i)).await;
    }
    // limit=0 gets clamped up to 1.
    let body: Value = s
        .req(Method::GET, "/v1/events?since=0&limit=0")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["events"].as_array().unwrap().len(), 1);

    // limit=999999 gets clamped to 5000 (more than enough for 3).
    let body: Value = s
        .req(Method::GET, "/v1/events?since=0&limit=999999")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["events"].as_array().unwrap().len(), 3);
}
