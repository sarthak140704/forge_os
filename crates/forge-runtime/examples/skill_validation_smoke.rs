//! LLM-free headless smoke: exercise Phase 4b's validation gate + autopromoter.
//! Verifies:
//!   1. A GOOD proposal passes validation, emits SkillValidationPassed, and
//!      promotes normally (SkillPromoted event follows).
//!   2. A BAD proposal (missing keywords + wrong tool) fails validation,
//!      emits SkillValidationFailed, and `promote_from_proposal` returns
//!      SkillOpsError::ValidationFailed. The file stays in proposed/.
//!   3. The AutoPromoter.sweep() promotes the good one (already promoted above)
//!      as a no-op AND leaves the bad one alone.
//!
//! Run:   cargo run -p forge-runtime --example skill_validation_smoke

use forge_domain::ForgeEvent;
use forge_runtime::{
    skills_ops::{AutoPromoter, SkillOps, SkillOpsError},
    LlmConfig, LlmProviderConfig, Runtime, RuntimeConfig,
};
use forge_skills::{ProposalWriter, SuggestedSkill};
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

    let tmp = std::env::temp_dir().join(format!(
        "forge-skillval-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis()
    ));
    let workspace = tmp.join("workspace");
    let db_path = tmp.join("forge.sqlite");
    let skills_root = tmp.join("skills");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(skills_root.join("proposed"))?;

    unsafe { std::env::set_var("__FORGE_DUMMY_KEY__", "unused"); }
    let config = RuntimeConfig {
        workspace_root: workspace.clone(),
        db_path,
        policy_path: None,
        llm: LlmConfig {
            providers: vec![LlmProviderConfig::Groq { api_key_env: "__FORGE_DUMMY_KEY__".into() }],
            model: "unused-model".into(),
        },
        max_parallel_goals: 2,
        skills_root: Some(skills_root.clone()),
        mcp_config: Some(tmp.join("mcp.yaml")),
        auto_promote_skills: false,
        autopromote_interval_secs: 300,
        curator: Default::default(),
        curator_sweep_enabled: false,
        curator_interval_secs: 900,
        workers: 0,
        worker_stale_secs: 120,
        org_memory_enabled: false,
        api_bind: None,
        api_token_env: String::new(),
    };
    let runtime = Runtime::boot(config).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let ops: Arc<SkillOps> = runtime.skill_ops.as_ref().expect("skill_ops present").clone();

    // The default runtime tool set is fs.*, shell.*, etc — see forge-tools.
    // Verify the whitelist got populated so the tools_resolvable check
    // will actually pass for a proposal that lists fs.read.
    assert!(ops.known_tools.iter().any(|t| t == "fs.read"),
        "expected fs.read to be in the validator whitelist, got: {:?}", ops.known_tools);
    println!("validator whitelist: {} tools", ops.known_tools.len());

    let writer = ProposalWriter::new(&skills_root);
    let mut all_ok = true;

    // ─── Scenario 1: good proposal passes and promotes ───────────────────────
    let good = writer.write_proposal(&SuggestedSkill {
        name: "validation-good".into(),
        description: "a well-formed skill".into(),
        tools: vec!["fs.read".into()],
        keywords: vec!["validate".into(), "good".into()],
        body: "# validation-good\n\nThis body is definitely more than forty non-whitespace characters so it passes body_length.\n".into(),
        origin_mission_id: "".into(),
    })?;
    let good_filename = good.file_name().unwrap().to_string_lossy().to_string();

    let mut rx = runtime.events.subscribe();
    println!("\n=== 1. promote GOOD proposal ({good_filename}) ===");
    let report = ops.validate_proposal(&good_filename).await?;
    println!("  report.ok = {}, failed = {:?}", report.ok, report.failed());
    if !report.ok {
        eprintln!("  FAIL: validator rejected a good proposal");
        all_ok = false;
    }
    let row = ops.promote_from_proposal(&good_filename, None).await?;
    println!("  promoted sha = {}", &row.sha[..12]);
    if !expect_event(&mut rx, "skill_validation_passed", Some("validation-good")).await { all_ok = false; }
    if !expect_event(&mut rx, "skill_promoted",           Some("validation-good")).await { all_ok = false; }

    // ─── Scenario 2: bad proposal fails validation, no promotion ─────────────
    // Directly hand-write the file so we can violate the format cleanly.
    let bad_path = skills_root.join("proposed").join("validation-bad.md");
    std::fs::write(&bad_path, r#"---
name: validation-bad
version: 0.1.0
description: broken skill for tests
status: pending_review
tools:
  - nonexistent.tool
triggers:
  keywords: []
  file_globs: []
inputs: []
outputs: []
---
short
"#)?;

    println!("\n=== 2. promote BAD proposal (expect ValidationFailed) ===");
    let bad_report = ops.validate_proposal("validation-bad.md").await?;
    println!("  report.ok = {}, hard_failures = {:?}", bad_report.ok, bad_report.hard_failures());
    if bad_report.ok {
        eprintln!("  FAIL: validator accepted a broken proposal");
        all_ok = false;
    }
    match ops.promote_from_proposal("validation-bad.md", None).await {
        Ok(_) => { eprintln!("  FAIL: promotion should have errored"); all_ok = false; }
        Err(SkillOpsError::ValidationFailed { filename, failed }) => {
            println!("  ✓ got ValidationFailed({filename}, {failed:?})");
            if failed.is_empty() {
                eprintln!("  FAIL: failed list should be non-empty");
                all_ok = false;
            }
        }
        Err(e) => {
            eprintln!("  FAIL: wrong error variant: {e}");
            all_ok = false;
        }
    }
    if !expect_event(&mut rx, "skill_validation_failed", Some("validation-bad")).await { all_ok = false; }
    // Bad file must still be in proposed/ (not moved to active/).
    if !bad_path.exists() {
        eprintln!("  FAIL: bad proposal file should remain in proposed/");
        all_ok = false;
    }
    // Nothing named validation-bad should be active.
    let active_bad = ops.history.active("validation-bad").await?;
    if active_bad.is_some() {
        eprintln!("  FAIL: validation-bad should NOT be active after failed promotion");
        all_ok = false;
    }

    // ─── Scenario 3: AutoPromoter with a fresh good proposal ─────────────────
    // Write another good proposal for a NEW name so the sweep has something
    // to promote (the earlier good one is already active).
    let good2 = writer.write_proposal(&SuggestedSkill {
        name: "auto-promoted".into(),
        description: "another good one for autopromoter".into(),
        tools: vec!["fs.write".into()],
        keywords: vec!["auto".into()],
        body: "# auto-promoted\n\nAutopromoter should pick this up because it clears every hard check.\n".into(),
        origin_mission_id: "".into(),
    })?;
    let good2_name = good2.file_name().unwrap().to_string_lossy().to_string();
    println!("\n=== 3. AutoPromoter.sweep() should promote {good2_name} ===");

    let auto = AutoPromoter::new(ops.clone(), Duration::from_secs(600));
    let promoted = auto.sweep().await?;
    println!("  sweep promoted count = {promoted}");
    if promoted == 0 {
        eprintln!("  FAIL: sweep should have promoted the good proposal");
        all_ok = false;
    }
    if !expect_event(&mut rx, "skill_validation_passed", Some("auto-promoted")).await { all_ok = false; }
    if !expect_event(&mut rx, "skill_promoted",           Some("auto-promoted")).await { all_ok = false; }
    if !expect_event(&mut rx, "skill_auto_promoted",      Some("auto-promoted")).await { all_ok = false; }

    // Bad proposal should have been skipped (still in proposed/, no active).
    if !bad_path.exists() {
        eprintln!("  FAIL: autopromoter should NOT have moved the bad proposal");
        all_ok = false;
    }

    let _ = std::fs::remove_dir_all(&tmp);
    if all_ok {
        println!("\nPASS: validation gate + autopromoter verified end-to-end");
        Ok(())
    } else {
        eprintln!("\nFAIL: one or more assertions did not hold");
        std::process::exit(1);
    }
}

