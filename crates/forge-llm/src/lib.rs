//! LLM router.
//!
//! Provider-agnostic trait + adapters. The router picks a provider per request
//! according to a strategy (failover-in-order for Phase 1) and skips providers
//! whose circuit breaker is currently open.

pub mod openrouter;
pub mod openai;
pub mod anthropic;
pub mod gemini;
pub mod azure_openai;
pub mod ollama;
pub mod groq;
pub mod mock;

// Phase 6a — semantic memory: embedding providers.
pub mod embed_openai;
pub mod embed_ollama;

use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("all providers failed; last error: {0}")]
    AllFailed(String),
    #[error("no providers configured")]
    NoProviders,
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("provider error: {0}")]
    Provider(String),
}

// ────────────────────────────────────────────────────────────────────────────
// Phase 6a — embedding provider trait.
// ────────────────────────────────────────────────────────────────────────────

/// A provider that turns arbitrary text into a fixed-dimension vector.
/// Used for semantic recall over `org_memory` in Phase 6a.
///
/// Providers are expected to be cheap to construct + `Clone`; the runtime
/// wraps them in an `Arc<dyn EmbeddingProvider>` and hands that to the
/// mission service.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Human-readable name (e.g. `"openai:text-embedding-3-small"`).
    fn name(&self) -> &str;
    /// The dimension of the vectors this provider returns. Used to skip
    /// rows in `org_memory` embedded by a different provider.
    fn dim(&self) -> usize;
    /// Embed a single string. Long inputs may be truncated by the
    /// provider; callers should keep individual embeddings under a few
    /// thousand tokens.
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,      // "system" | "user" | "assistant"
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// If set, ask the provider to constrain output to valid JSON.
    #[serde(default)]
    pub json_mode: bool,
    /// Optional mission attribution for cost accounting. When set, the
    /// router accumulates tokens per mission and emits mission-tagged
    /// LLM events. Ignored by providers themselves.
    #[serde(default)]
    pub mission_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub content: String,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub provider: String,
    pub model: String,
    pub latency_ms: u64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ProviderHealth { Up, Degraded, Down }

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn health(&self) -> ProviderHealth;
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError>;
}

/// Optional observability hook. When set on the router, every LLM call
/// invokes the sink for observability + persistence. Runtime provides an
/// implementation wired to the event bus. Kept as a trait here so the LLM
/// crate stays free of event/persistence coupling.
#[async_trait]
pub trait LlmEventSink: Send + Sync {
    async fn on_request(&self, meta: LlmRequestMeta);
    async fn on_response(&self, meta: LlmResponseMeta);
    async fn on_failure(&self, meta: LlmFailureMeta);
}

#[derive(Clone, Debug)]
pub struct LlmRequestMeta {
    pub request_id: String,
    pub provider:   String,
    pub model:      String,
    pub mission_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct LlmResponseMeta {
    pub request_id:        String,
    pub provider:          String,
    pub model:             String,
    pub mission_id:        Option<String>,
    pub prompt_tokens:     usize,
    pub completion_tokens: usize,
    pub latency_ms:        u64,
}

#[derive(Clone, Debug)]
pub struct LlmFailureMeta {
    pub request_id: String,
    pub provider:   String,
    pub model:      String,
    pub mission_id: Option<String>,
    pub error:      String,
}

/// Rolling counters kept per mission_id inside the router. Read via
/// `LlmRouter::drain_mission_cost` after a mission ends.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MissionLlmCost {
    pub calls:              usize,
    pub prompt_tokens:      usize,
    pub completion_tokens:  usize,
    pub total_latency_ms:   u64,
}

