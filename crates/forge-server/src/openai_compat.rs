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
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use forge_domain::{EventEnvelope, ForgeEvent, MissionId, MissionStatus};
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{Duration, Instant};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::{require_full, ApiError, ApiState};

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
) -> Result<Response, ApiError> {
    require_full(&state, &headers)?;

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

    // Streaming path (Phase 5c): emit SSE chunks derived from live events.
    if req.stream {
        let model_name = if req.model.is_empty() { "forge-mission".to_string() } else { req.model.clone() };
        return Ok(streaming_completion(state.clone(), mid, model_name).into_response());
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
    }).into_response())
}

fn is_terminal(s: &MissionStatus) -> bool {
    matches!(s, MissionStatus::Completed | MissionStatus::Failed | MissionStatus::Cancelled)
}

// ---------------------------------------------------------------------------
// Streaming shim (Phase 5c) — `stream: true` on /v1/chat/completions.
// ---------------------------------------------------------------------------
//
// The output is a `text/event-stream` where each SSE `data:` frame is a
// serialized `chat.completion.chunk` compatible with OpenAI's spec:
//
//   data: {"id":"...","object":"chat.completion.chunk","choices":[
//          {"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}
//
//   data: {"id":"...","choices":[{"index":0,"delta":{"content":"..."},...}]}
//
//   data: {"id":"...","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}
//
//   data: [DONE]
//
// We derive the `content` deltas from Forge's own `ForgeEvent`s so a caller
// gets human-readable, live progress instead of raw tokens (which we don't
// have — Forge composes tools, not tokens). The terminal frame carries the
// OpenAI-shaped `finish_reason`.
//
// Backpressure: if a slow client falls off the broadcast channel we skip
// missed events (BroadcastStream::Lagged) and keep going.

