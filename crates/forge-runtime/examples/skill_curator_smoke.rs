//! LLM-free headless smoke: exercise Phase 4c's actionable Curator.
//!
//! Three scenarios:
//!   1. **Dedupe by body similarity** — two active skills with near-identical
//!      bodies. `scan(apply=true)` archives the loser and emits
//!      `SkillAutoArchived` + `SkillRetired`. The kept skill remains active.
//!   2. **Merge proposal** — two active skills whose bodies overlap
//!      moderately (~0.6 Jaccard) but neither dominates. `scan(apply=true)`
//!      writes a merged skill to `proposed/` and emits `SkillMergeProposed`.
//!      The merged proposal must pass the validator.
//!   3. **Unused suggestion** — an active skill that never appears in a
//!      `SkillsSelected` event surfaces as a `CuratorKind::Unused`
//!      suggestion. `apply` never archives Unused (only Duplicates).
//!
//! Run:   cargo run -p forge-runtime --example skill_curator_smoke

use forge_domain::ForgeEvent;
use forge_runtime::{
    skills_ops::{Curator, CuratorKind, CuratorPolicy, SkillOps},
    LlmConfig, LlmProviderConfig, Runtime, RuntimeConfig,
};
use forge_persistence::SqliteEventStore;
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
        "forge-curator-{}",
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
        db_path: db_path.clone(),
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
        // Tight thresholds so short test bodies still cross them.
        curator: CuratorPolicy {
            name_similarity_threshold: 0.92,
            body_similarity_threshold: 0.80,
            merge_similarity_low:      0.35,
            protect_recent_usage_missions: 5,
            auto_act: false,
        },
        curator_sweep_enabled: false,
        curator_interval_secs: 900,
        workers: 0,
        worker_stale_secs: 120,
        org_memory_enabled: false,
    };
    let runtime = Runtime::boot(config.clone()).await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let ops: Arc<SkillOps> = runtime.skill_ops.as_ref().expect("skill_ops present").clone();

    let mut all_ok = true;
    let writer = ProposalWriter::new(&skills_root);

    // Seed three active skills by promoting proposals through the real path.
    // (Guarantees they show up in history + on-disk active/ tree, so the
    // Curator sees them via list_active + reads their bodies.)
    let promote = |name: &str, body: &str| {
        let ops = ops.clone();
        let writer = &writer;
        let name = name.to_string();
        let body = body.to_string();
        async move {
            let path = writer.write_proposal(&SuggestedSkill {
                name: name.clone(),
                description: format!("seed skill {name}"),
                tools: vec!["fs.read".into()],
                keywords: vec!["seed".into(), name.clone()],
                body,
                origin_mission_id: String::new(),
            }).unwrap();
            let file = path.file_name().unwrap().to_string_lossy().to_string();
            ops.promote_from_proposal(&file, None).await.unwrap();
        }
    };

    // Two near-duplicate bodies (Jaccard should be > 0.80).
    let dupe_body_a = "Run cargo test in the crate root and report failures with stack traces to the operator afterwards.";
    let dupe_body_b = "Run cargo test in the crate root and report failures with stack traces to the operator afterwards for review.";
    promote("dedupe-alpha", dupe_body_a).await;
    promote("dedupe-beta",  dupe_body_b).await;

    // Two moderately overlapping bodies for the merge scenario. Share
    // several sentences (~45-70% Jaccard) so they land in the merge band
    // between merge_low (0.35) and body_similarity_threshold (0.80).
    let merge_body_a = "Set up a fresh Python virtual environment using venv in the project root. \
                        Activate the environment. Install project dependencies from requirements.txt. \
                        Verify the interpreter path matches the venv. \
                        Print the resolved package versions. \
                        Log the activation step to stdout for the operator.";
    let merge_body_b = "Activate the environment. Install project dependencies from requirements.txt. \
                        Verify the interpreter path matches the venv. \
                        Print the resolved package versions. \
                        Run the test suite with pytest. Report failing tests to stdout for the operator.";
    promote("py-envsetup",     merge_body_a).await;
    promote("py-test-workflow", merge_body_b).await;

    // A totally isolated skill with a unique body — will be flagged Unused
    // (no `SkillsSelected` event ever mentions it).
    let isolated_body = "Compose a haiku about a solitary lighthouse standing against a stormy midnight sea.";
    promote("haiku-lighthouse", isolated_body).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Fresh curator with tight thresholds; matches the config.
    let events_store = Arc::new(SqliteEventStore::new(runtime.pool.clone()));
    let curator = Curator::with_policy(ops.clone(), events_store, config.curator.clone());

    // Fresh event receiver AFTER seed promotions so we don't drown in noise.
    let mut rx = runtime.events.subscribe();

    // ─── Scenario 1: dry-run classification ──────────────────────────────────
    println!("\n=== 1. curator scan (dry-run) ===");
    let dry = curator.scan(false).await?;
    println!("  suggestions: {}", dry.suggestions.len());
    for s in &dry.suggestions {
        println!("    - {:>15} : {} — {}", s.kind.as_str(), s.name, s.evidence);
    }
    let has_dupe = dry.suggestions.iter().any(|s| matches!(s.kind, CuratorKind::Duplicate)
        && (s.name == "dedupe-alpha" || s.name == "dedupe-beta"));
    let has_merge = dry.suggestions.iter().any(|s| matches!(s.kind, CuratorKind::MergeCandidate)
        && (s.name == "py-envsetup" || s.name == "py-test-workflow"));
    let has_unused = dry.suggestions.iter().any(|s| matches!(s.kind, CuratorKind::Unused)
        && s.name == "haiku-lighthouse");
    if !has_dupe   { eprintln!("  FAIL: no Duplicate suggestion for dedupe pair"); all_ok = false; }
    if !has_merge  { eprintln!("  FAIL: no MergeCandidate suggestion for py pair"); all_ok = false; }
    if !has_unused { eprintln!("  FAIL: no Unused suggestion for haiku-lighthouse"); all_ok = false; }
    if dry.auto_archived.len() != 0 || dry.merge_proposals.len() != 0 {
        eprintln!("  FAIL: dry-run should not have taken actions"); all_ok = false;
    }

    // ─── Scenario 2: apply — auto-archive + merge proposal ───────────────────
    println!("\n=== 2. curator scan (apply=true) ===");
    let report = curator.scan(true).await?;
    println!(
        "  archived: {} pairs, merge proposals: {}",
        report.auto_archived.len(), report.merge_proposals.len()
    );
    for (lost, kept) in &report.auto_archived {
        println!("    - archived `{lost}` → kept `{kept}`");
    }
    for f in &report.merge_proposals {
        println!("    - merge proposal: {f}");
    }

    if report.auto_archived.is_empty() {
        eprintln!("  FAIL: expected at least one auto-archive"); all_ok = false;
    }
    if report.merge_proposals.is_empty() {
        eprintln!("  FAIL: expected at least one merge proposal"); all_ok = false;
    }

    // The loser is alphabetically later → dedupe-beta archived, dedupe-alpha kept.
    let alpha_active = ops.history.active("dedupe-alpha").await?;
    let beta_active  = ops.history.active("dedupe-beta").await?;
    if alpha_active.is_none() { eprintln!("  FAIL: dedupe-alpha should still be active"); all_ok = false; }
    if beta_active.is_some()  { eprintln!("  FAIL: dedupe-beta should be archived"); all_ok = false; }

    // The merge proposal must actually parse + pass the validator.
    if let Some(filename) = report.merge_proposals.first() {
        let val = ops.validate_proposal(filename).await?;
        println!("  merge proposal validation: ok={} failed={:?}",
                 val.ok, val.failed());
        if !val.ok {
            eprintln!("  FAIL: generated merge proposal failed validation: {:?}", val.hard_failures());
            all_ok = false;
        }
    }

    // Drain expected events (order-agnostic).
    if !observe_events(&mut rx, &[
        "skill_auto_archived",
        "skill_retired",
        "skill_merge_proposed",
    ]).await { all_ok = false; }

    // ─── Scenario 3: apply is idempotent (no new merge proposals) ────────────
    println!("\n=== 3. second apply pass — should be a no-op ===");
    // Re-run with a fresh receiver so we only see the second-pass events.
    let mut rx2 = runtime.events.subscribe();
    let report2 = curator.scan(true).await?;
    println!(
        "  archived this pass: {}, merge proposals this pass: {}",
        report2.auto_archived.len(), report2.merge_proposals.len()
    );
    if !report2.merge_proposals.is_empty() {
        eprintln!("  FAIL: second pass should skip the merge (pending proposal already exists)");
        all_ok = false;
    }
    // Consume any curator suggestions still emitted; we only care that
    // no NEW SkillMergeProposed / SkillAutoArchived fired for the same pair.
    let saw_new_merge = drain_and_check(&mut rx2, "skill_merge_proposed").await;
    if saw_new_merge {
        eprintln!("  FAIL: second pass emitted another SkillMergeProposed");
        all_ok = false;
    }

    // Cleanup — best effort. On Windows the sqlite file may still be
    // memory-mapped by the runtime; leaking the temp dir is fine for CI.
    let _ = std::fs::remove_dir_all(&tmp);
    if all_ok {
        println!("\nPASS: Phase 4c curator verified end-to-end");
        Ok(())
    } else {
        eprintln!("\nFAIL: one or more assertions did not hold");
        std::process::exit(1);
    }
}

