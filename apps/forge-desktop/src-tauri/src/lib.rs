//! Tauri v2 shell for Forge OS.
//!
//! Responsibilities:
//!   * Boot the runtime (persistence + LLM + tools + policy + planner + executor).
//!   * Expose a small IPC surface for the webview.
//!   * Bridge `ForgeEvent`s from the in-process broadcast bus to the webview
//!     as `forge://event`.

use forge_domain::{ForgeEvent, MissionId, MissionSummary, TaskId};
use forge_events::EventBus;
use forge_mission::{MissionDetail, MissionService};
use forge_runtime::{
    skills_ops::{Curator, CuratorReport, CuratorSuggestion, SkillOps},
    Checkpoint, CheckpointStore, LlmConfig, LlmProviderConfig, Runtime, RuntimeConfig,
};
use serde::Serialize;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};

/// Handle stored in Tauri managed state.
struct AppState {
    missions: MissionService,
    events: EventBus,
    /// Root of the skills tree (contains `active/` and `proposed/`).
    skills_root: PathBuf,
    checkpoints: CheckpointStore,
    pool: sqlx::SqlitePool,
    skill_ops: Option<Arc<SkillOps>>,
    curator:   Option<Arc<Curator>>,
}

// ---------------------------------------------------------------------------
// IPC commands
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct IpcMissionId { id: String }

#[tauri::command]
async fn create_mission(
    state: State<'_, Arc<AppState>>,
    title: String,
    description: String,
) -> Result<IpcMissionId, String> {
    let id = state.missions.create(title, description).await.map_err(|e| e.to_string())?;
    Ok(IpcMissionId { id: id.to_string() })
}

