//! # forge-gateway — webhook bridge for messaging platforms
//!
//! A tiny axum server that:
//!   * Receives inbound webhooks from Slack, Discord, Telegram, and a
//!     generic `POST /webhook` endpoint that any tool can use.
//!   * Forwards the extracted user text to a running Forge OS API server's
//!     OpenAI-compat shim (`POST /v1/chat/completions`).
//!   * Replies to the caller with the assistant text (generic path) or
//!     posts to the caller's `response_url` (Slack path), or edits the
//!     deferred interaction (Discord), or calls sendMessage (Telegram).
//!
//! ## Env
//!
//!   FORGE_URL                        — Forge API base URL (default http://127.0.0.1:7823)
//!   FORGE_TOKEN                      — Forge bearer token
//!   GATEWAY_BIND                     — where to listen (default 127.0.0.1:7824)
//!   GATEWAY_SHARED_SECRET            — bearer expected on POST /webhook (empty = open)
//!   SLACK_SIGNING_SECRET             — Slack's signing secret. Empty = skip verify.
//!                                       Real deployments MUST set this.
//!   DISCORD_APPLICATION_PUBLIC_KEY   — Discord app's public key (hex, 64 chars).
//!                                       Empty disables /discord/interactions.
//!   TELEGRAM_BOT_TOKEN               — Telegram bot token (`123:ABC...`).
//!                                       Empty disables /telegram/webhook.
//!   TELEGRAM_SECRET_TOKEN            — Optional. When set, incoming Telegram
//!                                       requests MUST carry a matching
//!                                       `X-Telegram-Bot-Api-Secret-Token` header.
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
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
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

    /// Discord application public key (hex, 64 chars). Empty disables /discord/interactions.
    #[arg(long, env = "DISCORD_APPLICATION_PUBLIC_KEY", default_value = "")]
    discord_public_key: String,

    /// Telegram bot token. Empty disables /telegram/webhook.
    #[arg(long, env = "TELEGRAM_BOT_TOKEN", default_value = "")]
    telegram_bot_token: String,

    /// Optional Telegram secret token — when set, the incoming request must carry
    /// a matching X-Telegram-Bot-Api-Secret-Token header.
    #[arg(long, env = "TELEGRAM_SECRET_TOKEN", default_value = "")]
    telegram_secret_token: String,

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
    discord_public_key: Arc<String>,
    telegram_bot_token: Arc<String>,
    telegram_secret_token: Arc<String>,
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
        discord_public_key: Arc::new(cli.discord_public_key),
        telegram_bot_token: Arc::new(cli.telegram_bot_token),
        telegram_secret_token: Arc::new(cli.telegram_secret_token),
    };
    if state.discord_public_key.is_empty() {
        tracing::warn!("DISCORD_APPLICATION_PUBLIC_KEY empty — /discord/interactions is DISABLED");
    }
    if state.telegram_bot_token.is_empty() {
        tracing::warn!("TELEGRAM_BOT_TOKEN empty — /telegram/webhook is DISABLED");
    }

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
        .route("/health",               get(health))
        .route("/webhook",              post(generic_webhook))
        .route("/slack/commands",       post(slack_slash))
        .route("/discord/interactions", post(discord_interactions))
        .route("/telegram/webhook",     post(telegram_webhook))
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
// /discord/interactions
// ---------------------------------------------------------------------------
//
// Discord sends every interaction (button clicks, slash commands, ping)
// signed with an ed25519 signature keyed to the application's public key.
// We verify the signature over `timestamp || body`.
//
// type=1 (PING) — respond with type=1 (PONG). Discord uses this to verify
//                  our endpoint at setup time.
// type=2 (APPLICATION_COMMAND) — respond with type=5 (deferred), then edit
//                                 the response via PATCH /webhooks/{app_id}/
//                                 {interaction_token}/messages/@original.

const DISCORD_TYPE_PING:                 u64 = 1;
const DISCORD_TYPE_APPLICATION_COMMAND:  u64 = 2;
const DISCORD_RESPONSE_PONG:             u64 = 1;
const DISCORD_RESPONSE_DEFERRED_MESSAGE: u64 = 5;

