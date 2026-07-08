//! # OpenAI-compat shim (`POST /v1/chat/completions`)
//!
//! Minimum viable adapter so a client written against the OpenAI Chat
//! Completions REST shape can drive Forge as if it were a reasoning-agent
//! model. We map:
//!
//!   * `messages[-1].content` → the mission's `title` + `description`
//!   * `model`                → passed through as metadata only (Forge picks
//!                              its own LLM based on its runtime config)
//!   * Any earlier `messages`  → concatenated into `description` for context
//!
//! Then we call `create_mission → plan_and_run`, poll the mission until it
//! reaches a terminal state, and return the final summary as the assistant
//! message.
//!
//! **Non-streaming only in MVP.** Streaming (`stream: true`) responds with
//! HTTP 400 explaining that streaming isn't implemented yet — a follow-up
//! can implement it by forwarding SSE.
//!
//! ## What's deliberately missing
//!
//! - `tools` / function-calling — Forge already has tools internally.
//! - `logprobs`, `top_logprobs`, `seed` — meaningless for a mission runner.
//! - Multi-turn — every request creates a fresh mission. To continue a
//!   mission, use the native `POST /missions/:id/extend` endpoint.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use forge_domain::MissionStatus;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

use crate::{check_auth, ApiError, ApiState};

