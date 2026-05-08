mod common;

use reqwest::Method;
use serde_json::{json, Value};
use serial_test::serial;

async fn put(s: &common::TestServer, key: &str, value: Value) -> reqwest::Response {
    s.req(Method::PUT, &format!("/v1/kv/{key}"))
        .json(&json!({ "value": value }))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
#[serial]
async fn put_then_get_round_trip() {
    let s = common::start().await;
    let r = put(&s, "services/payment/timeout_ms", json!(3000)).await;
    assert!(r.status().is_success());
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["operation"], "set");
    assert_eq!(body["key"], "services/payment/timeout_ms");
    let event_id = body["event_id"].as_i64().unwrap();
    assert!(event_id > 0);

    let g: Value = s
        .req(Method::GET, "/v1/kv/services/payment/timeout_ms")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(g["key"], "services/payment/timeout_ms");
    assert_eq!(g["value"], json!(3000));
    assert_eq!(g["event_id"], event_id);
    assert!(g["updated_at"].as_str().unwrap().len() > 0);
}

#[tokio::test]
#[serial]
async fn put_overwrites_value_and_advances_event_id() {
    let s = common::start().await;
    let _ = put(&s, "k", json!("v1")).await;
    let r2 = put(&s, "k", json!("v2")).await;
    let id2 = r2.json::<Value>().await.unwrap()["event_id"]
        .as_i64()
        .unwrap();
    let g: Value = s
        .req(Method::GET, "/v1/kv/k")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(g["value"], json!("v2"));
    assert_eq!(g["event_id"], id2);
}

#[tokio::test]
#[serial]
async fn get_missing_returns_404() {
    let s = common::start().await;
    let r = s
        .req(Method::GET, "/v1/kv/missing/key")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
}

#[tokio::test]
#[serial]
async fn delete_missing_returns_404() {
    let s = common::start().await;
    let r = s.req(Method::DELETE, "/v1/kv/none").send().await.unwrap();
    assert_eq!(r.status().as_u16(), 404);
}

#[tokio::test]
#[serial]
async fn delete_existing_removes_and_returns_event_id() {
    let s = common::start().await;
    let _ = put(&s, "doomed", json!(1)).await;
    let r = s
        .req(Method::DELETE, "/v1/kv/doomed")
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["operation"], "delete");

    let r2 = s
        .req(Method::GET, "/v1/kv/doomed")
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status().as_u16(), 404);
}

#[tokio::test]
#[serial]
async fn put_rejects_oversized_value() {
    let opts = common::StartOptions {
        max_value_bytes: 32,
        ..Default::default()
    };
    let s = common::start_with(opts).await;
    let big = "x".repeat(64);
    let r = s
        .req(Method::PUT, "/v1/kv/big")
        .json(&json!({ "value": big }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 413);
}

#[tokio::test]
#[serial]
async fn put_rejects_invalid_json_body() {
    let s = common::start().await;
    let r = s
        .req(Method::PUT, "/v1/kv/k")
        .header("Content-Type", "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert!(r.status().is_client_error());
}

#[tokio::test]
#[serial]
async fn list_with_prefix_filters_and_orders() {
    let s = common::start().await;
    put(&s, "services/payment/a", json!(1)).await;
    put(&s, "services/payment/b", json!(2)).await;
    put(&s, "services/ride/c", json!(3)).await;

    let body: Value = s
        .req(Method::GET, "/v1/kv?prefix=services/payment/")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["prefix"], "services/payment/");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["key"], "services/payment/a");
    assert_eq!(items[1]["key"], "services/payment/b");
    assert!(body["latest_event_id"].as_i64().unwrap() >= 3);
}

#[tokio::test]
#[serial]
async fn list_empty_prefix_returns_all_keys() {
    let s = common::start().await;
    put(&s, "a", json!(1)).await;
    put(&s, "b", json!(2)).await;
    let body: Value = s
        .req(Method::GET, "/v1/kv?prefix=")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
}

#[tokio::test]
#[serial]
async fn list_prefix_too_long_is_400() {
    let s = common::start().await;
    let prefix = "a".repeat(1100);
    let r = s
        .req(Method::GET, &format!("/v1/kv?prefix={prefix}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
}

#[tokio::test]
#[serial]
async fn like_metacharacters_in_prefix_are_treated_literally() {
    let s = common::start().await;
    // Real key
    put(&s, "ops/foo", json!(1)).await;
    // The prefix uses LIKE wildcards as literal chars, not metacharacters.
    let body: Value = s
        .req(Method::GET, "/v1/kv?prefix=ops/%25/")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
#[serial]
async fn key_validation_via_url_rejects_oversized() {
    let s = common::start().await;
    let key = "a/".repeat(600); // 1200 chars
    let r = s
        .req(Method::GET, &format!("/v1/kv/{key}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
}

#[tokio::test]
#[serial]
async fn put_value_can_be_complex_json() {
    let s = common::start().await;
    let v = json!({"a": 1, "b": [true, null, "x"]});
    let _ = put(&s, "complex", v.clone()).await;
    let g: Value = s
        .req(Method::GET, "/v1/kv/complex")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(g["value"], v);
}