async fn discord_interactions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    if state.discord_public_key.is_empty() {
        return (StatusCode::NOT_FOUND, "discord adapter disabled").into_response();
    }
    if let Err(e) = verify_discord_signature(&state.discord_public_key, &headers, &body) {
        tracing::warn!(err = %e, "discord signature verification failed");
        return (StatusCode::UNAUTHORIZED, "invalid signature").into_response();
    }
    let payload: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(err = %e, "discord payload parse failed");
            return (StatusCode::BAD_REQUEST, "invalid json").into_response();
        }
    };
    let itype = payload.get("type").and_then(|v| v.as_u64()).unwrap_or(0);
    if itype == DISCORD_TYPE_PING {
        return Json(serde_json::json!({"type": DISCORD_RESPONSE_PONG})).into_response();
    }
    if itype != DISCORD_TYPE_APPLICATION_COMMAND {
        return (StatusCode::OK, Json(serde_json::json!({
            "type": DISCORD_RESPONSE_DEFERRED_MESSAGE,
        }))).into_response();
    }

    let app_id = payload.get("application_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let token  = payload.get("token").and_then(|v| v.as_str()).unwrap_or("").to_string();
    // Best-effort text extraction from the first string option, or fall back
    // to the command name itself.
    let text = extract_discord_command_text(&payload);
    let user = payload.pointer("/member/user/username")
        .or_else(|| payload.pointer("/user/username"))
        .and_then(|v| v.as_str())
        .unwrap_or("someone")
        .to_string();

    tracing::info!(%user, %app_id, "discord interaction received");

    let state_bg = state.clone();
    tokio::spawn(async move {
        let system = Some(format!(
            "You are Forge OS answering a Discord slash-command from @{user}."
        ));
        let reply = match call_forge_chat(&state_bg, &text, system.as_deref(), None).await {
            Ok((r, _mid)) => r,
            Err(e) => format!(":warning: Forge upstream error: {e}"),
        };
        let url = format!("https://discord.com/api/v10/webhooks/{app_id}/{token}/messages/@original");
        let payload = serde_json::json!({"content": reply});
        if let Err(e) = state_bg.http.patch(&url).json(&payload).send().await {
            tracing::warn!(err = %e, "posting to discord followup failed");
        }
    });

    (StatusCode::OK, Json(serde_json::json!({
        "type": DISCORD_RESPONSE_DEFERRED_MESSAGE,
    }))).into_response()
}

fn extract_discord_command_text(payload: &serde_json::Value) -> String {
    // Interaction data → options: [{ name, type, value }, ...]. Pull the
    // first `type: 3` (STRING) option, else the command name itself.
    if let Some(options) = payload.pointer("/data/options").and_then(|v| v.as_array()) {
        for opt in options {
            let ty = opt.get("type").and_then(|v| v.as_u64()).unwrap_or(0);
            if ty == 3 {
                if let Some(s) = opt.get("value").and_then(|v| v.as_str()) {
                    return s.to_string();
                }
            }
        }
    }
    payload.pointer("/data/name").and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn verify_discord_signature(public_key_hex: &str, headers: &HeaderMap, body: &str) -> Result<()> {
    let sig_hex = headers.get("X-Signature-Ed25519")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| anyhow::anyhow!("missing X-Signature-Ed25519"))?;
    let ts = headers.get("X-Signature-Timestamp")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| anyhow::anyhow!("missing X-Signature-Timestamp"))?;

    let pk_bytes = hex::decode(public_key_hex).context("public key must be hex")?;
    let pk_arr: [u8; 32] = pk_bytes.as_slice().try_into()
        .map_err(|_| anyhow::anyhow!("public key must be 32 bytes"))?;
    let vk = VerifyingKey::from_bytes(&pk_arr).context("invalid ed25519 public key")?;

    let sig_bytes = hex::decode(sig_hex).context("signature must be hex")?;
    let sig_arr: [u8; 64] = sig_bytes.as_slice().try_into()
        .map_err(|_| anyhow::anyhow!("signature must be 64 bytes"))?;
    let sig = Signature::from_bytes(&sig_arr);

    let mut msg = Vec::with_capacity(ts.len() + body.len());
    msg.extend_from_slice(ts.as_bytes());
    msg.extend_from_slice(body.as_bytes());

    vk.verify(&msg, &sig).context("signature mismatch")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// /telegram/webhook
// ---------------------------------------------------------------------------
//
// Telegram POSTs the Bot API `Update` JSON. When we register the webhook
// with setWebhook we can pass a secret token; Telegram then echoes it back
// in the `X-Telegram-Bot-Api-Secret-Token` header.

