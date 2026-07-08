//! Smoke test: auto-checkpoint fires on mutating tool completions.
//!
//! Runs two write missions end-to-end (distinct file contents) and asserts
//! that each triggers a `CheckpointCreated` event.  Then repeats the same
//! write to confirm the second run is a no-op (git detects no changes), and
//! reports whether that no-op is visible to the UI or silently swallowed.
//!
//! Requires GROQ_API_KEY.
//! Run:   cargo run -p forge-runtime --example checkpoints_smoke

use forge_domain::{ForgeEvent, MissionId, MissionStatus};
use forge_runtime::{LlmConfig, LlmProviderConfig, Runtime, RuntimeConfig};
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

    if std::env::var("GROQ_API_KEY").ok().filter(|k| !k.is_empty()).is_none() {
        eprintln!("GROQ_API_KEY not set; aborting");
        std::process::exit(2);
    }

    let tmp = std::env::temp_dir().join(format!(
        "forge-cp-smoke-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis()
    ));
    let workspace = tmp.join("workspace");
    let db_path = tmp.join("forge.sqlite");
    std::fs::create_dir_all(&workspace)?;

    let model = std::env::var("FORGE_LLM_MODEL").unwrap_or_else(|_| "llama-3.3-70b-versatile".into());

    let config = RuntimeConfig {
        workspace_root: workspace.clone(),
        db_path: db_path.clone(),
        policy_path: None,
        llm: LlmConfig {
            providers: vec![LlmProviderConfig::Groq { api_key_env: "GROQ_API_KEY".into() }],
            model,
        },
        max_parallel_goals: 2,
        skills_root: Some(tmp.join("skills")),
        mcp_config:  Some(tmp.join("mcp.yaml")),
        auto_promote_skills: false,
        autopromote_interval_secs: 300,
    };
    let runtime = Runtime::boot(config).await?;

    // Missions to run (title, prompt). Each prompt asks the planner to
    // emit exactly one fs.write task with the given content.
    let scenarios: Vec<(&str, &str)> = vec![
        ("cp-A", "Write the exact text 'checkpoint-alpha\\n' to a file named cp-a.txt at the workspace root. Do not read anything first."),
        ("cp-B", "Write the exact text 'checkpoint-beta\\n' to a file named cp-b.txt at the workspace root. Do not read anything first."),
        ("cp-B-dup", "Write the exact text 'checkpoint-beta\\n' to a file named cp-b.txt at the workspace root. Do not read anything first."),
    ];

    let mut all_ok = true;
    for (i, (title, prompt)) in scenarios.iter().enumerate() {
        println!("\n=== scenario {}: {title} ===", i + 1);
        let expect_checkpoint = i < 2; // third run should be a no-op (same bytes)
        let ok = run_one(&runtime, title, prompt, expect_checkpoint).await?;
        if !ok { all_ok = false; }
    }

    let _ = std::fs::remove_dir_all(&tmp);
    if all_ok { println!("\nPASS: auto-checkpoint flow verified end-to-end"); Ok(()) }
    else       { eprintln!("\nFAIL"); std::process::exit(1) }
}

async fn run_one(
    runtime: &Runtime,
    title: &str,
    prompt: &str,
    expect_checkpoint: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut rx = runtime.events.subscribe();
    let mid: MissionId = runtime.missions.create(title.into(), prompt.into()).await?;
    println!("  mission {mid}");
    runtime.missions.plan_and_run(mid).await?;

    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    let mut task_completed = false;
    let mut tool_seen: Option<String> = None;
    let mut checkpoint_seen: Option<(String, String, String)> = None;
    let mut skip_seen: Option<(String, String)> = None;
    let mut final_status: Option<MissionStatus> = None;

    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Ok(env)) => match env.event {
                ForgeEvent::ToolInvoked { tool, .. } => {
                    // Doesn't carry mission_id; we rely on task_completed for correlation.
                    tool_seen = Some(tool);
                }
                ForgeEvent::TaskCompleted { .. } => { task_completed = true; }
                ForgeEvent::CheckpointCreated { short_sha, tool, mission_id, label, .. } => {
                    if mission_id == Some(mid) {
                        println!("  ✓ checkpoint {short_sha} ({tool}) — {label}");
                        checkpoint_seen = Some((short_sha, tool, label));
                    }
                }
                ForgeEvent::CheckpointSkipped { tool, mission_id, reason, .. } => {
                    if mission_id == Some(mid) {
                        println!("  ⊘ checkpoint skipped ({tool}) — {reason}");
                        skip_seen = Some((tool, reason));
                    }
                }
                ForgeEvent::MissionStatusChanged { id, to, .. } if id == mid => {
                    if matches!(to, MissionStatus::Completed | MissionStatus::Failed | MissionStatus::Cancelled) {
                        final_status = Some(to);
                    }
                }
                _ => {}
            },
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => break,
            Err(_) => {}
        }
        // Give the auto-snapshot task a chance to publish AFTER terminal.
        if final_status.is_some() && task_completed {
            // small grace period so the checkpoint spawn can run
            tokio::time::sleep(Duration::from_millis(600)).await;
            // drain any late events without blocking
            while let Ok(Ok(env)) = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
                match env.event {
                    ForgeEvent::CheckpointCreated { short_sha, tool, mission_id, label, .. } => {
                        if mission_id == Some(mid) {
                            println!("  ✓ checkpoint {short_sha} ({tool}) — {label}");
                            checkpoint_seen = Some((short_sha, tool, label));
                        }
                    }
                    ForgeEvent::CheckpointSkipped { tool, mission_id, reason, .. } => {
                        if mission_id == Some(mid) {
                            println!("  ⊘ checkpoint skipped ({tool}) — {reason}");
                            skip_seen = Some((tool, reason));
                        }
                    }
                    _ => {}
                }
            }
            break;
        }
    }

    println!("  status={final_status:?} tool={tool_seen:?} checkpoint={} skipped={}",
        checkpoint_seen.is_some(), skip_seen.is_some());

    let mut ok = true;
    if !matches!(final_status, Some(MissionStatus::Completed)) {
        eprintln!("  FAIL: mission not completed");
        ok = false;
    }
    if expect_checkpoint && checkpoint_seen.is_none() {
        eprintln!("  FAIL: expected CheckpointCreated event, got none");
        ok = false;
    }
    if !expect_checkpoint {
        if checkpoint_seen.is_some() {
            eprintln!("  NOTE: expected no-op (identical bytes) but a checkpoint fired anyway — LLM must have changed something.");
        } else if skip_seen.is_none() {
            eprintln!("  FAIL: expected CheckpointSkipped event for no-op run, got neither");
            ok = false;
        }
    }
    Ok(ok)
}
