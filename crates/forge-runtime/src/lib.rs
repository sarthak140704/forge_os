//! Runtime composition root.
//!
//! Boots every subsystem and returns a `Runtime` that hosts the
//! `MissionService` + `EventBus` for external consumption (Tauri, tests).
//!
//! The point of a composition root is: one place to configure the entire
//! system, one place to swap dependencies (e.g. SQLite→Postgres, real LLM
//! →Mock), no service inside the workspace needs to know how anything else
//! is built.

use forge_domain::{ForgeEvent, MissionId};
use forge_events::EventBus;
use forge_execution::{ExecutionDeps, ExecutionEngine, MaterializeError, TaskInputMaterializer, UpstreamResult};
use forge_llm::{
    LlmEventSink, LlmFailureMeta, LlmProvider, LlmRequestMeta, LlmResponseMeta, LlmRouter,
    RoutingStrategy,
};
use forge_mcp::{McpConfig, McpRegistry, McpServerStatus};
use forge_mission::{LearningDeps, MissionService};
use forge_persistence::{
    connect, SqliteEventStore, SqliteGoalRepository, SqliteMissionRepository, SqlitePool,
    SqliteReflectionRepository, SqliteTaskRepository, TaskRepository, GoalRepository,
};
use forge_planner::{Planner, Reflector};
use forge_policy::PolicyEngine;
use forge_skills::{FilesystemSkillLoader, ProposalWriter, SkillLoader, SkillRegistry};
use forge_tools::{ToolRegistry};
use serde::Deserialize;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use thiserror::Error;

pub mod memory;
pub mod user_memory;
pub mod feature_flags;
pub mod episodic_recall;
pub mod checkpoints;
pub mod secrets;
pub mod audit;
pub use memory::ProjectMemory;
pub use user_memory::UserMemory;
pub use feature_flags::FeatureFlags;
pub use checkpoints::{Checkpoint, CheckpointStore};

/// Bridges the executor's `TaskInputMaterializer` trait to `forge_planner`.
/// Kept in the runtime layer (not the planner crate) so `forge-planner`
/// doesn't need to know about `forge-execution`.
struct PlannerMaterializer {
    planner: Arc<Planner>,
    missions: Arc<dyn forge_persistence::MissionRepository>,
    tools: Arc<ToolRegistry>,
}

#[async_trait::async_trait]
impl TaskInputMaterializer for PlannerMaterializer {
    async fn materialize(
        &self,
        mission_id: forge_domain::MissionId,
        goal: &forge_domain::Goal,
        tasks: &[forge_domain::Task],
        upstream: &[UpstreamResult],
    ) -> Result<Vec<serde_json::Value>, MaterializeError> {
        let mission = self.missions.get(mission_id).await
            .map_err(|e| MaterializeError::Llm(format!("load mission: {e}")))?;
        let task_pairs: Vec<(String, serde_json::Value)> = tasks.iter()
            .map(|t| (t.tool.clone(), t.input.clone()))
            .collect();
        let upstream_tuples: Vec<(String, String, String)> = upstream.iter()
            .map(|u| (u.goal_title.clone(), u.tool.clone(), u.result_summary.clone()))
            .collect();
        self.planner.materialize_task_inputs(
            Some(mission_id),
            &mission.title,
            &goal.title,
            &goal.description,
            &task_pairs,
            &upstream_tuples,
            &self.tools.schemas(),
        ).await.map_err(|e| match e {
            forge_planner::PlannerError::ParseJson(m)
            | forge_planner::PlannerError::Invalid(m) => MaterializeError::Malformed(m),
            other => MaterializeError::Llm(other.to_string()),
        })
    }
}

/// Publishes LLM router callbacks to the domain event bus so they flow into
/// the event store + UI stream alongside every other state change.
struct EventBusLlmSink {
    events: EventBus,
}

/// Bridges `forge_mission::EpisodicRecall` to the runtime's persistence
/// layer, keeping the mission crate free of persistence detail.
struct RuntimeEpisodicRecall {
    missions_repo:    Arc<dyn forge_persistence::MissionRepository>,
    reflections_repo: Arc<dyn forge_persistence::ReflectionRepository>,
    max_recall:       usize,
}

