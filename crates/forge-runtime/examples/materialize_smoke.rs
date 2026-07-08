//! End-to-end smoke test for just-in-time task input materialization.
//!
//! Runs a mission that requires the write step to consume the list step's
//! output ("List directories then write them to summary.md"). Without the
//! materializer, the planner emits a placeholder for the write step's
//! `content` arg. With the materializer, the placeholder is replaced with
//! the real directory listing at execution time.
//!
//! Requires GROQ_API_KEY. Uses a fresh workspace + DB so it doesn't
//! interfere with the running Tauri app.
//!
//! Run:
//!   cargo run -p forge-runtime --example materialize_smoke

use forge_domain::{ForgeEvent, MissionStatus};
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

    // Fresh isolated workspace.
    let tmp = std::env::temp_dir().join(format!(
        "forge-materialize-smoke-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
    ));
    let workspace = tmp.join("workspace");
    let db_path = tmp.join("forge.sqlite");
    std::fs::create_dir_all(&workspace)?;
    for d in ["src", "docs", "tests"] {
        std::fs::create_dir_all(workspace.join(d))?;
    }
    std::fs::write(workspace.join("README.md"), "# smoke test project\n")?;

    println!("workspace: {}", workspace.display());
    println!("db:        {}", db_path.display());

    let model = std::env::var("FORGE_LLM_MODEL")
        .unwrap_or_else(|_| "llama-3.3-70b-versatile".to_string());

    let config = RuntimeConfig {
        workspace_root: workspace.clone(),
        db_path: db_path.clone(),
        policy_path: None,
        llm: LlmConfig {
            providers: vec![LlmProviderConfig::Groq {
                api_key_env: "GROQ_API_KEY".to_string(),
            }],
            model,
        },
        max_parallel_goals: 2,
        skills_root: Some(tmp.join("skills")),
        mcp_config:  Some(tmp.join("mcp.yaml")),
        auto_promote_skills: false,
        autopromote_interval_secs: 300, // missing file → skipped
    };

    let runtime = Runtime::boot(config).await?;
    let mut rx = runtime.events.subscribe();

    let mission_id = runtime
        .missions
        .create(
            "materialize smoke".to_string(),
            "List the top-level directories of the workspace (using fs.list on \".\") and \
             then write the actual comma-separated directory names to a file at \
             SUMMARY.md at the workspace root. The written content must contain the \
             real directory names, not a placeholder."
                .to_string(),
        )
        .await?;
    println!("created mission {mission_id}");

    runtime.missions.plan_and_run(mission_id).await?;

    // Wait for terminal status or timeout.
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    let mut refresh_count = 0usize;
    let mut final_status: Option<MissionStatus> = None;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Ok(env)) => match env.event {
                ForgeEvent::TaskInputRefreshed { task_id, tool } => {
                    refresh_count += 1;
                    println!("  ✎ task {task_id} tool={tool} input refreshed");
                }
                ForgeEvent::MissionStatusChanged { id, to, .. } if id == mission_id => {
                    println!("  → mission → {:?}", to);
                    if matches!(
                        to,
                        MissionStatus::Completed | MissionStatus::Failed | MissionStatus::Cancelled
                    ) {
                        final_status = Some(to);
                        break;
                    }
                }
                _ => {}
            },
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => break,
            Err(_) => {} // timeout on recv; keep waiting until deadline
        }
    }

    let summary_path = workspace.join("SUMMARY.md");
    let summary = std::fs::read_to_string(&summary_path).unwrap_or_default();
    println!("---");
    println!("mission status: {:?}", final_status);
    println!("input refreshes: {refresh_count}");
    println!("SUMMARY.md ({}b):", summary.len());
    for line in summary.lines().take(10) {
        println!("  {line}");
    }
    println!("---");

    let mut ok = true;
    if final_status != Some(MissionStatus::Completed) {
        eprintln!("FAIL: mission did not reach Completed");
        ok = false;
    }
    let lc = summary.to_lowercase();
    let placeholder_markers = [
        "[insert",
        "<see prior",
        "<placeholder",
        "todo:",
        "{{",
        "$(",
    ];
    for m in placeholder_markers {
        if lc.contains(m) {
            eprintln!("FAIL: SUMMARY.md still contains placeholder marker `{m}`");
            ok = false;
        }
    }
    // At least one of the seeded directories should be named.
    if !(lc.contains("src") || lc.contains("docs") || lc.contains("tests")) {
        eprintln!("FAIL: SUMMARY.md doesn't name any seeded directory");
        ok = false;
    }

    // Cleanup temp workspace (best-effort).
    let _ = std::fs::remove_dir_all(&tmp);

    if ok {
        println!("PASS: materializer end-to-end verified");
        Ok(())
    } else {
        std::process::exit(1);
    }
}
