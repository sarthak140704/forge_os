//! LLM-free headless smoke: verify auto-checkpoint end-to-end via direct
//! event injection. Bypasses the planner (no Groq tokens burned).
//!
//! Flow per scenario:
//!   1. Create a mission via MissionService (DB insert only, no LLM).
//!   2. Insert a synthetic goal + task with tool="fs.write" through the
//!      now-public `goals` / `tasks` repositories.
//!   3. Write bytes directly to workspace_root/<filename>.
//!   4. Publish `ForgeEvent::TaskCompleted { id: <task_id> }` through
//!      `runtime.events`.
//!   5. Wait for either CheckpointCreated or CheckpointSkipped and assert.
//!
//! Scenarios:
//!   A. Write "alpha\n"   → expect CheckpointCreated (new content in shadow)
//!   B. Write "beta\n"    → expect CheckpointCreated (new content in shadow)
//!   C. Write "beta\n"    → expect CheckpointSkipped (identical bytes)
//!
//! Run:   cargo run -p forge-runtime --example checkpoints_headless_smoke

use forge_domain::{ForgeEvent, Goal, GoalStatus, Mission, MissionId, MissionStatus, Task, TaskStatus};
use forge_persistence::{GoalRepository, TaskRepository};
use forge_runtime::{LlmConfig, LlmProviderConfig, Runtime, RuntimeConfig};
use serde_json::json;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("forge_=info,warn")),
        )
        .try_init()
        .ok();

    let tmp = std::env::temp_dir().join(format!(
        "forge-cp-headless-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis()
    ));
    let workspace = tmp.join("workspace");
    let db_path = tmp.join("forge.sqlite");
    std::fs::create_dir_all(&workspace)?;

    // We still need a valid LlmProviderConfig (Runtime::boot requires at least one),
    // but nothing in this test actually invokes it — the LLM router is unused.
    // Use a dummy env var that won't be read.
    unsafe { std::env::set_var("__FORGE_DUMMY_KEY__", "unused"); }
    let config = RuntimeConfig {
        workspace_root: workspace.clone(),
        db_path: db_path.clone(),
        policy_path: None,
        llm: LlmConfig {
            providers: vec![LlmProviderConfig::Groq { api_key_env: "__FORGE_DUMMY_KEY__".into() }],
            model: "unused-model".into(),
        },
        max_parallel_goals: 2,
        skills_root: Some(tmp.join("skills")),
        mcp_config:  Some(tmp.join("mcp.yaml")),
        auto_promote_skills: false,
        autopromote_interval_secs: 300,
        curator: Default::default(),
        curator_sweep_enabled: false,
        curator_interval_secs: 900,
    };
    let runtime = Runtime::boot(config).await?;

    // Give the shadow-git init and its subscriber a moment to start.
    tokio::time::sleep(Duration::from_millis(250)).await;

    let scenarios: Vec<(&str, &str, &[u8], Expect)> = vec![
        ("A: first write",             "cp-a.txt", b"alpha\n", Expect::Created),
        ("B: second write, new file",  "cp-b.txt", b"beta\n",  Expect::Created),
        ("C: dup write, same bytes",   "cp-b.txt", b"beta\n",  Expect::Skipped),
    ];

    let mut all_ok = true;
    for (i, (label, filename, bytes, expect)) in scenarios.iter().enumerate() {
        println!("\n=== scenario {}: {label} ===", i + 1);
        let ok = run_one(&runtime, &workspace, filename, bytes, *expect).await?;
        if !ok { all_ok = false; }
    }

    let _ = std::fs::remove_dir_all(&tmp);
    if all_ok { println!("\nPASS: CheckpointCreated + CheckpointSkipped verified end-to-end"); Ok(()) }
    else       { eprintln!("\nFAIL"); std::process::exit(1) }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Expect { Created, Skipped }

async fn run_one(
    runtime: &Runtime,
    workspace: &std::path::Path,
    filename: &str,
    bytes: &[u8],
    expect: Expect,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut rx = runtime.events.subscribe();

    // 1. Create mission (DB only, no LLM).
    let mission = Mission::new_draft(format!("headless-{filename}"), "synthetic");
    let mid: MissionId = mission.id;
    runtime.missions.missions.insert(&mission).await?;

    // 2. Insert synthetic goal + task.
    let goal = Goal::new(mid, "synthetic goal", "", Vec::new());
    let gid = goal.id;
    runtime.goals.insert(&goal).await?;

    let task = Task::new(gid, "fs.write", json!({
        "path": workspace.join(filename).to_string_lossy(),
        "content": String::from_utf8_lossy(bytes),
    }));
    let tid = task.id;
    runtime.tasks.insert(&task).await?;

    // 3. Write to workspace directly (simulating what fs.write would have done).
    let target = workspace.join(filename);
    std::fs::write(&target, bytes)?;

    // 4. Publish TaskCompleted to trigger the auto-checkpoint spawn.
    runtime.events.publish(ForgeEvent::TaskCompleted {
        id: tid,
        result_summary: format!(r#"{{"bytes":{},"path":"{}"}}"#, bytes.len(), target.display()),
    }).await?;

    // 5. Wait up to 6s for a CheckpointCreated or CheckpointSkipped for our task.
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    let mut observed: Option<Observed> = None;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Ok(env)) => match env.event {
                ForgeEvent::CheckpointCreated { short_sha, tool, task_id, label, .. } => {
                    if task_id == Some(tid) {
                        println!("  ✓ CheckpointCreated {short_sha} ({tool}) — {label}");
                        observed = Some(Observed::Created); break;
                    }
                }
                ForgeEvent::CheckpointSkipped { tool, task_id, reason, .. } => {
                    if task_id == Some(tid) {
                        println!("  ⊘ CheckpointSkipped ({tool}) — {reason}");
                        observed = Some(Observed::Skipped); break;
                    }
                }
                _ => {}
            },
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => break,
            Err(_) => {} // recv timeout
        }
    }

    let ok = match (expect, observed) {
        (Expect::Created, Some(Observed::Created)) => true,
        (Expect::Skipped, Some(Observed::Skipped)) => true,
        (want, got) => {
            eprintln!("  FAIL: expected {want:?}, got {got:?}");
            false
        }
    };
    // Mark mission completed so shadow-git doesn't complain about pending work.
    let mut m = runtime.missions.missions.get(mid).await?;
    m.status = MissionStatus::Completed;
    runtime.missions.missions.update(&m).await?;
    let mut g = runtime.goals.get(gid).await?;
    g.status = GoalStatus::Completed;
    runtime.goals.update(&g).await?;
    let mut t = runtime.tasks.get(tid).await?;
    t.status = TaskStatus::Completed;
    runtime.tasks.update(&t).await?;
    Ok(ok)
}

#[derive(Debug, PartialEq, Eq)]
enum Observed { Created, Skipped }
