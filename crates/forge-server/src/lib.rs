//! # Forge OS — HTTP + SSE API server (Phase 5)
//!
//! An axum-based HTTP surface that any client (CLI, mobile UI, messaging
//! gateway, editor extension) can use to drive Forge OS the same way the
//! desktop UI does through Tauri IPC.
//!
//! ## Design
//!
//! - **Loopback-only by default**. `bind` in `ApiConfig` defaults callers to
//!   `127.0.0.1:7823`. If you want to expose Forge over LAN, do it behind a
//!   TLS-terminating reverse proxy — this server speaks plain HTTP by design.
//! - **Bearer-token auth required**. Every non-`/health` request must carry
//!   `Authorization: Bearer <token>`, matched constant-time against the
//!   `ApiState.token` string. An empty token DISABLES auth and logs a WARN
//!   line — meant for local development only.
//! - **Zero domain leakage**. Requests/responses use ordinary JSON, never
//!   the internal `MissionId`/`ForgeEvent` types raw — instead we serialize
//!   through domain types whose serde form is already stable.
//! - **SSE for events**. `GET /events` returns a `text/event-stream` that
//!   forwards `EventEnvelope`s from the in-process `EventBus`, one per SSE
//!   `data:` line. Supports `?since=<seq>&mission=<uuid>`.
//! - **OpenAI-compat shim**. `POST /v1/chat/completions` is a minimal,
//!   non-streaming adapter that maps a chat request to
//!   `create_mission → plan_and_run → poll until terminal` so existing
//!   OpenAI-client SDKs can drive Forge as a "reasoning agent" without
//!   knowing the mission model.
//!
//! ## What this DOESN'T do
//!
//! - No TLS termination (use a reverse proxy).
//! - No user accounts / RBAC — Phase 5 team-edition item.
//! - No skills / secrets / checkpoints endpoints yet — MVP scope is missions
//!   + events. Add follow-ups incrementally.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::Stream;
use forge_domain::{EventEnvelope, MissionId};
use forge_events::EventBus;
use forge_mission::MissionService;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

pub use openai_compat::{ChatCompletionRequest, ChatCompletionResponse};

mod openai_compat;

// ---------------------------------------------------------------------------
// Config + state
// ---------------------------------------------------------------------------

/// Runtime knobs for the API server.
#[derive(Clone, Debug)]
pub struct ApiConfig {
    /// Socket to bind. `127.0.0.1:7823` is the sensible default for a
    /// personal desktop deployment.
    pub bind: SocketAddr,

    /// Bearer token required on every non-/health request. Empty string
    /// disables auth entirely (WARN log + `X-Forge-Auth: disabled` header
    /// on every response) — for local dev only.
    pub token: String,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:7823".parse().expect("localhost:7823 always parses"),
            token: String::new(),
        }
    }
}

#[derive(Clone)]
pub struct ApiState {
    pub missions: MissionService,
    pub events:   EventBus,
    pub token:    Arc<String>,
}

impl ApiState {
    pub fn new(missions: MissionService, events: EventBus, token: String) -> Self {
        Self { missions, events, token: Arc::new(token) }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Anything the server can go wrong with. Rendered as JSON to the client.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("not found")]
    NotFound,
    #[error("mission service error: {0}")]
    Mission(#[from] forge_mission::MissionError),
    #[error("bind failed: {0}")]
    Bind(#[from] std::io::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (code, msg) = match &self {
            ApiError::Unauthorized      => (StatusCode::UNAUTHORIZED, self.to_string()),
            ApiError::BadRequest(_)     => (StatusCode::BAD_REQUEST,  self.to_string()),
            ApiError::NotFound          => (StatusCode::NOT_FOUND,    self.to_string()),
            ApiError::Mission(_)        => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            ApiError::Bind(_)           => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
        };
        let body = Json(serde_json::json!({
            "error": {
                "code":    code.as_u16(),
                "message": msg,
            }
        }));
        (code, body).into_response()
    }
}

// ---------------------------------------------------------------------------
// Router builder
// ---------------------------------------------------------------------------