// ---------------------------------------------------------------------------
// Request / response DTOs — subset of the OpenAI shape.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ChatCompletionRequest {
    #[serde(default)]
    pub model:    String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream:   bool,

    /// Not honoured — Forge decides its own budget via the reflection/replan
    /// caps in its runtime config. We accept the field so clients don't 400
    /// on unknown-field-strict parsers (there aren't any at the axum layer,
    /// but the OpenAI SDKs often send them).
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChatMessage {
    pub role:    String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionResponse {
    pub id:       String,
    pub object:   &'static str,
    pub created:  i64,
    pub model:    String,
    pub choices:  Vec<Choice>,
    pub usage:    Usage,
    /// Non-OpenAI extension: expose the Forge mission id so a caller can
    /// join to /events or /missions/:id later.
    pub forge_mission_id: String,
}

#[derive(Debug, Serialize)]
pub struct Choice {
    pub index:         u32,
    pub message:       ChatCompletionMessage,
    pub finish_reason: String,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionMessage {
    pub role:    &'static str,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct Usage {
    pub prompt_tokens:     u32,
    pub completion_tokens: u32,
    pub total_tokens:      u32,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// How long we'll block waiting for a mission to terminate before giving
/// up and returning a "timed_out" finish reason. Long enough for a
/// non-trivial mission, short enough that a client isn't stuck forever.
const MISSION_POLL_TIMEOUT: Duration = Duration::from_secs(300);
const MISSION_POLL_INTERVAL: Duration = Duration::from_millis(500);

pub async fn chat_completions(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(req): Json<ChatCompletionRequest>,
) -> Result<Json<ChatCompletionResponse>, ApiError> {
    check_auth(&state, &headers)?;

    if req.stream {
        return Err(ApiError::BadRequest(
            "streaming (stream=true) is not implemented — subscribe to /events instead".into()
        ));
    }
    let (title, description) = messages_to_mission(&req.messages)
        .ok_or_else(|| ApiError::BadRequest("at least one user message is required".into()))?;

    // Create + kick off out-of-band so a planner failure surfaces as a
    // terminal mission state (which we then translate to
    // `finish_reason: "error"`) rather than a 500.
    let mid = state.missions.create(title, description).await?;
    {
        let svc = state.missions.clone();
        tokio::spawn(async move {
            if let Err(e) = svc.plan_and_run(mid).await {
                tracing::warn!(mission = %mid.as_uuid(), err = %e,
                    "openai-compat: background plan_and_run failed (mission marked Failed)");
            }
        });
    }

    // Poll until terminal or timeout.
    let started = Instant::now();
    let finish_reason;
    let mut summary_lines: Vec<String> = Vec::new();
    loop {
        let detail = state.missions.detail(mid).await?;
        let status = detail.mission.status.clone();
        if is_terminal(&status) {
            finish_reason = match status {
                MissionStatus::Completed => "stop",
                MissionStatus::Failed    => "error",
                MissionStatus::Cancelled => "cancelled",
                _                        => "stop",
            }.to_string();
            summary_lines.push(format!("Mission {:?}.", status));
            for goal in &detail.goals {
                summary_lines.push(format!("• [{:?}] {}", goal.status, goal.title));
            }
            break;
        }
        if started.elapsed() > MISSION_POLL_TIMEOUT {
            finish_reason = "length".to_string(); // OpenAI uses "length" for cap-exceeded
            summary_lines.push(format!(
                "Mission did not terminate within {} seconds. Poll /missions/{} for progress.",
                MISSION_POLL_TIMEOUT.as_secs(),
                mid.as_uuid()
            ));
            break;
        }
        tokio::time::sleep(MISSION_POLL_INTERVAL).await;
    }

    let content = summary_lines.join("\n");
    let created = time::OffsetDateTime::now_utc().unix_timestamp();
    let id = format!("chatcmpl-{}", mid.as_uuid());

    // Rough token estimate — the real usage lives in mission_cost_summary
    // events, but we don't block waiting for that.
    let content_tokens = (content.len() / 4) as u32;
    let prompt_tokens: u32 = req.messages.iter().map(|m| (m.content.len() / 4) as u32).sum();

    Ok(Json(ChatCompletionResponse {
        id,
        object:  "chat.completion",
        created,
        model:   if req.model.is_empty() { "forge-mission".into() } else { req.model },
        choices: vec![Choice {
            index: 0,
            message: ChatCompletionMessage { role: "assistant", content },
            finish_reason,
        }],
        usage: Usage {
            prompt_tokens,
            completion_tokens: content_tokens,
            total_tokens: prompt_tokens + content_tokens,
        },
        forge_mission_id: mid.as_uuid().to_string(),
    }))
}

fn is_terminal(s: &MissionStatus) -> bool {
    matches!(s, MissionStatus::Completed | MissionStatus::Failed | MissionStatus::Cancelled)
}

/// Fold a chat history into `(title, description)`.
///
/// Convention:
///   * Title = first non-empty line of the LAST user message, capped to 80 chars.
///   * Description = the full concatenation of every message (role prefixed)
///     minus the title line. This gives Forge as much context as the caller
///     had, while keeping the mission title short and human-readable.
fn messages_to_mission(msgs: &[ChatMessage]) -> Option<(String, String)> {
    let last_user = msgs.iter().rev().find(|m| m.role == "user")?;
    let title_line = last_user
        .content
        .lines()
        .find(|l| !l.trim().is_empty())?;
    let title = title_line.chars().take(80).collect::<String>();
    let mut desc = String::new();
    for m in msgs {
        desc.push_str(&format!("[{}]\n{}\n\n", m.role, m.content));
    }
    Some((title.trim().to_string(), desc.trim().to_string()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage { role: role.into(), content: content.into() }
    }

    #[test]
    fn messages_to_mission_single_user() {
        let (title, desc) = messages_to_mission(&[
            msg("user", "Print pi to 3 digits\nExtra context here."),
        ]).unwrap();
        assert_eq!(title, "Print pi to 3 digits");
        assert!(desc.contains("Print pi"));
        assert!(desc.contains("[user]"));
    }

    #[test]
    fn messages_to_mission_history_kept() {
        let (title, desc) = messages_to_mission(&[
            msg("system", "You are Forge."),
            msg("user",   "Say hi."),
            msg("assistant", "Hi."),
            msg("user",   "Now compute 2+2."),
        ]).unwrap();
        assert_eq!(title, "Now compute 2+2.");
        assert!(desc.contains("[system]"));
        assert!(desc.contains("[assistant]"));
        assert!(desc.contains("Say hi."));
    }

    #[test]
    fn messages_to_mission_no_user_returns_none() {
        assert!(messages_to_mission(&[
            msg("system", "hi"),
            msg("assistant", "hello"),
        ]).is_none());
    }

    #[test]
    fn messages_to_mission_title_capped_at_80() {
        let long = "a".repeat(200);
        let (title, _) = messages_to_mission(&[msg("user", &long)]).unwrap();
        assert_eq!(title.chars().count(), 80);
    }
}
