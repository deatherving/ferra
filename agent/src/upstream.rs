use anyhow::{anyhow, Context, Result};
use reqwest::Url;
use serde::Deserialize;
use serde_json::Value;

/// HTTP client to ferra-server. No global request timeout — the watch
/// connection is long-lived; per-request timeouts come from the
/// `tokio::time::timeout` wrappers in the watch loop.
#[derive(Clone)]
pub struct UpstreamClient {
    http: reqwest::Client,
    base: String,
}

#[derive(Debug, Deserialize)]
pub struct Snapshot {
    #[allow(dead_code)]
    pub prefix: String,
    pub latest_event_id: i64,
    pub items: Vec<SnapshotItem>,
}

#[derive(Debug, Deserialize)]
pub struct SnapshotItem {
    pub key: String,
    pub value: Value,
    pub event_id: i64,
}

#[derive(Debug, Deserialize)]
pub struct GetResponse {
    #[allow(dead_code)]
    pub key: String,
    pub value: Value,
    pub event_id: i64,
}

impl UpstreamClient {
    pub fn new(base_url: String) -> Self {
        let http = reqwest::Client::builder()
            .build()
            .expect("build reqwest client");
        Self {
            http,
            base: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// `GET /v1/kv?prefix=...`
    pub async fn snapshot(&self, prefix: &str) -> Result<Snapshot> {
        let url = Url::parse_with_params(&format!("{}/v1/kv", self.base), &[("prefix", prefix)])
            .context("build snapshot url")?;
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .context("snapshot request")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("snapshot failed: HTTP {status}: {body}"));
        }
        resp.json().await.context("decode snapshot response")
    }

    /// `GET /v1/kv/{key}` — returns `None` on 404 (key was deleted between
    /// the watch event and our follow-up GET).
    pub async fn get_key(&self, key: &str) -> Result<Option<GetResponse>> {
        let url = format!("{}/v1/kv/{}", self.base, encode_key_path(key));
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .context("get_key request")?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("get_key failed: HTTP {status}: {body}"));
        }
        Ok(Some(resp.json().await.context("decode get response")?))
    }

    /// `GET /v1/watch?prefix=...&since=...` — returns the open response;
    /// caller streams the body.
    pub async fn open_watch(&self, prefix: &str, since: i64) -> Result<reqwest::Response> {
        let url = Url::parse_with_params(
            &format!("{}/v1/watch", self.base),
            &[("prefix", prefix), ("since", &since.to_string())],
        )
        .context("build watch url")?;
        let resp = self
            .http
            .get(url)
            .header("Accept", "text/event-stream")
            .send()
            .await
            .context("watch request")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("watch open failed: HTTP {status}: {body}"));
        }
        Ok(resp)
    }
}

/// Percent-encode a key for use as URL path segments. Preserves `/` so a key
/// like `services/payment/timeout_ms` becomes part of the path, not a single
/// segment.
fn encode_key_path(key: &str) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(key.len());
    for b in key.bytes() {
        let safe = matches!(b,
            b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z'
            | b'-' | b'.' | b'_' | b'~' | b'/'
        );
        if safe {
            s.push(b as char);
        } else {
            let _ = write!(s, "%{:02X}", b);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::encode_key_path;

    #[test]
    fn preserves_slashes() {
        assert_eq!(encode_key_path("a/b/c"), "a/b/c");
    }

    #[test]
    fn percent_encodes_unsafe() {
        assert_eq!(encode_key_path("a b"), "a%20b");
        assert_eq!(encode_key_path("?#&"), "%3F%23%26");
    }
}