/// Consume events for up to 3s; return true iff *every* wanted type was seen.
async fn observe_events(
    rx: &mut tokio::sync::broadcast::Receiver<forge_domain::EventEnvelope>,
    wanted: &[&str],
) -> bool {
    let mut remaining: std::collections::HashSet<&str> = wanted.iter().copied().collect();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !remaining.is_empty() && std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Ok(env)) => {
                let kind = event_kind(&env.event);
                if remaining.remove(kind) {
                    println!("  ✓ observed {kind}");
                }
            }
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed))    => break,
            Err(_)                        => {}
        }
    }
    if !remaining.is_empty() {
        for k in &remaining {
            eprintln!("  FAIL: never observed {k}");
        }
        false
    } else {
        true
    }
}

async fn drain_and_check(
    rx: &mut tokio::sync::broadcast::Receiver<forge_domain::EventEnvelope>,
    forbidden: &str,
) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_millis(800);
    let mut saw = false;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(150), rx.recv()).await {
            Ok(Ok(env)) => {
                if event_kind(&env.event) == forbidden { saw = true; }
            }
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed))    => break,
            Err(_)                        => {}
        }
    }
    saw
}

fn event_kind(ev: &ForgeEvent) -> &'static str {
    match ev {
        ForgeEvent::SkillAutoArchived { .. }        => "skill_auto_archived",
        ForgeEvent::SkillMergeProposed { .. }       => "skill_merge_proposed",
        ForgeEvent::SkillRetired { .. }             => "skill_retired",
        ForgeEvent::SkillCurationSuggested { .. }   => "skill_curation_suggested",
        ForgeEvent::SkillPromoted { .. }            => "skill_promoted",
        ForgeEvent::SkillValidationPassed { .. }    => "skill_validation_passed",
        ForgeEvent::SkillValidationFailed { .. }    => "skill_validation_failed",
        _                                           => "other",
    }
}