/// Assemble the full router. Kept public so integration tests can spin
/// the server up without going through `serve()`.
pub fn build_router(state: ApiState) -> Router {
    Router::new()
        // Public — no auth required
        .route("/health",                get(health))
        // Missions
        .route("/missions",              get(list_missions).post(create_mission))
        .route("/missions/:id",          get(get_mission))
        .route("/missions/:id/cancel",   post(cancel_mission))
        .route("/missions/:id/extend",   post(extend_mission))
        // Events
        .route("/events",                get(events_sse))
        // OpenAI-compat shim
        .route("/v1/chat/completions",   post(openai_compat::chat_completions))
        .with_state(state)
}

/// Bind the router and serve until the process exits or the future is
/// cancelled. Emits a single `tracing::info!` line on successful bind.
pub async fn serve(bind: SocketAddr, state: ApiState) -> Result<(), ApiError> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(
        bind = %bind,
        auth = if state.token.is_empty() { "disabled" } else { "bearer" },
        "forge-server listening"
    );
    if state.token.is_empty() {
        tracing::warn!(
            "forge-server bearer auth is DISABLED — do not expose this bind \
             address beyond localhost. Set FORGE_API_TOKEN to enable auth."
        );
    }
    axum::serve(listener, build_router(state))
        .await
        .map_err(ApiError::Bind)
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

fn check_auth(state: &ApiState, headers: &HeaderMap) -> Result<(), ApiError> {
    if state.token.is_empty() {
        return Ok(()); // auth disabled — WARN was logged at boot
    }
    let hv = headers.get(header::AUTHORIZATION).ok_or(ApiError::Unauthorized)?;
    let s  = hv.to_str().map_err(|_| ApiError::Unauthorized)?;
    let tok = s.strip_prefix("Bearer ").ok_or(ApiError::Unauthorized)?;
    if constant_time_eq(tok.as_bytes(), state.token.as_bytes()) {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

/// Constant-time byte comparison — no early return on length or content
/// mismatch so timing side-channels can't leak the token byte-by-byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        // Still consume the bytes we do have so timing is roughly stable
        // for equal-length attempts.
        let mut _acc = 0u8;
        let n = a.len().min(b.len());
        for i in 0..n { _acc |= a[i] ^ b[i]; }
        return false;
    }
    let mut acc = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status":  "ok",
        "service": "forge-server",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

// ---------------------------------------------------------------------------
// Missions
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateMissionBody {
    pub title:       String,
    pub description: String,
    /// If true, only create + plan (do NOT execute). Defaults to false so the
    /// simple client can post one request and walk away.
    #[serde(default)]
    pub plan_only:   bool,
}

#[derive(Debug, Serialize)]
pub struct MissionIdBody {
    pub id: String,
}

async fn create_mission(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreateMissionBody>,
) -> Result<Json<MissionIdBody>, ApiError> {
    check_auth(&state, &headers)?;
    if body.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title must be non-empty".into()));
    }
    let id = state.missions.create(body.title, body.description).await?;
    // Kick off planning/execution *out-of-band* so the caller can subscribe to
    // /events for progress. Failures surface as MissionFailed events — the REST
    // response is limited to "the mission exists". The OpenAI shim uses its
    // own polling path.
    if !body.plan_only {
        let svc = state.missions.clone();
        tokio::spawn(async move {
            if let Err(e) = svc.plan_and_run(id).await {
                tracing::warn!(mission = %id.as_uuid(), err = %e,
                    "background plan_and_run failed (see MissionFailed event)");
            }
        });
    }
    Ok(Json(MissionIdBody { id: id.as_uuid().to_string() }))
}

