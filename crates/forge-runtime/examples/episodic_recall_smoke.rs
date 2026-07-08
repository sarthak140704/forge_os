//! Smoke test: episodic recall injects prior-mission summaries into the
//! planner prompt.
//!
//! Strategy:
//!   1. Boot a runtime with a fresh DB.
//!   2. Directly INSERT a prior completed mission + reflection into SQLite
//!      whose keywords will match the new mission we're about to run.
//!   3. Create + run a new mission with overlapping keywords.
//!   4. Assert that `episodic_recall::build_recall_block` returns Some(_)
//!      containing the seeded mission's title AND that the new mission
//!      completes (the recall shouldn't break anything).
//!
//! We can't easily inspect the actual planner prompt from outside, so the
//! recall_block call is our proxy for the code path being exercised.
//!
//! Requires GROQ_API_KEY.
//! Run:   cargo run -p forge-runtime --example episodic_recall_smoke

use forge_domain::{ForgeEvent, MissionStatus};
use forge_runtime::{
    episodic_recall::build_recall_block, LlmConfig, LlmProviderConfig, Runtime, RuntimeConfig,
};
use std::sync::Arc;
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
        "forge-episodic-smoke-{}",
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
    };
    let runtime = Runtime::boot(config).await?;

    // Seed a prior completed mission with overlapping keywords with what we're
    // about to run. Use the same pool as the runtime.
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "2020-01-01T00:00:00Z".into());
    let prior_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO missions (id, title, description, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'completed', ?4, ?4)",
    )
    .bind(&prior_id)
    .bind("hello file writer previous")
    .bind("Wrote hello.txt with the string hello. Used fs.write tool successfully.")
    .bind(&now)
    .execute(&runtime.pool)
    .await?;
    sqlx::query(
        "INSERT INTO reflections (mission_id, created_at, outcome, payload) VALUES (?1, ?2, 'Completed', ?3)"
    )
    .bind(&prior_id)
    .bind(&now)
    .bind(r#"{"summary":"prior success writing hello.txt","what_worked":["fs.write worked first try"],"what_failed":[]}"#)
    .execute(&runtime.pool)
    .await?;
    println!("seeded prior mission {prior_id}");

    // Build a synthetic current mission (not inserted — we're just testing the
    // recall function directly).
    let current = forge_domain::Mission::new_draft(
        "write hello file".to_string(),
        "Create a file named greeting.txt containing hello".to_string(),
    );
    let missions_repo: Arc<dyn forge_persistence::MissionRepository> = Arc::new(
        forge_persistence::SqliteMissionRepository::new(runtime.pool.clone()),
    );
    let reflections_repo: Arc<dyn forge_persistence::ReflectionRepository> = Arc::new(
        forge_persistence::SqliteReflectionRepository::new(runtime.pool.clone()),
    );
    let block = build_recall_block(&missions_repo, &reflections_repo, &current, 3).await;
    println!("---\nrecall block:\n{}\n---", block.as_deref().unwrap_or("(none)"));

    let mut ok = true;
    match &block {
        Some(b) => {
            if !b.contains("hello file writer previous") {
                eprintln!("FAIL: recall block missing seeded mission title");
                ok = false;
            }
            if !b.contains("Completed") {
                eprintln!("FAIL: recall block missing 'Completed' outcome");
                ok = false;
            }
        }
        None => { eprintln!("FAIL: recall block was None"); ok = false; }
    }

    // Also run a real mission end-to-end to prove the injection doesn't break
    // the planner.
    let mid = runtime.missions.create(
        "write hello file".to_string(),
        "Create a file named greeting.txt at the workspace root containing the string hello.".to_string(),
    ).await?;
    println!("created mission {mid}");
    let mut rx = runtime.events.subscribe();
    runtime.missions.plan_and_run(mid).await?;

    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    let mut final_status: Option<MissionStatus> = None;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Ok(env)) => {
                if let ForgeEvent::MissionStatusChanged { id, to, .. } = env.event {
                    if id == mid {
                        println!("  → mission → {:?}", to);
                        if matches!(to, MissionStatus::Completed | MissionStatus::Failed | MissionStatus::Cancelled) {
                            final_status = Some(to);
                            break;
                        }
                    }
                }
            }
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => break,
            Err(_) => {}
        }
    }
    if final_status != Some(MissionStatus::Completed) {
        eprintln!("FAIL: end-to-end mission did not reach Completed: {final_status:?}");
        ok = false;
    }

    let _ = std::fs::remove_dir_all(&tmp);
    if ok { println!("PASS: episodic recall end-to-end verified"); Ok(()) }
    else  { std::process::exit(1) }
}
