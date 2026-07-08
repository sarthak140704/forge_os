//! LLM-free headless smoke: exercise the Phase 4a version-controlled skills
//! path (promote → promote → rollback → retire) and assert:
//!   - every step emits the right ForgeEvent variant,
//!   - the history table has the expected chain of rows (with parent_sha links),
//!   - files on disk end up in the expected place (`active/`, `archived/`),
//!   - the curator surfaces a duplicate suggestion for two similar names.
//!
//! Run:   cargo run -p forge-runtime --example skill_versioning_smoke

use forge_domain::ForgeEvent;
use forge_runtime::{LlmConfig, LlmProviderConfig, Runtime, RuntimeConfig};
use forge_skills::{ProposalWriter, SuggestedSkill};
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
        "forge-skillver-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis()
    ));
    let workspace = tmp.join("workspace");
    let db_path = tmp.join("forge.sqlite");
    let skills_root = tmp.join("skills");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(skills_root.join("active"))?;
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
        mcp_config:  Some(tmp.join("mcp.yaml")),
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

    let ops = runtime.skill_ops.as_ref().expect("skill_ops must be present when skills_root is set").clone();
    let curator = runtime.curator.as_ref().expect("curator must be present when skills_root is set").clone();

    // ─── Prepare two proposals for the same skill name ───────────────────────
    let writer = ProposalWriter::new(&skills_root);
    let p1 = writer.write_proposal(&SuggestedSkill {
        name: "smoke-skill".into(),
        description: "first version".into(),
        tools: vec!["fs.read".into()],
        keywords: vec!["smoke".into()],
        body: "# smoke-skill v1\n\nThis is the first version of the smoke skill used for versioning tests. It exercises promotion, rollback, and history behaviour end to end.\n".into(),
        origin_mission_id: "".into(),
    })?;
    // Guarantee a distinct filename by advancing filesystem timestamps.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let p2 = writer.write_proposal(&SuggestedSkill {
        name: "smoke-skill".into(),
        description: "second version".into(),
        tools: vec!["fs.read".into(), "fs.write".into()],
        keywords: vec!["smoke".into()],
        body: "# smoke-skill v2\n\nSecond version of the same skill, adds an extra tool and rewrites the body so version monotonicity and rollback can be validated.\n".into(),
        origin_mission_id: "".into(),
    })?;
    // Also write a proposal for a name close enough to trigger the curator
    // duplicate heuristic (Jaro-Winkler on names ≥ 0.90).
    let p3 = writer.write_proposal(&SuggestedSkill {
        name: "smoke-skil".into(), // one char off
        description: "near-duplicate".into(),
        tools: vec!["fs.read".into()],
        keywords: vec!["smoke".into()],
        body: "# near-dup\n\nA near-duplicate proposal used to exercise the curator name-similarity heuristic without being an outright collision.\n".into(),
        origin_mission_id: "".into(),
    })?;

    let file1 = p1.file_name().unwrap().to_string_lossy().to_string();
    let file2 = p2.file_name().unwrap().to_string_lossy().to_string();
    let file3 = p3.file_name().unwrap().to_string_lossy().to_string();

    let mut rx = runtime.events.subscribe();
    let mut all_ok = true;

    // ─── 1. Promote first proposal ───────────────────────────────────────────
    println!("\n=== 1. promote_from_proposal({file1}) ===");
    let row1 = ops.promote_from_proposal(&file1, None).await?;
    println!("  row1.sha    = {}", row1.sha);
    println!("  row1.origin = {:?}", row1.origin);
    if row1.parent_sha.is_some() {
        eprintln!("  FAIL: first promotion should have parent_sha = None");
        all_ok = false;
    }
    if !expect_event(&mut rx, "skill_promoted", &row1.sha).await {
        all_ok = false;
    }

    // ─── 2. Promote second proposal (parent = row1.sha) ──────────────────────
    println!("\n=== 2. promote_from_proposal({file2}) ===");
    let row2 = ops.promote_from_proposal(&file2, None).await?;
    println!("  row2.sha        = {}", row2.sha);
    println!("  row2.parent_sha = {:?}", row2.parent_sha);
    if row2.parent_sha.as_deref() != Some(row1.sha.as_str()) {
        eprintln!("  FAIL: second promotion parent_sha should be {}, got {:?}", row1.sha, row2.parent_sha);
        all_ok = false;
    }
    if row2.sha == row1.sha {
        eprintln!("  FAIL: second promotion should have a different sha");
        all_ok = false;
    }
    if !expect_event(&mut rx, "skill_promoted", &row2.sha).await {
        all_ok = false;
    }

    // ─── 3. Rollback to row1's sha ───────────────────────────────────────────
    println!("\n=== 3. rollback(smoke-skill, {}) ===", &row1.sha[..12]);
    let row3 = ops.rollback("smoke-skill", &row1.sha, Some("test rollback")).await?;
    println!("  row3.sha        = {} (should equal row1.sha)", row3.sha);
    println!("  row3.parent_sha = {:?} (should equal row2.sha)", row3.parent_sha);
    if row3.sha != row1.sha {
        eprintln!("  FAIL: rollback must produce a row with sha == target sha");
        all_ok = false;
    }
    if row3.parent_sha.as_deref() != Some(row2.sha.as_str()) {
        eprintln!("  FAIL: rollback parent_sha should be {}, got {:?}", row2.sha, row3.parent_sha);
        all_ok = false;
    }
    if !expect_event(&mut rx, "skill_rolled_back", &row1.sha).await {
        all_ok = false;
    }

    // ─── 4. Verify history rows ──────────────────────────────────────────────
    let hist = ops.history.history("smoke-skill").await?;
    println!("\n=== 4. history rows for smoke-skill: {} ===", hist.len());
    for r in &hist {
        println!("  id={:>3}  sha={}  origin={:?}  retired={}",
            r.id, &r.sha[..12], r.origin, r.retired_at.is_some());
    }
    if hist.len() != 3 {
        eprintln!("  FAIL: expected exactly 3 history rows, got {}", hist.len());
        all_ok = false;
    }
    // Newest first: rollback, then row2 (retired), then row1 (retired).
    if !hist.first().map(|r| r.retired_at.is_none()).unwrap_or(false) {
        eprintln!("  FAIL: newest row should be active");
        all_ok = false;
    }

    // ─── 5. Retire smoke-skill entirely ──────────────────────────────────────
    println!("\n=== 5. retire(smoke-skill, eol) ===");
    let retired_sha = ops.retire("smoke-skill", "end-of-life").await?;
    println!("  retired_sha = {:?}", retired_sha);
    if retired_sha.as_deref() != Some(row1.sha.as_str()) {
        eprintln!("  FAIL: retire should have retired the currently-active sha ({}), got {:?}", row1.sha, retired_sha);
        all_ok = false;
    }
    if !expect_event(&mut rx, "skill_retired", &row1.sha).await {
        all_ok = false;
    }
    let active_after = ops.history.active("smoke-skill").await?;
    if active_after.is_some() {
        eprintln!("  FAIL: no row should be active after retire");
        all_ok = false;
    }
    // Walk active/ and confirm no file parses to `smoke-skill` anymore.
    let active_dir = skills_root.join("active");
    let mut leftover = false;
    if active_dir.exists() {
        for entry in std::fs::read_dir(&active_dir)?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") { continue; }
            if let Ok(raw) = std::fs::read_to_string(&path) {
                if let Ok(parsed) = forge_skills::parse_skill(&raw) {
                    if parsed.front.name == "smoke-skill" {
                        eprintln!("  FAIL: {} still parses to smoke-skill after retire", path.display());
                        leftover = true;
                    }
                }
            }
        }
    }
    if leftover { all_ok = false; }
    // And confirm the archive dir now has at least one file (the retired one).
    let archived_dir = skills_root.join("archived");
    let archived_count = if archived_dir.exists() {
        std::fs::read_dir(&archived_dir)?.flatten().count()
    } else { 0 };
    if archived_count == 0 {
        eprintln!("  FAIL: expected at least one file in archived/ after retire");
        all_ok = false;
    } else {
        println!("  ✓ archived/ contains {archived_count} file(s)");
    }

    // ─── 6. Curator: promote near-duplicate then run ─────────────────────────
    println!("\n=== 6. promote near-dup ({file3}) then run curator ===");
    let near = ops.promote_from_proposal(&file3, None).await?;
    println!("  near.name = {}  near.sha = {}", near.name, &near.sha[..12]);
    // Drain any events emitted by that promote.
    drain_events(&mut rx, Duration::from_millis(200)).await;
    // Promote a second skill so the curator has a pair to compare against.
    let p4 = writer.write_proposal(&SuggestedSkill {
        name: "smoke-skiler".into(),
        description: "another near-dup".into(),
        tools: vec!["fs.read".into()],
        keywords: vec!["smoke".into()],
        body: "# skiler\n\nAnother near-duplicate skill used to give the curator a second promoted skill to compare names against during suggestion runs.\n".into(),
        origin_mission_id: "".into(),
    })?;
    let file4 = p4.file_name().unwrap().to_string_lossy().to_string();
    ops.promote_from_proposal(&file4, None).await?;
    drain_events(&mut rx, Duration::from_millis(200)).await;

    let suggestions = curator.run().await?;
    println!("  curator suggestions: {}", suggestions.len());
    for s in &suggestions {
        println!("    - kind={:?} name={} — {}", s.kind, s.name, s.evidence);
    }
    let dup_found = suggestions.iter().any(|s| matches!(s.kind, forge_runtime::skills_ops::CuratorKind::Duplicate));
    if !dup_found {
        eprintln!("  FAIL: expected at least one Duplicate suggestion");
        all_ok = false;
    }

    let _ = std::fs::remove_dir_all(&tmp);
    if all_ok {
        println!("\nPASS: skill versioning + curator verified end-to-end");
        Ok(())
    } else {
        eprintln!("\nFAIL: one or more assertions did not hold");
        std::process::exit(1);
    }
}

