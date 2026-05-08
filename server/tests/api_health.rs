mod common;

use serial_test::serial;

#[tokio::test]
#[serial]
async fn healthz_is_open() {
    let s = common::start().await;
    let resp = s.client.get(s.url("/healthz")).send().await.unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
#[serial]
async fn readyz_is_open_and_checks_db() {
    let s = common::start().await;
    let resp = s.client.get(s.url("/readyz")).send().await.unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
#[serial]
async fn readyz_reports_unavailable_when_pool_closed() {
    let s = common::start().await;
    s.pool.close().await;
    let resp = s.client.get(s.url("/readyz")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 503);
}
