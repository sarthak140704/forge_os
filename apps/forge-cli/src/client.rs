//! Thin HTTP client wrapping the Forge API server.
//!
//! Everything the CLI subcommands need is exposed as a method that returns
//! `anyhow::Result<T>` — errors bubble up with context strings for a
//! human-readable exit.

use anyhow::{anyhow, Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::time::Duration;

/// A minimal client around `reqwest::Client` that:
///   * injects the bearer token on every request that needs it.
///   * normalises non-2xx into `anyhow::Error` with the server's error body.
#[derive(Clone)]
pub struct ApiClient {
    base:  String,
    token: String,
    http:  reqwest::Client,
}

impl ApiClient {
    pub fn new(base: &str, token: &str, timeout_secs: u64) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .expect("reqwest client build");
        Self {
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http,
        }
    }

    pub fn base(&self) -> &str { &self.base }
    pub fn token(&self) -> &str { &self.token }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .http
            .get(self.url(path))
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        parse_json(resp, path).await
    }

    pub async fn post_json<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let resp = self
            .http
            .post(self.url(path))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        parse_json(resp, path).await
    }

    /// POST that doesn't care about the body (used for cancel / extend).
    pub async fn post_empty<B: Serialize>(&self, path: &str, body: &B) -> Result<()> {
        let resp = self
            .http
            .post(self.url(path))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        let s = resp.status();
        if !s.is_success() {
            let msg = resp.text().await.unwrap_or_default();
            return Err(anyhow!("POST {path}: {s} — {msg}"));
        }
        Ok(())
    }

    /// Health probe — never sends the bearer, returns Ok even on 401.
    pub async fn health(&self) -> Result<u16> {
        let resp = self
            .http
            .get(self.url("/health"))
            .send()
            .await
            .with_context(|| "GET /health")?;
        Ok(resp.status().as_u16())
    }
}

async fn parse_json<T: DeserializeOwned>(resp: reqwest::Response, path: &str) -> Result<T> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("{path}: {status} — {body}"));
    }
    let text = resp
        .text()
        .await
        .with_context(|| format!("reading response body of {path}"))?;
    serde_json::from_str::<T>(&text)
        .with_context(|| format!("decoding response of {path}: {text}"))
}