#[async_trait::async_trait]
impl forge_mission::EpisodicRecall for RuntimeEpisodicRecall {
    async fn recall_for(&self, mission: &forge_domain::Mission) -> Option<forge_mission::RecallSurface> {
        let block = episodic_recall::build_recall_block(
            &self.missions_repo,
            &self.reflections_repo,
            mission,
            self.max_recall,
        ).await?;
        let text = format!("{} {}", mission.title, mission.description);
        let keywords = episodic_recall::extract_keywords(&text);
        // build_recall_block emits one "- **Title**" line per matched mission.
        let prior_count = block.matches("\n- **").count();
        Some(forge_mission::RecallSurface { block, keywords, prior_count })
    }
    }
    #[async_trait::async_trait]
    impl LlmEventSink for EventBusLlmSink {
    async fn on_request(&self, meta: LlmRequestMeta) {
        let mid = meta.mission_id.as_deref().and_then(|s| MissionId::from_str(s).ok());
        let _ = self.events.publish(ForgeEvent::LlmRequested {
            request_id: meta.request_id,
            provider:   meta.provider,
            model:      meta.model,
            mission_id: mid,
        }).await;
    }
    async fn on_response(&self, meta: LlmResponseMeta) {
        let mid = meta.mission_id.as_deref().and_then(|s| MissionId::from_str(s).ok());
        let _ = self.events.publish(ForgeEvent::LlmResponded {
            request_id: meta.request_id,
            latency_ms: meta.latency_ms,
            prompt_tokens:     meta.prompt_tokens,
            completion_tokens: meta.completion_tokens,
            mission_id: mid,
            provider:   meta.provider,
            model:      meta.model,
        }).await;
    }
    async fn on_failure(&self, meta: LlmFailureMeta) {
        let mid = meta.mission_id.as_deref().and_then(|s| MissionId::from_str(s).ok());
        let _ = self.events.publish(ForgeEvent::LlmFailed {
            request_id: meta.request_id,
            provider:   meta.provider,
            error:      meta.error,
            mission_id: mid,
        }).await;
    }
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("persistence: {0}")]
    Persistence(#[from] forge_persistence::PersistenceError),
    #[error("policy: {0}")]
    Policy(#[from] forge_policy::PolicyError),
    #[error("config: {0}")]
    Config(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Debug, Deserialize)]
pub struct RuntimeConfig {
    pub workspace_root: PathBuf,
    pub db_path: PathBuf,
    pub policy_path: Option<PathBuf>,
    pub llm: LlmConfig,
    #[serde(default = "default_max_parallel")]
    pub max_parallel_goals: usize,
    /// Directory holding skills. Files under `active/` (or the root) are
    /// loaded on boot; proposals get written under `proposed/`. Defaults to
    /// `workspace_root/../skills/` if unset. Missing directory is fine — the
    /// loader returns an empty registry.
    #[serde(default)]
    pub skills_root: Option<PathBuf>,
    /// Location of `mcp.yaml`. Defaults to `db_path.parent()/mcp.yaml`.
    /// Missing file is fine — MCP is opt-in.
    #[serde(default)]
    pub mcp_config: Option<PathBuf>,
}
fn default_max_parallel() -> usize { 4 }

#[derive(Clone, Debug, Deserialize)]
pub struct LlmConfig {
    /// Ordered list of providers to try. First success wins.
    pub providers: Vec<LlmProviderConfig>,
    /// Model id passed to the winning provider.
    pub model: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LlmProviderConfig {
    OpenRouter { api_key_env: String },
    OpenAi { api_key_env: String, #[serde(default)] organization_env: Option<String>, #[serde(default)] base: Option<String> },
    Groq { api_key_env: String },
    Ollama { #[serde(default = "default_ollama_base")] base: String },
}
fn default_ollama_base() -> String { "http://127.0.0.1:11434".to_string() }

impl RuntimeConfig {
    pub fn from_toml_str(s: &str) -> Result<Self, RuntimeError> {
        toml::from_str(s).map_err(|e| RuntimeError::Config(e.to_string()))
    }

    pub fn from_toml_file(path: &std::path::Path) -> Result<Self, RuntimeError> {
        Self::from_toml_str(&std::fs::read_to_string(path)?)
    }
}

pub struct Runtime {
    pub config:   RuntimeConfig,
    pub pool:     SqlitePool,
    pub events:   EventBus,
    pub missions: MissionService,
    pub tools:    Arc<ToolRegistry>,
    pub llm:      Arc<LlmRouter>,
    /// MCP registry. Held to keep the child processes alive for the
    /// lifetime of the runtime — dropping it kills every MCP server.
    pub mcp:      Arc<McpRegistry>,
    /// Shadow-git checkpoint store. Snapshots the workspace after each
    /// mutating task so `revert_checkpoint` can undo any change.
    pub checkpoints: CheckpointStore,

    /// Goal repository. Exposed for headless drivers/tests that need to
    /// insert synthetic mission/goal/task chains and publish events directly
    /// against the runtime's bus.
    pub goals: Arc<SqliteGoalRepository>,
    /// Task repository. See `goals` above for rationale.
    pub tasks: Arc<SqliteTaskRepository>,
}

impl Runtime {
    pub async fn boot(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        tracing::info!(
            workspace = %config.workspace_root.display(),
            db = %config.db_path.display(),
            "booting forge runtime"
        );
        // Ensure workspace + db dir exist.
        std::fs::create_dir_all(&config.workspace_root)?;
        if let Some(parent) = config.db_path.parent() { std::fs::create_dir_all(parent)?; }

        // SQLite.
        let db_url = format!("sqlite://{}?mode=rwc", config.db_path.display().to_string().replace('\\', "/"));
        let pool = connect(&db_url).await?;

        // Repositories + event store.
        let missions_repo = Arc::new(SqliteMissionRepository::new(pool.clone()));
        let goals_repo    = Arc::new(SqliteGoalRepository::new(pool.clone()));
        let tasks_repo    = Arc::new(SqliteTaskRepository::new(pool.clone()));
        let event_store   = Arc::new(SqliteEventStore::new(pool.clone()));

        let events = EventBus::new(event_store, 1024);

        // Policy.
        let policy = match &config.policy_path {
            Some(p) if p.exists() => Arc::new(PolicyEngine::from_file(p)?),
            _ => Arc::new(PolicyEngine::empty()),
        };

        // Tools.
        let mut registry = ToolRegistry::new();
        for tool in forge_tools::builtins::all(config.workspace_root.clone()) {
            registry.register(tool);
        }

        // MCP servers — spawn every enabled entry in `mcp.yaml`, register
        // their tools as `mcp.<server>.<tool>`. Failures never block boot.
        let mcp_cfg_path = config.mcp_config.clone().unwrap_or_else(|| {
            config.db_path.parent().map(|p| p.join("mcp.yaml")).unwrap_or_else(|| PathBuf::from("mcp.yaml"))
        });
        let mcp_config = McpConfig::load_or_empty(&mcp_cfg_path, &config.workspace_root);
        let mcp_boot = McpRegistry::start(&mcp_config).await;
        for status in &mcp_boot.statuses {
            match status {
                McpServerStatus::Started { name, tools } => {
                    tracing::info!(mcp = %name, tools = tools.len(), "mcp server started");
                    // Emit event best-effort — a broadcast full channel is
                    // fine here; UI can catch up via replay.
                    let _ = events.publish(ForgeEvent::McpServerStarted {
                        name: name.clone(),
                        tools: tools.clone(),
                    }).await;
                }
                McpServerStatus::Failed { name, error } => {
                    tracing::warn!(mcp = %name, error = %error, "mcp server failed");
                    let _ = events.publish(ForgeEvent::McpServerFailed {
                        name: name.clone(),
                        error: error.clone(),
                    }).await;
                }
                McpServerStatus::Disabled { name } => {
                    tracing::info!(mcp = %name, "mcp server disabled by config");
                }
            }
        }
        // Namespaced by mcp.<server>.<tool>, so no collision with built-ins.
        for tool in mcp_boot.tools {
            registry.register(tool);
        }
        let mcp = Arc::new(mcp_boot.registry);

        let tools = Arc::new(registry);

        // LLM providers.
        let mut providers: Vec<Arc<dyn LlmProvider>> = Vec::new();
        for pc in &config.llm.providers {
            match pc {
                LlmProviderConfig::OpenRouter { api_key_env } => {
                    match std::env::var(api_key_env) {
                        Ok(key) if !key.is_empty() => {
                            providers.push(Arc::new(forge_llm::openrouter::OpenRouterProvider::new(key)));
                        }
                        _ => tracing::warn!(env = %api_key_env, "OpenRouter API key not set; skipping"),
                    }
                }
                LlmProviderConfig::OpenAi { api_key_env, organization_env, base } => {
                    match std::env::var(api_key_env) {
                        Ok(key) if !key.is_empty() => {
                            let mut p = forge_llm::openai::OpenAiProvider::new(key);
                            if let Some(b) = base { p = p.with_base(b.clone()); }
                            if let Some(org_env) = organization_env {
                                if let Ok(org) = std::env::var(org_env) {
                                    if !org.is_empty() { p = p.with_organization(org); }
                                }
                            }
                            providers.push(Arc::new(p));
                        }
                        _ => tracing::warn!(env = %api_key_env, "OpenAI API key not set; skipping"),
                    }
                }
                LlmProviderConfig::Groq { api_key_env } => {
                    match std::env::var(api_key_env) {
                        Ok(key) if !key.is_empty() => {
                            providers.push(Arc::new(forge_llm::groq::GroqProvider::new(key)));
                        }
                        _ => tracing::warn!(env = %api_key_env, "Groq API key not set; skipping"),
                    }
                }
                LlmProviderConfig::Ollama { base } => {
                    providers.push(Arc::new(forge_llm::ollama::OllamaProvider::new(base.clone())));
                }
            }
        }
        if providers.is_empty() {
            tracing::warn!("no LLM providers available; planner calls will fail");
        }
        let llm_provider_count = providers.len();
        let llm = Arc::new(LlmRouter::new(providers, RoutingStrategy::FailoverInOrder));

        // Feature flags (best-effort; missing file → defaults).
        let flags_path = config.db_path.parent()
            .map(|p| p.join("feature-flags.toml"))
            .unwrap_or_else(|| PathBuf::from("feature-flags.toml"));
        let flags = FeatureFlags::load(&flags_path);
        tracing::info!(
            materializer = flags.materializer.enabled,
            episodic_recall = flags.episodic_recall.enabled,
            cost_summary = flags.cost_summary.enabled,
            path = %flags_path.display(),
            "feature flags loaded"
        );

        // Wire an event-bus sink into the LLM router so every LLM call emits
        // LlmRequested / LlmResponded / LlmFailed events for observability.
        // Skipped if there are no providers — nothing to observe.
        if llm_provider_count > 0 {
            llm.set_event_sink(Some(Arc::new(EventBusLlmSink { events: events.clone() })));
        }

        // Planner + execution.
        let planner = Arc::new(Planner::new(llm.clone(), config.llm.model.clone()));

        // Materializer: only wire in when we actually have LLM providers AND
        // the feature flag is on. Without providers there's nothing to call;
        // falling back to plan-time inputs is the right behaviour.
        let materializer: Option<Arc<dyn TaskInputMaterializer>> = if llm_provider_count > 0 && flags.materializer.enabled {
            Some(Arc::new(PlannerMaterializer {
                planner:  planner.clone(),
                missions: missions_repo.clone(),
                tools:    tools.clone(),
            }))
        } else {
            None
        };

        // Reflector (only if we have at least one LLM provider).
        let reflector: Option<Arc<Reflector>> = if llm_provider_count > 0 {
            Some(Arc::new(Reflector::new(llm.clone(), config.llm.model.clone())))
        } else {
            None
        };

        // Skill registry — active skills only for the runtime.
        let skills_root = config.skills_root.clone()
            .unwrap_or_else(|| config.workspace_root.join("..").join("skills"));
        let loader = FilesystemSkillLoader::new(skills_root.clone());
        let skill_vec = loader.load_all().unwrap_or_default();
        tracing::info!(count = skill_vec.len(), root = %skills_root.display(), "loaded skills");
        let skills = Arc::new(SkillRegistry::new(skill_vec));

        let proposal_writer = Arc::new(ProposalWriter::new(skills_root.clone()));

        // Project memory — read once at boot from the workspace.
        let project_memory_obj = ProjectMemory::load(&config.workspace_root);
        let user_memory_obj = UserMemory::load(config.db_path.parent());
        let project_memory: Option<Arc<str>> = match (&project_memory_obj, &user_memory_obj) {
            (Some(p), Some(u)) => Some(Arc::from(format!(
                "{}\n\n---\n\n{}",
                u.to_prompt_section(),
                p.to_prompt_section(),
            ).as_str())),
            (Some(p), None) => Some(Arc::from(p.to_prompt_section().as_str())),
            (None, Some(u)) => Some(Arc::from(u.to_prompt_section().as_str())),
            (None, None) => None,
        };
        if let Some(mem) = &project_memory {
            tracing::info!(
                bytes = mem.len(),
                project = project_memory_obj.is_some(),
                user = user_memory_obj.is_some(),
                "memory loaded"
            );
        }

        let reflections_repo = Arc::new(SqliteReflectionRepository::new(pool.clone()));

        // Episodic recall wire-up.
        let episodic_recall: Option<Arc<dyn forge_mission::EpisodicRecall>> =
            if flags.episodic_recall.enabled {
                Some(Arc::new(RuntimeEpisodicRecall {
                    missions_repo: missions_repo.clone(),
                    reflections_repo: reflections_repo.clone(),
                    max_recall: flags.episodic_recall.max_recall,
                }))
            } else { None };

        let exec_deps = ExecutionDeps {
            missions:  missions_repo.clone(),
            goals:     goals_repo.clone(),
            tasks:     tasks_repo.clone(),
            events:    events.clone(),
            policy:    policy.clone(),
            tools:     tools.clone(),
            workspace: config.workspace_root.clone(),
            max_parallel_goals: config.max_parallel_goals,
            materializer,
        };
        let execution = ExecutionEngine::new(exec_deps);

        let missions = MissionService {
            missions: missions_repo,
            goals:    goals_repo.clone(),
            tasks:    tasks_repo.clone(),
            events:   events.clone(),
            planner,
            execution,
            tools:    tools.clone(),
            skills:   skills.clone(),
            learning: LearningDeps {
                reflector,
                proposal_writer: Some(proposal_writer),
                reflections: reflections_repo,
            },
            project_memory,
            llm_router: if flags.cost_summary.enabled { Some(llm.clone()) } else { None },
            episodic_recall,
        };

        tracing::info!("forge runtime booted");
        // Boot the shadow-git checkpoint store rooted at the workspace.
        // Sibling to the SQLite file so it lives with the rest of app-data.
        let checkpoints_dir = config.db_path.parent()
            .map(|p| p.join("checkpoints").join(".git"))
            .unwrap_or_else(|| std::path::PathBuf::from(".forge-shadow"));
        let checkpoints = CheckpointStore::init(config.workspace_root.clone(), checkpoints_dir);
        if checkpoints.is_enabled() {
            tracing::info!("shadow-git checkpoints enabled");
            // Auto-snapshot on every TaskCompleted event whose tool touches
            // the filesystem. Fire-and-forget — checkpoint failures never
            // block task execution.
            let cp = checkpoints.clone();
            let mut rx = events.subscribe();
            let events_ck = events.clone();
            let goals_repo_ck = goals_repo.clone();
            let tasks_repo_ck = tasks_repo.clone();
            tokio::spawn(async move {
                use forge_domain::ForgeEvent;
                use std::str::FromStr;
                loop {
                    match rx.recv().await {
                        Ok(env) => {
                            let (task_id, tool, path_hint) = match &env.event {
                                ForgeEvent::TaskCompleted { id, .. } => {
                                    // Look up the task to get its tool name and mission.
                                    let t = match tasks_repo_ck.get(*id).await {
                                        Ok(t) => t, Err(_) => continue,
                                    };
                                    let path_hint = t.input.get("path")
                                        .and_then(|v| v.as_str()).map(String::from);
                                    (Some(*id), t.tool, path_hint)
                                }
                                _ => continue,
                            };
                            if !is_mutating_tool(&tool) { continue; }
                            // Look up mission_id via the goal that owns the task.
                            let mission_id = match task_id {
                                Some(tid) => {
                                    match tasks_repo_ck.get(tid).await {
                                        Ok(t) => match goals_repo_ck.get(t.goal_id).await {
                                            Ok(g) => Some(g.mission_id.to_string()),
                                            Err(_) => None,
                                        },
                                        Err(_) => None,
                                    }
                                }
                                None => None,
                            };
                            let label = format!("{}: {}", tool, path_hint.as_deref().unwrap_or("<workspace>"));
                            let mid_parsed = mission_id.as_deref().and_then(|s| forge_domain::MissionId::from_str(s).ok());
                            match cp.commit(&label, mission_id.as_deref(), task_id.map(|t| t.to_string()).as_deref(), Some(&tool)).await {
                                Ok(Some(sha)) => {
                                    let short = sha.chars().take(7).collect::<String>();
                                    let _ = events_ck.publish(ForgeEvent::CheckpointCreated {
                                        sha,
                                        short_sha: short,
                                        tool: tool.clone(),
                                        mission_id: mid_parsed,
                                        task_id,
                                        label,
                                    }).await;
                                }
                                Ok(None)       => {
                                    // No workspace changes — surface this so
                                    // the UI can show "no-op" feedback instead
                                    // of silently swallowing the attempt.
                                    tracing::info!(tool = %tool, "checkpoint skipped: no changes");
                                    let _ = events_ck.publish(ForgeEvent::CheckpointSkipped {
                                        tool: tool.clone(),
                                        mission_id: mid_parsed,
                                        task_id,
                                        reason: "no workspace changes to commit".into(),
                                    }).await;
                                }
                                Err(e)         => tracing::warn!(err = %e, tool = %tool, "checkpoint failed"),
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(dropped = n, "checkpoint bus lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }

        Ok(Self { config, pool, events, missions, tools, llm, mcp, checkpoints, goals: goals_repo, tasks: tasks_repo })
    }
}

/// Tools whose successful execution can mutate the workspace. Used by the
/// checkpoint background task to decide when to snapshot.
///
/// The forge tool registry uses dotted names — see `crates/forge-tools/src/builtins.rs`
/// (`fs.read`, `fs.write`, `fs.mkdir`, `fs.list`, `shell.run`) and
/// `crates/forge-mcp/src/adapter.rs` (`mcp.{server}.{tool}`). We match those,
/// not the historical `file_write` / `create_directory` shape used elsewhere
/// in agent frameworks.
fn is_mutating_tool(name: &str) -> bool {
    // Local tools.
    if matches!(name, "fs.write" | "fs.mkdir" | "fs.append" | "fs.delete" | "fs.move") {
        return true;
    }
    // shell.run can do anything; assume mutating so users can revert its output.
    if name == "shell.run" { return true; }
    // MCP filesystem tools — namespaced as `mcp.<server>.<remote>`.
    if name.starts_with("mcp.") {
        let lower = name.to_ascii_lowercase();
        // "write_file", "create_directory", "edit_file", "move_file", "delete_file",
        // "append_file", etc. — any tool whose remote name contains a mutating verb.
        if lower.contains("write")
            || lower.contains("create_directory")
            || lower.contains("edit")
            || lower.contains("move")
            || lower.contains("delete")
            || lower.contains("append")
            || lower.contains("mkdir")
            || lower.contains("rename")
        {
            return true;
        }
    }
    false
}

/// Convenience: install a JSON tracing subscriber if the caller hasn't yet.
pub fn install_tracing_default() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("forge_=info,warn")))
        .try_init();
}