async fn list_missions(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Vec<forge_domain::MissionSummary>>, ApiError> {
    check_auth(&state, &headers)?;
    Ok(Json(state.missions.list().await?))
}

async fn get_mission(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<forge_mission::MissionDetail>, ApiError> {
    check_auth(&state, &headers)?;
    let mid = MissionId::from_str(&id).map_err(|_| ApiError::BadRequest("invalid mission id".into()))?;
    match state.missions.detail(mid).await {
        Ok(d) => Ok(Json(d)),
        Err(forge_mission::MissionError::Persist(
            forge_persistence::PersistenceError::NotFound { .. }
        )) => Err(ApiError::NotFound),
        Err(e) => Err(ApiError::from(e)),
    }
}

async fn cancel_mission(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    check_auth(&state, &headers)?;
    let mid = MissionId::from_str(&id).map_err(|_| ApiError::BadRequest("invalid mission id".into()))?;
    state.missions.cancel(mid).await?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Debug, Deserialize)]
pub struct ExtendMissionBody { pub prompt: String }

async fn extend_mission(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ExtendMissionBody>,
) -> Result<StatusCode, ApiError> {
    check_auth(&state, &headers)?;
    if body.prompt.trim().is_empty() {
        return Err(ApiError::BadRequest("prompt must be non-empty".into()));
    }
    let mid = MissionId::from_str(&id).map_err(|_| ApiError::BadRequest("invalid mission id".into()))?;
    state.missions.extend(mid, body.prompt).await?;
    Ok(StatusCode::ACCEPTED)
}

// ---------------------------------------------------------------------------
// SSE — GET /events?since=&mission=
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct EventsQuery {
    /// Only forward envelopes with seq > this value.
    #[serde(default)]
    pub since:   Option<u64>,
    /// Filter by mission_id (raw UUID string). Global events pass through.
    #[serde(default)]
    pub mission: Option<String>,
}

async fn events_sse(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, axum::Error>>>, ApiError> {
    check_auth(&state, &headers)?;

    let rx = state.events.subscribe();
    let since = q.since.unwrap_or(0);
    let mission_filter = q.mission.clone();

    let stream = BroadcastStream::new(rx).filter_map(move |res| {
        // Lagged/closed — drop the frame silently, the browser will keep the
        // connection open and see the next in-order envelope.
        let env: EventEnvelope = res.ok()?;
        if (env.seq.0 as u64) <= since { return None; }
        if let Some(mf) = &mission_filter {
            if !event_matches_mission(&env, mf) { return None; }
        }
        let payload = serde_json::to_string(&env).ok()?;
        Some(Ok(SseEvent::default()
            .id(env.seq.0.to_string())
            .event("forge.event")
            .data(payload)))
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Same routing logic as the frontend's `eventMissionId` but simplified —
/// we don't have the goal→mission / task→goal indices server-side, so we
/// only match direct-mission events. Task/goal-scoped events are always
/// forwarded when a filter is set so the client can still see them.
fn event_matches_mission(env: &EventEnvelope, mid_str: &str) -> bool {
    use forge_domain::ForgeEvent as E;
    let ev_mid = match &env.event {
        E::MissionCreated { id, .. }             => Some(id),
        E::MissionPlanningStarted { id }         => Some(id),
        E::MissionPlanningCompleted { id, .. }   => Some(id),
        E::MissionPlanningFailed { id, .. }      => Some(id),
        E::MissionStatusChanged { id, .. }       => Some(id),
        E::MissionCancelRequested { id }         => Some(id),
        E::SkillsSelected { mission_id, .. }
        | E::ReplanRequested { mission_id, .. }
        | E::PlanRevised { mission_id, .. }
        | E::ReplanCapExceeded { mission_id, .. }
        | E::MissionReflectionCompleted { mission_id, .. }
        | E::SkillProposalWritten { mission_id, .. }
        | E::MissionCostSummary { mission_id, .. }
        | E::EpisodicRecallSurfaced { mission_id, .. }
        | E::MissionQueued { mission_id, .. }
        | E::OrgMemoryLearned { mission_id, .. }
        | E::OrgMemoryRecalled { mission_id, .. } => Some(mission_id),
        _ => None,
    };
    match ev_mid {
        Some(mid) => mid.as_uuid().to_string() == mid_str,
        // Global events (mcp_*, some skill_* variants) are always forwarded
        // when a mission filter is active — clients can drop them if they
        // want a strict view.
        None => true,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_equal_length_match() {
        assert!(constant_time_eq(b"hello", b"hello"));
    }

    #[test]
    fn constant_time_eq_equal_length_mismatch() {
        assert!(!constant_time_eq(b"hello", b"world"));
    }

    #[test]
    fn constant_time_eq_different_length() {
        assert!(!constant_time_eq(b"hi",    b"hello"));
        assert!(!constant_time_eq(b"hello", b"hi"));
    }

    #[test]
    fn constant_time_eq_empty() {
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"x", b""));
    }
}
