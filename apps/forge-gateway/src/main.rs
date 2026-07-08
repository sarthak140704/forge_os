//! # forge-gateway — webhook bridge for messaging platforms
//!
//! A tiny axum server that:
//!   * Receives inbound webhooks from Slack (and a generic `POST /webhook`
//!     endpoint that any tool can use).
//!   * Forwards the extracted user text to a running Forge OS API server's
//!     OpenAI-compat shim (`POST /v1/chat/completions`).
//!   * Replies to the caller with the assistant text (generic path) or
//!     posts to the caller's `response_url` (Slack path).
//!
//! ## Env
//!
//!   FORGE_URL             — Forge API base URL (default http://127.0.0.1:7823)
//!   FORGE_TOKEN           — Forge bearer token
//!   GATEWAY_BIND          — where to listen (default 127.0.0.1:7824)
//!   GATEWAY_SHARED_SECRET — bearer expected on POST /webhook (empty = open)
//!   SLACK_SIGNING_SECRET  — Slack's signing secret. Empty = skip verify.
//!                          Real deployments MUST set this.
//!
//! ## Endpoints
//!
//!   GET  /health              — always 200 `{"status":"ok"}`
//!   POST /webhook             — generic. Body `{"message":"...", "system":"..."?}`.
//!                                Bearer `GATEWAY_SHARED_SECRET` required.
//!                                Returns `{"reply":"...", "mission_id":"..."}`.
//!   POST /slack/commands      — Slack slash-command payload
//!                                (`application/x-www-form-urlencoded`).
//!                                Verifies X-Slack-Signature.
//!                                Replies immediately with ephemeral ack;
//!                                real response is POSTed to response_url.
//!
//! This is deliberately small — the value of the gateway is *routing*, not
//! deep integration. Adding Discord/Telegram is a copy of the Slack handler
//! with their platform's signature scheme.

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::net::SocketAddr;
use std::sync::Arc;

type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// CLI + config
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "forge-gateway", version, about = "Webhook bridge for Forge OS.")]
struct Cli {
    /// Bind address for the gateway itself.
    #[arg(long, env = "GATEWAY_BIND", default_value = "127.0.0.1:7824")]
    bind: SocketAddr,

    /// Forge API base URL.
    #[arg(long, env = "FORGE_URL", default_value = "http://127.0.0.1:7823")]
    forge_url: String,

    /// Forge API bearer token.
    #[arg(long, env = "FORGE_TOKEN", default_value = "")]
    forge_token: String,

    /// Bearer token required on POST /webhook. Empty disables the check.
    #[arg(long, env = "GATEWAY_SHARED_SECRET", default_value = "")]
    shared_secret: String,

    /// Slack signing secret. Empty skips verification (dev only).
    #[arg(long, env = "SLACK_SIGNING_SECRET", default_value = "")]
    slack_signing_secret: String,

    /// Request timeout in seconds for the Forge upstream.
    #[arg(long, default_value_t = 300)]
    upstream_timeout: u64,
}

#[derive(Clone)]
pub struct AppState {
    http: reqwest::Client,
    forge_url: Arc<String>,
    forge_token: Arc<String>,
    shared_secret: Arc<String>,
    slack_signing_secret: Arc<String>,
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("forge_gateway=info,warn")),
        )
        .init();

    let cli = Cli::parse();

    let state = AppState {
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(cli.upstream_timeout))
            .build()
            .context("building reqwest client")?,
        forge_url: Arc::new(cli.forge_url.trim_end_matches('/').to_string()),
        forge_token: Arc::new(cli.forge_token),
        shared_secret: Arc::new(cli.shared_secret),
        slack_signing_secret: Arc::new(cli.slack_signing_secret),
    };

    if state.forge_token.is_empty() {
        tracing::warn!("FORGE_TOKEN is empty — Forge upstream must have auth disabled");
    }
    if state.slack_signing_secret.is_empty() {
        tracing::warn!("SLACK_SIGNING_SECRET is empty — /slack/commands runs UNVERIFIED (dev only)");
    }
    if state.shared_secret.is_empty() {
        tracing::warn!("GATEWAY_SHARED_SECRET is empty — /webhook is OPEN (dev only)");
    }

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(cli.bind).await
        .with_context(|| format!("binding {}", cli.bind))?;
    tracing::info!(%cli.bind, "forge-gateway listening");
    axum::serve(listener, app).await.context("axum serve")?;
    Ok(())
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health",         get(health))
        .route("/webhook",        post(generic_webhook))
        .route("/slack/commands", post(slack_slash))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// /health
// ---------------------------------------------------------------------------

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status":"ok", "service":"forge-gateway"}))
}