/// Wait up to 3s for a `ForgeEvent` whose `type` field serializes to `want`
/// AND (when applicable) whose payload contains `sha`. Prints the event on
/// success. Returns true if seen.
async fn expect_event(
    rx: &mut tokio::sync::broadcast::Receiver<forge_domain::EventEnvelope>,
    want: &str,
    sha_hint: &str,
) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
            Ok(Ok(env)) => {
                let (kind, matches_sha) = match &env.event {
                    ForgeEvent::SkillPromoted { sha, .. }        => ("skill_promoted", sha == sha_hint),
                    ForgeEvent::SkillRolledBack { to_sha, .. }   => ("skill_rolled_back", to_sha == sha_hint),
                    ForgeEvent::SkillRetired { sha, .. }         => ("skill_retired", sha == sha_hint),
                    ForgeEvent::SkillCurationSuggested { .. }    => ("skill_curation_suggested", true),
                    _ => ("other", false),
                };
                if kind == want && matches_sha {
                    println!("  ✓ observed {want}");
                    return true;
                }
            }
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed))    => break,
            Err(_)                        => {}
        }
    }
    eprintln!("  FAIL: never observed {want} (sha_hint={})", &sha_hint[..sha_hint.len().min(12)]);
    false
}

/// Consume any pending events and discard, so the next `expect_event` call
/// starts from a clean slate.
async fn drain_events(
    rx: &mut tokio::sync::broadcast::Receiver<forge_domain::EventEnvelope>,
    for_at_most: Duration,
) {
    let deadline = std::time::Instant::now() + for_at_most;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(_)) => {}
            _         => break,
        }
    }
}