#[tauri::command]
async fn plan_and_run(
    state: State<'_, Arc<AppState>>,
    mission_id: String,
) -> Result<(), String> {
    let id = MissionId::from_str(&mission_id).map_err(|e| e.to_string())?;
    state.missions.plan_and_run(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn cancel_mission(
    state: State<'_, Arc<AppState>>,
    mission_id: String,
) -> Result<(), String> {
    let id = MissionId::from_str(&mission_id).map_err(|e| e.to_string())?;
    state.missions.cancel(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn extend_mission(
    state: State<'_, Arc<AppState>>,
    mission_id: String,
    prompt: String,
) -> Result<(), String> {
    let id = MissionId::from_str(&mission_id).map_err(|e| e.to_string())?;
    state.missions.extend(id, prompt).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn approve_task(
    state: State<'_, Arc<AppState>>,
    task_id: String,
) -> Result<(), String> {
    let id = TaskId::from_str(&task_id).map_err(|e| e.to_string())?;
    state.missions.approve_task(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_missions(state: State<'_, Arc<AppState>>) -> Result<Vec<MissionSummary>, String> {
    state.missions.list().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_mission(
    state: State<'_, Arc<AppState>>,
    mission_id: String,
) -> Result<MissionDetail, String> {
    let id = MissionId::from_str(&mission_id).map_err(|e| e.to_string())?;
    state.missions.detail(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn replay_events(
    state: State<'_, Arc<AppState>>,
    since: Option<i64>,
) -> Result<Vec<forge_domain::EventEnvelope>, String> {
    let cutoff = since.map(forge_domain::EventId);
    state.events.replay_since(cutoff).await.map_err(|e| e.to_string())
}

/// Returns Ok(()) once the runtime is booted (`AppState` is managed).
/// Any command that requires `State<'_, Arc<AppState>>` would 500 before
/// this, so the frontend can poll this on mount to bootstrap its ready flag
/// even when it missed the `forge://runtime-ready` broadcast event.
#[tauri::command]
async fn runtime_status(_state: State<'_, Arc<AppState>>) -> Result<(), String> {
    Ok(())
}

// ---- Skill proposal review IPC ----

#[derive(Serialize)]
struct SkillProposalSummary {
    name: String,
    version: String,
    description: String,
    filename: String,
}

#[tauri::command]
async fn list_skill_proposals(state: State<'_, Arc<AppState>>) -> Result<Vec<SkillProposalSummary>, String> {
    let proposals = forge_skills::proposal::list_proposals(&state.skills_root).map_err(|e| e.to_string())?;
    Ok(proposals.into_iter().map(|s| SkillProposalSummary {
        name: s.front.name,
        version: s.front.version,
        description: s.front.description,
        filename: std::path::Path::new(&s.source_path)
            .file_name().and_then(|n| n.to_str()).unwrap_or("").to_string(),
    }).collect())
}

#[tauri::command]
async fn approve_skill_proposal(state: State<'_, Arc<AppState>>, filename: String) -> Result<String, String> {
    forge_skills::proposal::approve_proposal(&state.skills_root, &filename)
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn reject_skill_proposal(state: State<'_, Arc<AppState>>, filename: String) -> Result<(), String> {
    forge_skills::proposal::reject_proposal(&state.skills_root, &filename).map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_reflections(
    state: State<'_, Arc<AppState>>,
    mission_id: String,
) -> Result<Vec<forge_persistence::ReflectionRecord>, String> {
    let id = MissionId::from_str(&mission_id).map_err(|e| e.to_string())?;
    state.missions.reflections(id).await.map_err(|e| e.to_string())
}

// ---- Phase 3: shadow-git checkpoints, secrets, audit export ----

#[tauri::command]
async fn list_checkpoints(
    state: State<'_, Arc<AppState>>,
    mission_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<Checkpoint>, String> {
    state.checkpoints
        .list(limit.unwrap_or(50), mission_id.as_deref())
        .await
}

#[tauri::command]
async fn revert_checkpoint(
    state: State<'_, Arc<AppState>>,
    sha: String,
) -> Result<(), String> {
    state.checkpoints.revert(&sha).await
}

#[derive(Serialize)]
struct SecretStatus { name: String, set: bool, source: &'static str }

#[tauri::command]
async fn list_secret_status(_state: State<'_, Arc<AppState>>) -> Result<Vec<SecretStatus>, String> {
    let mut out = Vec::new();
    for name in forge_runtime::secrets::KNOWN_SECRETS {
        let env_set = std::env::var(name).map(|v| !v.trim().is_empty()).unwrap_or(false);
        let kr_set  = forge_runtime::secrets::has(name);
        let (set, source) = if env_set { (true, "env") }
                            else if kr_set { (true, "keyring") }
                            else { (false, "unset") };
        out.push(SecretStatus { name: name.to_string(), set, source });
    }
    Ok(out)
}

#[tauri::command]
async fn set_secret(_state: State<'_, Arc<AppState>>, name: String, value: String) -> Result<(), String> {
    forge_runtime::secrets::set(&name, &value)
}

#[tauri::command]
async fn delete_secret(_state: State<'_, Arc<AppState>>, name: String) -> Result<(), String> {
    forge_runtime::secrets::delete(&name)
}

#[derive(Serialize)]
struct AuditExportResult {
    path:        String,
    missions:    usize,
    goals:       usize,
    tasks:       usize,
    events:      usize,
    reflections: usize,
}

#[tauri::command]
async fn export_audit(
    state: State<'_, Arc<AppState>>,
    dest: String,
) -> Result<AuditExportResult, String> {
    let path = std::path::PathBuf::from(&dest);
    let counts = forge_runtime::audit::write_to(&state.pool, &path).await?;
    Ok(AuditExportResult {
        path: dest,
        missions: counts.missions,
        goals: counts.goals,
        tasks: counts.tasks,
        events: counts.events,
        reflections: counts.reflections,
    })
}

// ---- Phase 4a: version-controlled skills ----

#[derive(Serialize)]
struct SkillVersionDto {
    name:              String,
    sha:               String,
    version:           String,
    origin:            String,
    origin_mission_id: Option<String>,
    parent_sha:        Option<String>,
    promoted_at:       String,
    retired_at:        Option<String>,
    reason:            Option<String>,
}

impl From<forge_persistence::SkillVersionRecord> for SkillVersionDto {
    fn from(r: forge_persistence::SkillVersionRecord) -> Self {
        Self {
            name: r.name,
            sha: r.sha,
            version: r.version,
            origin: match r.origin {
                forge_persistence::SkillOrigin::Proposal   => "proposal",
                forge_persistence::SkillOrigin::Handcrafted => "handcrafted",
                forge_persistence::SkillOrigin::Rollback   => "rollback",
                forge_persistence::SkillOrigin::Curated    => "curated",
            }.into(),
            origin_mission_id: r.origin_mission_id,
            parent_sha: r.parent_sha,
            promoted_at: r.promoted_at,
            retired_at: r.retired_at,
            reason: r.reason,
        }
    }
}

fn require_skill_ops(state: &AppState) -> Result<Arc<SkillOps>, String> {
    state.skill_ops.clone().ok_or_else(|| "skills subsystem is not configured (skills_root missing)".to_string())
}

fn require_curator(state: &AppState) -> Result<Arc<Curator>, String> {
    state.curator.clone().ok_or_else(|| "curator is not configured (skills_root missing)".to_string())
}

#[tauri::command]
async fn list_active_skills(state: State<'_, Arc<AppState>>) -> Result<Vec<SkillVersionDto>, String> {
    let ops = require_skill_ops(&state)?;
    let rows = ops.history.list_active().await.map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(SkillVersionDto::from).collect())
}

#[tauri::command]
async fn list_skill_versions(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> Result<Vec<SkillVersionDto>, String> {
    let ops = require_skill_ops(&state)?;
    let rows = ops.history.history(&name).await.map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(SkillVersionDto::from).collect())
}

#[tauri::command]
async fn rollback_skill(
    state: State<'_, Arc<AppState>>,
    name: String,
    sha: String,
    reason: Option<String>,
) -> Result<SkillVersionDto, String> {
    let ops = require_skill_ops(&state)?;
    let row = ops.rollback(&name, &sha, reason.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    Ok(SkillVersionDto::from(row))
}

#[tauri::command]
async fn retire_skill(
    state: State<'_, Arc<AppState>>,
    name: String,
    reason: String,
) -> Result<Option<String>, String> {
    let ops = require_skill_ops(&state)?;
    ops.retire(&name, &reason).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn run_curator(state: State<'_, Arc<AppState>>) -> Result<Vec<CuratorSuggestion>, String> {
    let curator = require_curator(&state)?;
    curator.run().await.map_err(|e| e.to_string())
}

/// Phase 4c: full curator scan. `apply=false` returns suggestions only;
/// `apply=true` also archives duplicate losers and drops merge proposals
/// into `proposed/`. Never destructive — archived skills are recoverable
/// via rollback because the content-addressed store keeps every version.
#[tauri::command]
async fn curator_scan(
    state: State<'_, Arc<AppState>>,
    apply: bool,
) -> Result<CuratorReport, String> {
    let curator = require_curator(&state)?;
    curator.scan(apply).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn validate_skill_proposal(
    state: State<'_, Arc<AppState>>,
    filename: String,
) -> Result<forge_skills::ValidationReport, String> {
    let ops = require_skill_ops(&state)?;
    ops.validate_proposal(&filename).await.map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// App entry
// ---------------------------------------------------------------------------

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")))
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            // Boot the runtime asynchronously; UI can render even before it's up.
            tauri::async_runtime::spawn(async move {
                match boot_runtime(&handle).await {
                    Ok(state) => {
                        // Spawn the bridge before we hand state to the app so no
                        // initial events are dropped.
                        spawn_event_bridge(handle.clone(), state.events.clone());
                        handle.manage(state);
                        let _ = handle.emit("forge://runtime-ready", ());
                        tracing::info!("runtime ready, IPC commands live");
                    }
                    Err(e) => {
                        tracing::error!(err = %e, "runtime boot failed");
                        let _ = handle.emit("forge://runtime-error", format!("{e}"));
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            create_mission,
            plan_and_run,
            cancel_mission,
            extend_mission,
            approve_task,
            list_missions,
            get_mission,
            replay_events,
            runtime_status,
            list_skill_proposals,
            approve_skill_proposal,
            reject_skill_proposal,
            list_reflections,
            list_checkpoints,
            revert_checkpoint,
            list_secret_status,
            set_secret,
            delete_secret,
            export_audit,
            list_active_skills,
            list_skill_versions,
            rollback_skill,
            retire_skill,
            run_curator,
            curator_scan,
            validate_skill_proposal,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}

async fn boot_runtime(app: &tauri::AppHandle) -> anyhow::Result<Arc<AppState>> {
    let app_data = app.path().app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."));
    std::fs::create_dir_all(&app_data)?;
    let workspace = app_data.join("workspace");
    let db_path = app_data.join("forge.sqlite");
    let policy_path = app_data.join("policy.yaml");

    // If the user hasn't provided a policy file, ship a permissive default so
    // the UI at least boots. Real deployments will override this.
    if !policy_path.exists() {
        let default = include_str!("../../../../config/policy.default.yaml");
        std::fs::write(&policy_path, default)?;
    }

    let openrouter_key = forge_runtime::secrets::resolve("OPENROUTER_API_KEY");
    let openai_key     = forge_runtime::secrets::resolve("OPENAI_API_KEY");
    let groq_key       = forge_runtime::secrets::resolve("GROQ_API_KEY");
    // Bridge keyring-resolved values into env vars so the LLM providers'
    // env-based key readers pick them up transparently. `set_var` is safe
    // during boot before any thread reads these variables.
    if let Some(v) = openrouter_key.as_deref() { std::env::set_var("OPENROUTER_API_KEY", v); }
    if let Some(v) = openai_key.as_deref()     { std::env::set_var("OPENAI_API_KEY", v); }
    if let Some(v) = groq_key.as_deref()       { std::env::set_var("GROQ_API_KEY", v); }

    // Failover order: OpenRouter → OpenAI → Groq → Ollama. First one with a key wins.
    let mut providers: Vec<LlmProviderConfig> = Vec::new();
    if openrouter_key.is_some() {
        providers.push(LlmProviderConfig::OpenRouter { api_key_env: "OPENROUTER_API_KEY".into() });
    }
    if openai_key.is_some() {
        providers.push(LlmProviderConfig::OpenAi {
            api_key_env: "OPENAI_API_KEY".into(),
            organization_env: Some("OPENAI_ORG_ID".into()),
            base: None,
        });
    }
    if groq_key.is_some() {
        providers.push(LlmProviderConfig::Groq { api_key_env: "GROQ_API_KEY".into() });
    }
    providers.push(LlmProviderConfig::Ollama { base: "http://127.0.0.1:11434".into() });

    // Pick a sensible default model for whichever provider comes first.
    let model = std::env::var("FORGE_MODEL").unwrap_or_else(|_| {
        if openrouter_key.is_some() {
            "openai/gpt-4o-mini".into()
        } else if openai_key.is_some() {
            "gpt-4o-mini".into()
        } else if groq_key.is_some() {
            "llama-3.3-70b-versatile".into()
        } else {
            "llama3.1".into()
        }
    });

    let config = RuntimeConfig {
        workspace_root: workspace,
        db_path,
        policy_path: Some(policy_path),
        llm: LlmConfig { providers, model },
        max_parallel_goals: 4,
        skills_root: Some(app_data.join("skills")),
        mcp_config: Some(app_data.join("mcp.yaml")),
        auto_promote_skills: false,
        autopromote_interval_secs: 300,
        curator: Default::default(),
        curator_sweep_enabled: false,
        curator_interval_secs: 900,
    };
    // On first run, seed the skills dir from the bundled defaults if it's empty.
    let skills_root = app_data.join("skills").join("active");
    if !skills_root.exists() {
        std::fs::create_dir_all(&skills_root).ok();
        // Copy bundled seed skills into the app-data skills dir.
        for (name, body) in SEED_SKILLS {
            let dest = skills_root.join(name);
            if !dest.exists() {
                let _ = std::fs::write(&dest, body);
            }
        }
    }

    // Seed a commented-out MCP config so users can enable servers by
    // uncommenting rather than authoring from scratch.
    let mcp_path = app_data.join("mcp.yaml");
    if !mcp_path.exists() {
        let seed = include_str!("../../../../config/mcp.default.yaml");
        let _ = std::fs::write(&mcp_path, seed);
    }
    let runtime = Runtime::boot(config).await?;
    Ok(Arc::new(AppState {
        missions: runtime.missions,
        events: runtime.events,
        skills_root: app_data.join("skills"),
        checkpoints: runtime.checkpoints,
        pool: runtime.pool,
        skill_ops: runtime.skill_ops,
        curator:   runtime.curator,
    }))
}

const SEED_SKILLS: &[(&str, &str)] = &[
    ("rust-crate.md",     include_str!("../../../../config/skills/active/rust-crate.md")),
    ("node-project.md",   include_str!("../../../../config/skills/active/node-project.md")),
    ("python-project.md", include_str!("../../../../config/skills/active/python-project.md")),
    ("git-repo.md",       include_str!("../../../../config/skills/active/git-repo.md")),
];

fn spawn_event_bridge(app: tauri::AppHandle, bus: EventBus) {
    tauri::async_runtime::spawn(async move {
        let mut rx = bus.subscribe();
        loop {
            match rx.recv().await {
                Ok(envelope) => {
                    let _ = app.emit("forge://event", &envelope);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, "event bridge lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

// Silence unused import warning if the enum is only used in matches.
#[allow(dead_code)]
fn _keep_forge_event(_: ForgeEvent) {}