#[derive(Default)]
struct MissionCostBucket {
    calls:              AtomicU64,
    prompt_tokens:      AtomicU64,
    completion_tokens:  AtomicU64,
    total_latency_ms:   AtomicU64,
}
impl MissionCostBucket {
    fn snapshot(&self) -> MissionLlmCost {
        MissionLlmCost {
            calls:             self.calls.load(Ordering::Relaxed) as usize,
            prompt_tokens:     self.prompt_tokens.load(Ordering::Relaxed) as usize,
            completion_tokens: self.completion_tokens.load(Ordering::Relaxed) as usize,
            total_latency_ms:  self.total_latency_ms.load(Ordering::Relaxed),
        }
    }
    fn add(&self, resp: &CompletionResponse) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.prompt_tokens.fetch_add(resp.prompt_tokens as u64, Ordering::Relaxed);
        self.completion_tokens.fetch_add(resp.completion_tokens as u64, Ordering::Relaxed);
        self.total_latency_ms.fetch_add(resp.latency_ms, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub enum RoutingStrategy { FailoverInOrder }

pub struct LlmRouter {
    providers: Vec<Arc<dyn LlmProvider>>,
    strategy:  RoutingStrategy,
    breakers:  DashMap<String, Breaker>,
    sink:      parking_lot::RwLock<Option<Arc<dyn LlmEventSink>>>,
    /// Per-mission running totals. Populated on successful responses whose
    /// request carried a `mission_id`. Cleared by `drain_mission_cost`.
    costs:     DashMap<String, MissionCostBucket>,
    request_seq: AtomicU64,
}

struct Breaker { open_until: Option<Instant>, consecutive_failures: u32 }

impl LlmRouter {
    pub fn new(providers: Vec<Arc<dyn LlmProvider>>, strategy: RoutingStrategy) -> Self {
        Self {
            providers,
            strategy,
            breakers: DashMap::new(),
            sink: parking_lot::RwLock::new(None),
            costs: DashMap::new(),
            request_seq: AtomicU64::new(0),
        }
    }

    /// Set (or clear) the observability sink. Safe to call after boot.
    pub fn set_event_sink(&self, sink: Option<Arc<dyn LlmEventSink>>) {
        *self.sink.write() = sink;
    }

    fn next_request_id(&self, mission_id: Option<&str>) -> String {
        let n = self.request_seq.fetch_add(1, Ordering::Relaxed) + 1;
        match mission_id {
            Some(m) => format!("{m}:{n}"),
            None    => format!("free:{n}"),
        }
    }

    /// Drain and return the cost totals for a mission. Returns None if no
    /// LLM calls were attributed to this mission. Subsequent calls after a
    /// drain return None until new calls come in.
    pub fn drain_mission_cost(&self, mission_id: &str) -> Option<MissionLlmCost> {
        self.costs.remove(mission_id).map(|(_, b)| b.snapshot())
    }

    fn is_open(&self, name: &str) -> bool {
        self.breakers.get(name)
            .and_then(|b| b.open_until)
            .map(|t| Instant::now() < t)
            .unwrap_or(false)
    }

    fn record_success(&self, name: &str) {
        self.breakers.entry(name.to_string())
            .and_modify(|b| { b.open_until = None; b.consecutive_failures = 0; })
            .or_insert(Breaker { open_until: None, consecutive_failures: 0 });
    }

    fn record_failure(&self, name: &str) {
        let mut entry = self.breakers.entry(name.to_string())
            .or_insert(Breaker { open_until: None, consecutive_failures: 0 });
        entry.consecutive_failures += 1;
        if entry.consecutive_failures >= 3 {
            entry.open_until = Some(Instant::now() + std::time::Duration::from_secs(30));
        }
    }

    pub async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        if self.providers.is_empty() { return Err(LlmError::NoProviders); }
        let request_id = self.next_request_id(req.mission_id.as_deref());
        let sink_opt = self.sink.read().clone();
        let mut last_err: Option<String> = None;
        let mut last_provider: Option<String> = None;
        let mut all_errs: Vec<String> = Vec::new();

        match self.strategy {
            RoutingStrategy::FailoverInOrder => {
                for p in &self.providers {
                    if self.is_open(p.name()) {
                        tracing::debug!(provider = p.name(), "circuit-breaker open, skipping");
                        continue;
                    }
                    if let Some(sink) = &sink_opt {
                        sink.on_request(LlmRequestMeta {
                            request_id: request_id.clone(),
                            provider:   p.name().to_string(),
                            model:      req.model.clone(),
                            mission_id: req.mission_id.clone(),
                        }).await;
                    }
                    match p.complete(req.clone()).await {
                        Ok(resp) => {
                            self.record_success(p.name());
                            if let Some(mid) = req.mission_id.as_deref() {
                                self.costs
                                    .entry(mid.to_string())
                                    .or_default()
                                    .add(&resp);
                            }
                            if let Some(sink) = &sink_opt {
                                sink.on_response(LlmResponseMeta {
                                    request_id: request_id.clone(),
                                    provider:   resp.provider.clone(),
                                    model:      resp.model.clone(),
                                    mission_id: req.mission_id.clone(),
                                    prompt_tokens:     resp.prompt_tokens,
                                    completion_tokens: resp.completion_tokens,
                                    latency_ms:        resp.latency_ms,
                                }).await;
                            }
                            return Ok(resp);
                        }
                        Err(e) => {
                            self.record_failure(p.name());
                            let msg = e.to_string();
                            tracing::warn!(provider = p.name(), err = %msg, "provider failed");
                            if let Some(sink) = &sink_opt {
                                sink.on_failure(LlmFailureMeta {
                                    request_id: request_id.clone(),
                                    provider:   p.name().to_string(),
                                    model:      req.model.clone(),
                                    mission_id: req.mission_id.clone(),
                                    error:      msg.clone(),
                                }).await;
                            }
                            last_provider = Some(p.name().to_string());
                            last_err = Some(msg.clone());
                            all_errs.push(format!("[{}] {}", p.name(), msg));
                        }
                    }
                }
            }
        }
        let _ = last_provider;
        let combined = if all_errs.is_empty() {
            last_err.unwrap_or_else(|| "no attempt made".into())
        } else {
            all_errs.join(" | ")
        };
        Err(LlmError::AllFailed(combined))
    }

    pub fn provider_names(&self) -> Vec<String> {
        self.providers.iter().map(|p| p.name().to_string()).collect()
    }
}