async fn telegram_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if state.telegram_bot_token.is_empty() {
        return (StatusCode::NOT_FOUND, "telegram adapter disabled").into_response();
    }
    // Optional secret-token check.
    if !state.telegram_secret_token.is_empty() {
        let got = headers.get("X-Telegram-Bot-Api-Secret-Token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if got != state.telegram_secret_token.as_str() {
            return (StatusCode::UNAUTHORIZED, "bad secret token").into_response();
        }
    }

    let chat_id = body.pointer("/message/chat/id").and_then(|v| v.as_i64());
    let text    = body.pointer("/message/text").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let user    = body.pointer("/message/from/username").and_then(|v| v.as_str()).unwrap_or("someone").to_string();
    if chat_id.is_none() || text.trim().is_empty() {
        // Non-message updates (edited_message, channel_post, callback_query...)
        // → ack and drop. We keep this handler dumb-simple.
        return (StatusCode::OK, "ok").into_response();
    }
    let chat_id = chat_id.unwrap();

    tracing::info!(%user, chat_id, "telegram message received");

    let state_bg = state.clone();
    tokio::spawn(async move {
        let system = Some(format!(
            "You are Forge OS answering a Telegram message from @{user}."
        ));
        let reply = match call_forge_chat(&state_bg, &text, system.as_deref(), None).await {
            Ok((r, _mid)) => r,
            Err(e) => format!("⚠️ Forge upstream error: {e}"),
        };
        let url = format!("https://api.telegram.org/bot{}/sendMessage",
            state_bg.telegram_bot_token);
        let payload = serde_json::json!({
            "chat_id": chat_id,
            "text":    reply,
        });
        if let Err(e) = state_bg.http.post(&url).json(&payload).send().await {
            tracing::warn!(err = %e, "posting to telegram sendMessage failed");
        }
    });

    (StatusCode::OK, "ok").into_response()
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
            discord_public_key:   Arc::new(String::new()),
            telegram_bot_token:   Arc::new(String::new()),
            telegram_secret_token:Arc::new(String::new()),
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

    #[test]
    fn discord_signature_verifies_when_correct() {
        use ed25519_dalek::{Signer, SigningKey};
        use rand_core::OsRng;
        let sk = SigningKey::generate(&mut OsRng);
        let pk_hex = hex::encode(sk.verifying_key().to_bytes());
        let body = r#"{"type":1}"#;
        let ts = time::OffsetDateTime::now_utc().unix_timestamp().to_string();
        let sig = sk.sign(format!("{ts}{body}").as_bytes());
        let sig_hex = hex::encode(sig.to_bytes());

        let mut h = HeaderMap::new();
        h.insert("X-Signature-Ed25519",   sig_hex.parse().unwrap());
        h.insert("X-Signature-Timestamp", ts.parse().unwrap());
        assert!(verify_discord_signature(&pk_hex, &h, body).is_ok());
    }

    #[test]
    fn discord_signature_rejects_tampered_body() {
        use ed25519_dalek::{Signer, SigningKey};
        use rand_core::OsRng;
        let sk = SigningKey::generate(&mut OsRng);
        let pk_hex = hex::encode(sk.verifying_key().to_bytes());
        let body = r#"{"type":1}"#;
        let ts = time::OffsetDateTime::now_utc().unix_timestamp().to_string();
        let sig = sk.sign(format!("{ts}{body}").as_bytes());
        let sig_hex = hex::encode(sig.to_bytes());

        let mut h = HeaderMap::new();
        h.insert("X-Signature-Ed25519",   sig_hex.parse().unwrap());
        h.insert("X-Signature-Timestamp", ts.parse().unwrap());
        assert!(verify_discord_signature(&pk_hex, &h, r#"{"type":2}"#).is_err());
    }

    #[test]
    fn discord_extracts_string_option() {
        let payload = serde_json::json!({
            "data": {
                "name": "ask",
                "options": [
                    {"name":"limit",  "type": 4, "value": 5},
                    {"name":"prompt", "type": 3, "value": "hello there"}
                ]
            }
        });
        assert_eq!(extract_discord_command_text(&payload), "hello there");
    }

    #[test]
    fn discord_falls_back_to_command_name() {
        let payload = serde_json::json!({"data":{"name":"status"}});
        assert_eq!(extract_discord_command_text(&payload), "status");
    }
}