/// Wait up to 3s for a matching ForgeEvent whose `type` field serializes to
/// `want`. If `name_hint` is Some, also require the event's `name` field to
/// match. Returns true on hit; prints on both hit + miss.
async fn expect_event(
    rx: &mut tokio::sync::broadcast::Receiver<forge_domain::EventEnvelope>,
    want: &str,
    name_hint: Option<&str>,
) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
            Ok(Ok(env)) => {
                let (kind, ev_name) = match &env.event {
                    ForgeEvent::SkillPromoted { name, .. }           => ("skill_promoted", Some(name.as_str())),
                    ForgeEvent::SkillValidationPassed { name, .. }   => ("skill_validation_passed", Some(name.as_str())),
                    ForgeEvent::SkillValidationFailed { name, .. }   => ("skill_validation_failed", Some(name.as_str())),
                    ForgeEvent::SkillAutoPromoted { name, .. }       => ("skill_auto_promoted", Some(name.as_str())),
                    _ => ("other", None),
                };
                if kind == want && name_hint.map(|h| Some(h) == ev_name).unwrap_or(true) {
                    println!("  ✓ observed {want}");
                    return true;
                }
            }
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed))    => break,
            Err(_)                        => {}
        }
    }
    eprintln!("  FAIL: never observed {want}{}", name_hint.map(|h| format!(" (name={h})")).unwrap_or_default());
    false
}
