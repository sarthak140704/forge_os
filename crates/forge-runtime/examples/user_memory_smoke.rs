//! Smoke test: user memory is loaded and injected into the planner prompt.
//!
//! We can't directly observe planner prompts from outside, so we take an
//! indirect approach: write a user memory that tells the agent to always
//! write to a specific file name, and see if the resulting mission produces
//! that file. This is a soft signal (the LLM may or may not follow the hint),
//! so the smoke test also verifies the more mechanical fact that
//! `UserMemory::load` picks the file up.
//!
//! Requires GROQ_API_KEY.
//! Run:   cargo run -p forge-runtime --example user_memory_smoke

use forge_domain::{ForgeEvent, MissionStatus};
use forge_runtime::{LlmConfig, LlmProviderConfig, Runtime, RuntimeConfig, UserMemory};
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
        "forge-usermem-smoke-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis()
    ));
    let workspace = tmp.join("workspace");
    let db_dir = tmp.clone();
    let db_path = db_dir.join("forge.sqlite");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&db_dir)?;

    // Write a user.md in db_dir so UserMemory::load(Some(db_dir)) picks it up.
    let user_md = db_dir.join("user.md");
    std::fs::write(&user_md,
        "# User preferences\n\
         - When writing any deliverable file for this mission, name it `PREFERRED.md`.\n\
         - Always keep files short (< 3 lines).\n"
    )?;
    println!("wrote user memory: {}", user_md.display());

    // Sanity-check the loader before boot.
    let um = UserMemory::load(Some(&db_dir)).expect("user memory should load");
    assert!(!um.content.is_empty());
    println!("user memory loaded from disk: {} bytes", um.content.len());

    // Point FORGE_USER_MEMORY env override at the same file — belt-and-braces.
    std::env::set_var("FORGE_USER_MEMORY", &user_md);

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
    let mut rx = runtime.events.subscribe();

    let mid = runtime.missions.create(
        "user memory smoke".into(),
        "Write a one-line hello note to a markdown file at the workspace root. \
         Follow the user's preferences.".into(),
    ).await?;
    println!("created mission {mid}");
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

    println!("---");
    println!("final: {final_status:?}");
    let entries: Vec<String> = std::fs::read_dir(&workspace)?.flatten()
        .filter_map(|e| e.file_name().to_string_lossy().to_string().into())
        .collect();
    println!("workspace files: {entries:?}");
    println!("---");

    let mut ok = true;
    if final_status != Some(MissionStatus::Completed) {
        eprintln!("FAIL: mission did not reach Completed");
        ok = false;
    }
    // Hard requirement: UserMemory::load found the file.
    if um.content.is_empty() {
        eprintln!("FAIL: UserMemory content was empty");
        ok = false;
    }
    // Soft signal: preferred filename hint was followed. Warn but don't fail
    // hard, since LLM adherence to preferences is probabilistic.
    let hit_preferred = entries.iter().any(|n| n.eq_ignore_ascii_case("PREFERRED.md"));
    if !hit_preferred {
        eprintln!("WARN: mission did not name the file PREFERRED.md ({entries:?})");
    } else {
        println!("BONUS: LLM followed the PREFERRED.md hint from user memory");
    }

    std::env::remove_var("FORGE_USER_MEMORY");
    let _ = std::fs::remove_dir_all(&tmp);
    if ok { println!("PASS: user memory loaded and mission ran"); Ok(()) }
    else  { std::process::exit(1) }
}
