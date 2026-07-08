//! Smoke test: mission cost summary event is emitted with non-zero tokens.
//!
//! Runs a tiny mission end-to-end and asserts that after terminal transition
//! we receive a `MissionCostSummary` event whose totals are non-zero.
//!
//! Requires GROQ_API_KEY.
//! Run:   cargo run -p forge-runtime --example cost_summary_smoke

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

    let tmp = std::env::temp_dir().join(format!(
        "forge-cost-smoke-{}",
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
        curator: Default::default(),
        curator_sweep_enabled: false,
        curator_interval_secs: 900,
    };
    let runtime = Runtime::boot(config).await?;
    let mut rx = runtime.events.subscribe();

    let mid = runtime.missions.create(
        "cost smoke".into(),
        "Write the single line 'hello' to a file called note.txt at the workspace root.".into(),
    ).await?;
    println!("created mission {mid}");
    runtime.missions.plan_and_run(mid).await?;

    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    let mut cost_seen: Option<(usize, usize, usize, u64)> = None;
    let mut llm_requests = 0usize;
    let mut final_status: Option<MissionStatus> = None;

    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Ok(env)) => match env.event {
                ForgeEvent::LlmRequested { mission_id, .. } => {
                    if mission_id == Some(mid) { llm_requests += 1; }
                }
                ForgeEvent::MissionCostSummary { mission_id, llm_calls, prompt_tokens, completion_tokens, total_latency_ms } => {
                    if mission_id == mid {
                        cost_seen = Some((llm_calls, prompt_tokens, completion_tokens, total_latency_ms));
                        println!("  💰 cost summary: {llm_calls} calls, {prompt_tokens}p + {completion_tokens}c tokens, {total_latency_ms}ms");
                    }
                }
                ForgeEvent::MissionStatusChanged { id, to, .. } if id == mid => {
                    println!("  → mission → {:?}", to);
                    if matches!(to, MissionStatus::Completed | MissionStatus::Failed | MissionStatus::Cancelled) {
                        final_status = Some(to);
                        // Cost summary is emitted AFTER terminal; don't break yet.
                    }
                }
                _ => {}
            },
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => break,
            Err(_) => {} // recv timeout, keep looping
        }
        if final_status.is_some() && cost_seen.is_some() { break; }
    }

    println!("---");
    println!("mission status  : {final_status:?}");
    println!("LlmRequested cnt: {llm_requests}");
    println!("cost summary    : {cost_seen:?}");
    println!("---");

    let mut ok = true;
    if llm_requests == 0 {
        eprintln!("FAIL: no LlmRequested events with this mission_id");
        ok = false;
    }
    match cost_seen {
        Some((calls, p, c, lat)) => {
            if calls == 0 { eprintln!("FAIL: cost summary shows 0 llm_calls"); ok = false; }
            if p == 0     { eprintln!("FAIL: cost summary shows 0 prompt_tokens"); ok = false; }
            if c == 0     { eprintln!("FAIL: cost summary shows 0 completion_tokens"); ok = false; }
            if lat == 0   { eprintln!("FAIL: cost summary shows 0 latency"); ok = false; }
        }
        None => { eprintln!("FAIL: no MissionCostSummary event seen"); ok = false; }
    }

    let _ = std::fs::remove_dir_all(&tmp);
    if ok { println!("PASS: cost tracking end-to-end verified"); Ok(()) }
    else  { std::process::exit(1) }
}