// ---------------------------------------------------------------------------
// /webhook — generic
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GenericIn {
    message: String,
    #[serde(default)]
    system:  Option<String>,
    /// Optional pass-through model hint (ignored server-side).
    #[serde(default)]
    model:   Option<String>,
}

#[derive(Debug, Serialize)]
struct GenericOut {
    reply:      String,
    mission_id: String,
}

async fn generic_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<GenericIn>,
) -> Result<Json<GenericOut>, ApiErr> {
    if !state.shared_secret.is_empty() {
        let hv = headers.get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .unwrap_or("");
        if hv != state.shared_secret.as_str() {
            return Err(ApiErr::unauth("bad or missing bearer"));
        }
    }
    if body.message.trim().is_empty() {
        return Err(ApiErr::bad("message must be non-empty"));
    }

    let (reply, mission_id) = call_forge_chat(&state, &body.message, body.system.as_deref(), body.model.as_deref())
        .await
        .map_err(|e| ApiErr::upstream(&e.to_string()))?;
    Ok(Json(GenericOut { reply, mission_id }))
}

// ---------------------------------------------------------------------------
// /slack/commands — slash-command receiver
// ---------------------------------------------------------------------------
//
// Slack POSTs `application/x-www-form-urlencoded` with fields:
//   command, text, response_url, user_id, user_name, channel_id, ...
// We ack immediately (Slack requires <3s) and post the real response back
// to response_url in a background task.

#[derive(Debug, Deserialize)]
struct SlackSlash {
    #[serde(default)] token:        String,
    #[serde(default)] command:      String,
    #[serde(default)] text:         String,
    #[serde(default)] response_url: String,
    #[serde(default)] user_name:    String,
    #[serde(default)] channel_id:   String,
}

async fn slack_slash(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    // 1) Signature verification.
    if !state.slack_signing_secret.is_empty() {
        if let Err(e) = verify_slack_signature(&state.slack_signing_secret, &headers, &body) {
            tracing::warn!(err = %e, "Slack signature verification failed");
            return (StatusCode::UNAUTHORIZED, "invalid signature").into_response();
        }
    }
    // 2) Parse the form body.
    let parsed: SlackSlash = match serde_urlencoded::from_str(&body) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(err = %e, "Slack payload parse failed");
            return (StatusCode::BAD_REQUEST, "invalid form body").into_response();
        }
    };
    let _ = parsed.token; // Slack legacy token — ignore; the signature is authoritative
    tracing::info!(user = %parsed.user_name, cmd = %parsed.command, "slack slash received");

    // 3) Kick off the actual work in the background.
    let state_bg = state.clone();
    let response_url = parsed.response_url.clone();
    let text = parsed.text.clone();
    let user = parsed.user_name.clone();
    let channel = parsed.channel_id.clone();
    tokio::spawn(async move {
        let system = Some(format!(
            "You are Forge OS answering a Slack request from @{user} in channel {channel}."
        ));
        match call_forge_chat(&state_bg, &text, system.as_deref(), None).await {
            Ok((reply, _mid)) => {
                if !response_url.is_empty() {
                    let payload = serde_json::json!({"text": reply, "response_type": "in_channel"});
                    if let Err(e) = state_bg.http.post(&response_url).json(&payload).send().await {
                        tracing::warn!(err = %e, "posting to Slack response_url failed");
                    }
                }
            }
            Err(e) => {
                let payload = serde_json::json!({
                    "text": format!(":warning: Forge upstream error: {e}"),
                    "response_type": "ephemeral"
                });
                let _ = state_bg.http.post(&response_url).json(&payload).send().await;
            }
        }
    });

    // 4) Immediate ack — user sees "Working on it..." until the tokio::spawn
    //    finishes and posts the real answer.
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "response_type": "ephemeral",
            "text": ":gear: Forge is working on your request — I'll reply here shortly."
        })),
    ).into_response()
}