fn streaming_completion(
    state: ApiState,
    mid: MissionId,
    model: String,
) -> Sse<impl Stream<Item = Result<SseEvent, axum::Error>>> {
    let id = format!("chatcmpl-{}", mid.as_uuid());
    let created = time::OffsetDateTime::now_utc().unix_timestamp();

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<SseEvent, axum::Error>>(64);

    // ---- Producer task: fan events + status polls into SSE chunks. ----
    let bus_rx = state.events.subscribe();
    let missions = state.missions.clone();
    tokio::spawn(async move {
        // 1) Initial role chunk.
        let init = chunk(&id, created, &model, Some(json!({"role":"assistant"})), None);
        if tx.send(Ok(sse_data(&init))).await.is_err() { return; }

        // 2) Stream event digests until terminal or timeout.
        let mut events = BroadcastStream::new(bus_rx);
        let started = Instant::now();
        let mut terminated = false;
        let mut finish_reason: Option<&'static str> = None;

        loop {
            // Poll mission status every 500ms so we still terminate cleanly
            // even when the broadcast channel is quiet.
            tokio::select! {
                maybe_env = events.next() => {
                    match maybe_env {
                        Some(Ok(env)) => {
                            if let Some(delta) = event_to_delta(&env, mid) {
                                let ch = chunk(&id, created, &model, Some(json!({"content": delta})), None);
                                if tx.send(Ok(sse_data(&ch))).await.is_err() { return; }
                            }
                        }
                        Some(Err(_)) => { /* lagged — drop */ }
                        None => break, // broadcast closed
                    }
                }
                _ = tokio::time::sleep(MISSION_POLL_INTERVAL) => {}
            }

            // Check terminal condition via detail().
            if let Ok(detail) = missions.detail(mid).await {
                if is_terminal(&detail.mission.status) {
                    finish_reason = Some(match detail.mission.status {
                        MissionStatus::Completed => "stop",
                        MissionStatus::Failed    => "error",
                        MissionStatus::Cancelled => "cancelled",
                        _                        => "stop",
                    });
                    // Emit a final summary chunk so a client that ignores
                    // per-event deltas still gets the outcome.
                    let mut summary = format!("\n\nMission {:?}.\n", detail.mission.status);
                    for g in &detail.goals {
                        summary.push_str(&format!("• [{:?}] {}\n", g.status, g.title));
                    }
                    let ch = chunk(&id, created, &model, Some(json!({"content": summary})), None);
                    let _ = tx.send(Ok(sse_data(&ch))).await;
                    terminated = true;
                    break;
                }
            }

            if started.elapsed() > MISSION_POLL_TIMEOUT {
                finish_reason = Some("length");
                let msg = format!(
                    "\n\nMission did not terminate within {}s. Poll /missions/{} for progress.\n",
                    MISSION_POLL_TIMEOUT.as_secs(), mid.as_uuid()
                );
                let ch = chunk(&id, created, &model, Some(json!({"content": msg})), None);
                let _ = tx.send(Ok(sse_data(&ch))).await;
                terminated = true;
                break;
            }
        }

        // 3) Final finish_reason chunk.
        let fr = finish_reason.unwrap_or(if terminated { "stop" } else { "error" });
        let final_ch = chunk(&id, created, &model, Some(json!({})), Some(fr.to_string()));
        let _ = tx.send(Ok(sse_data(&final_ch))).await;

        // 4) The OpenAI sentinel — a data-only frame with `[DONE]`.
        let done = SseEvent::default().data("[DONE]");
        let _ = tx.send(Ok(done)).await;
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Build a single `chat.completion.chunk` JSON blob.
fn chunk(
    id:      &str,
    created: i64,
    model:   &str,
    delta:   Option<serde_json::Value>,
    finish:  Option<String>,
) -> serde_json::Value {
    json!({
        "id":      id,
        "object":  "chat.completion.chunk",
        "created": created,
        "model":   model,
        "choices": [{
            "index":         0,
            "delta":         delta.unwrap_or_else(|| json!({})),
            "finish_reason": finish,
        }],
    })
}

fn sse_data(v: &serde_json::Value) -> SseEvent {
    // OpenAI clients don't consume `event:` or `id:` — just `data:`. Keep it
    // minimal so we're maximally compatible.
    SseEvent::default().data(v.to_string())
}

/// Distil a ForgeEvent into a short human-readable delta, or return None to
/// skip it entirely. We only forward events that meaningfully advance the
/// mission — heartbeat/verbose ones are filtered so clients don't drown.
fn event_to_delta(env: &EventEnvelope, mid: MissionId) -> Option<String> {
    // Only forward events tied to *our* mission.
    let ev_mid = match &env.event {
        ForgeEvent::MissionPlanningStarted { id }
        | ForgeEvent::MissionPlanningCompleted { id, .. }
        | ForgeEvent::MissionPlanningFailed { id, .. }
        | ForgeEvent::MissionStatusChanged { id, .. } => Some(*id),
        ForgeEvent::SkillsSelected { mission_id, .. }
        | ForgeEvent::ReplanRequested { mission_id, .. }
        | ForgeEvent::PlanRevised { mission_id, .. } => Some(*mission_id),
        _ => None,
    };
    if ev_mid? != mid { return None; }
    Some(match &env.event {
        ForgeEvent::MissionPlanningStarted { .. } => "Planning...\n".to_string(),
        ForgeEvent::MissionPlanningCompleted { goal_count, .. } =>
            format!("Planned {goal_count} goal(s).\n"),
        ForgeEvent::MissionPlanningFailed { error, .. } =>
            format!("Planning failed: {error}\n"),
        ForgeEvent::MissionStatusChanged { from, to, .. } =>
            format!("Status: {from:?} -> {to:?}\n"),
        ForgeEvent::SkillsSelected { skill_names, .. } if !skill_names.is_empty() =>
            format!("Selected skills: {}\n", skill_names.join(", ")),
        ForgeEvent::ReplanRequested { iteration, .. } =>
            format!("Replan #{iteration}...\n"),
        ForgeEvent::PlanRevised { iteration, added_goals, .. } =>
            format!("Replan #{iteration} added {added_goals} goal(s).\n"),
        _ => return None,
    })
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
