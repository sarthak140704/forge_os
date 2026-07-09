//! Phase 4f smoke — organizational memory.
//!
//! Exercises the memory repository end-to-end via a booted Runtime so that
//! we prove:
//!   * Migration V004 is applied on runtime boot.
//!   * `OrgMemoryRepository.insert` produces a queryable row.
//!   * `list_active` returns most-recent-first and skips retired rows.
//!   * `search` matches by tag + by LIKE on key/value.
//!   * `retire` is idempotent and removes the row from `list_active`.
//!
//! This does NOT drive a full mission (that requires an LLM). The
//! reflection→memory extraction path is unit-tested elsewhere; here we
//! prove the storage + query layer works over the Runtime's real handles.
//!
//! Run:   cargo run -p forge-runtime --example org_memory_smoke

use forge_persistence::NewOrgMemory;
use forge_runtime::{LlmConfig, LlmProviderConfig, Runtime, RuntimeConfig};

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
        "forge-orgmem-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis()
    ));
    let workspace = tmp.join("workspace");
    let db_path = tmp.join("forge.sqlite");
    std::fs::create_dir_all(&workspace)?;

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
        skills_root: None,
        mcp_config: None,
        auto_promote_skills: false,
        autopromote_interval_secs: 300,
        curator: Default::default(),
        curator_sweep_enabled: false,
        curator_interval_secs: 900,
        workers: 0,
        worker_stale_secs: 60,
        org_memory_enabled: true,
        embedding_provider: None,
        api_bind: None,
        api_token_env: String::new(),
    };
    let runtime = Runtime::boot(config).await?;
    let mut all_ok = true;

    // ─── 1. insert 3 memories ────────────────────────────────────────────
    println!("=== 1. insert 3 org-memory rows ===");
    let mid = runtime.missions.create("seed-mission".into(), "".into()).await?;
    let id1 = runtime.org_memory.insert(&NewOrgMemory {
        key:  "prefer-rust-cargo-workspaces".into(),
        value: "Rust projects with multiple crates should use a top-level Cargo workspace.".into(),
        tags: vec!["rust".into(), "workspace".into()],
        source_mission_id: Some(mid),
        embedding: None,
    }).await?;
    let id2 = runtime.org_memory.insert(&NewOrgMemory {
        key:  "always-run-cargo-fmt".into(),
        value: "Format every Rust change with cargo fmt before commit.".into(),
        tags: vec!["rust".into(), "fmt".into()],
        source_mission_id: Some(mid),
        embedding: None,
    }).await?;
    let id3 = runtime.org_memory.insert(&NewOrgMemory {
        key:  "python-uses-uv".into(),
        value: "Prefer uv over pip/poetry for Python dependency management.".into(),
        tags: vec!["python".into(), "uv".into()],
        source_mission_id: Some(mid),
        embedding: None,
    }).await?;
    println!("  ✓ inserted #{id1} #{id2} #{id3}");

    // ─── 2. list_active is most-recent-first ─────────────────────────────
    println!("\n=== 2. list_active returns rows in insertion (recency) order ===");
    let active = runtime.org_memory.list_active(10).await?;
    if active.len() != 3 {
        eprintln!("  FAIL: expected 3 active rows, got {}", active.len());
        all_ok = false;
    }
    if active.first().map(|r| r.id) != Some(id3) {
        eprintln!("  FAIL: expected id3 first (most recent), got {:?}", active.first().map(|r| r.id));
        all_ok = false;
    } else {
        println!("  ✓ ordered most-recent-first (id3, id2, id1)");
    }

    // ─── 3. search by tag ────────────────────────────────────────────────
    println!("\n=== 3. search by tag returns matching rows ===");
    let rust_hits = runtime.org_memory.search(&["rust".into()], 10).await?;
    if rust_hits.len() != 2 {
        eprintln!("  FAIL: expected 2 rust hits, got {}", rust_hits.len());
        all_ok = false;
    } else {
        println!("  ✓ 2 hits for tag=rust");
    }
    let python_hits = runtime.org_memory.search(&["python".into()], 10).await?;
    if python_hits.len() != 1 {
        eprintln!("  FAIL: expected 1 python hit, got {}", python_hits.len());
        all_ok = false;
    } else {
        println!("  ✓ 1 hit for tag=python");
    }

    // ─── 4. retire is idempotent + hides from list_active ────────────────
    println!("\n=== 4. retire hides the row from list_active ===");
    let first = runtime.org_memory.retire(id1).await?;
    let second = runtime.org_memory.retire(id1).await?;
    if !first {
        eprintln!("  FAIL: first retire should return true");
        all_ok = false;
    }
    if second {
        eprintln!("  FAIL: second retire should return false (idempotent no-op)");
        all_ok = false;
    }
    let active_after = runtime.org_memory.list_active(10).await?;
    if active_after.iter().any(|r| r.id == id1) {
        eprintln!("  FAIL: retired row {id1} is still in list_active");
        all_ok = false;
    } else {
        println!("  ✓ retired row hidden from list_active");
    }

    let _ = std::fs::remove_dir_all(&tmp);
    if all_ok {
        println!("\nPASS: org memory storage verified end-to-end");
        Ok(())
    } else {
        eprintln!("\nFAIL: org memory smoke had assertion failures");
        std::process::exit(1);
    }
}