fn verify_slack_signature(secret: &str, headers: &HeaderMap, body: &str) -> Result<()> {
    let ts = headers.get("X-Slack-Request-Timestamp")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| anyhow::anyhow!("missing X-Slack-Request-Timestamp"))?;
    let sig = headers.get("X-Slack-Signature")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| anyhow::anyhow!("missing X-Slack-Signature"))?;

    // 5 min replay window.
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let ts_num: i64 = ts.parse().context("timestamp not an integer")?;
    if (now - ts_num).abs() > 60 * 5 {
        anyhow::bail!("timestamp outside replay window");
    }

    let basestring = format!("v0:{ts}:{body}");
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .context("bad HMAC key")?;
    mac.update(basestring.as_bytes());
    let expected = format!("v0={}", hex::encode(mac.finalize().into_bytes()));

    // Constant-time comparison.
    if expected.len() != sig.len() {
        anyhow::bail!("signature length mismatch");
    }
    let mut diff = 0u8;
    for (a, b) in expected.as_bytes().iter().zip(sig.as_bytes()) {
        diff |= a ^ b;
    }
    if diff != 0 {
        anyhow::bail!("signature mismatch");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Upstream Forge call
// ---------------------------------------------------------------------------

async fn call_forge_chat(
    state:  &AppState,
    text:   &str,
    system: Option<&str>,
    model:  Option<&str>,
) -> Result<(String, String)> {
    let mut messages: Vec<serde_json::Value> = Vec::with_capacity(2);
    if let Some(sys) = system {
        messages.push(serde_json::json!({"role":"system", "content": sys}));
    }
    messages.push(serde_json::json!({"role":"user", "content": text}));

    let model = model.unwrap_or("forge-mission");
    let url = format!("{}/v1/chat/completions", state.forge_url);
    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });

    let mut req = state.http.post(&url).json(&body);
    if !state.forge_token.is_empty() {
        req = req.bearer_auth(state.forge_token.as_str());
    }
    let resp = req.send().await.with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let text_body = resp.text().await.context("reading upstream body")?;
    if !status.is_success() {
        anyhow::bail!("Forge upstream {status}: {}",
            text_body.chars().take(300).collect::<String>());
    }
    let json: serde_json::Value = serde_json::from_str(&text_body)
        .context("parsing Forge upstream JSON")?;
    let content = json.pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mission_id = json.get("forge_mission_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok((content, mission_id))
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

pub struct ApiErr {
    code: StatusCode,
    msg:  String,
}
impl ApiErr {
    fn unauth(m: &str)   -> Self { Self { code: StatusCode::UNAUTHORIZED,        msg: m.into() } }
    fn bad(m: &str)      -> Self { Self { code: StatusCode::BAD_REQUEST,         msg: m.into() } }
    fn upstream(m: &str) -> Self { Self { code: StatusCode::BAD_GATEWAY,         msg: m.into() } }
}
impl IntoResponse for ApiErr {
    fn into_response(self) -> axum::response::Response {
        (self.code, Json(serde_json::json!({"error": self.msg}))).into_response()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn state_for_tests(shared: &str, slack: &str) -> AppState {
        AppState {
            http: reqwest::Client::new(),
            forge_url:            Arc::new("http://127.0.0.1:0".into()),
            forge_token:          Arc::new(String::new()),
            shared_secret:        Arc::new(shared.into()),
            slack_signing_secret: Arc::new(slack.into()),
        }
    }

    #[test]
    fn slack_signature_verifies_when_correct() {
        let secret = "8f742231b10e8888abcd99yyyzzz85a5";
        let body = "token=xyz&text=hi";
        let ts = time::OffsetDateTime::now_utc().unix_timestamp().to_string();
        let base = format!("v0:{ts}:{body}");
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(base.as_bytes());
        let sig = format!("v0={}", hex::encode(mac.finalize().into_bytes()));

        let mut h = HeaderMap::new();
        h.insert("X-Slack-Request-Timestamp", ts.parse().unwrap());
        h.insert("X-Slack-Signature",         sig.parse().unwrap());
        assert!(verify_slack_signature(secret, &h, body).is_ok());
    }

    #[test]
    fn slack_signature_rejects_tamper() {
        let secret = "8f742231b10e8888abcd99yyyzzz85a5";
        let body = "token=xyz&text=hi";
        let ts = time::OffsetDateTime::now_utc().unix_timestamp().to_string();
        let base = format!("v0:{ts}:{body}");
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(base.as_bytes());
        let sig = format!("v0={}", hex::encode(mac.finalize().into_bytes()));

        // Tamper: change the body but reuse the signature.
        let mut h = HeaderMap::new();
        h.insert("X-Slack-Request-Timestamp", ts.parse().unwrap());
        h.insert("X-Slack-Signature",         sig.parse().unwrap());
        assert!(verify_slack_signature(secret, &h, "token=xyz&text=evil").is_err());
    }

    #[test]
    fn slack_signature_rejects_stale_timestamp() {
        let secret = "8f742231b10e8888abcd99yyyzzz85a5";
        let body = "token=xyz&text=hi";
        let ts = (time::OffsetDateTime::now_utc().unix_timestamp() - 3600).to_string();
        let base = format!("v0:{ts}:{body}");
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(base.as_bytes());
        let sig = format!("v0={}", hex::encode(mac.finalize().into_bytes()));

        let mut h = HeaderMap::new();
        h.insert("X-Slack-Request-Timestamp", ts.parse().unwrap());
        h.insert("X-Slack-Signature",         sig.parse().unwrap());
        assert!(verify_slack_signature(secret, &h, body).is_err());
    }

    #[test]
    fn build_router_smoke() {
        let _r = build_router(state_for_tests("", ""));
    }
}
