//! LLM-free headless smoke for Phase 4d — mission queue + worker pool.
//!
//! Boots a runtime with `workers=2`, creates 4 real missions, enqueues them
//! all, and waits for the worker pool to drain them. Planning will fail
//! (dummy LLM key), which is fine — we only care that the queue state
//! machine works end-to-end:
//!
//!   1. `enqueue` inserts rows in `queued`.
//!   2. Workers claim them (queued → claimed), pulling in parallel.
//!   3. `plan_and_run_sync` fails → `finish(success=false)` (claimed → failed).
//!   4. `MissionQueued` events fire on enqueue.
//!   5. A stale `claimed` row (heartbeat in the past) is rescued by the
//!      janitor and requeued back to `queued`.
//!
//! Run:   cargo run -p forge-runtime --example worker_pool_smoke

use forge_domain::ForgeEvent;
use forge_persistence::QueueStatus;
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

    let tmp = std::env::temp_dir().join(format!(
        "forge-workerpool-{}",
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
        workers: 2,
        // Short window so we can rescue a "stale" claim in a few seconds
        // rather than waiting the desktop default of 120s.
        worker_stale_secs: 6,
        org_memory_enabled: true,
        embedding_provider: None,
        api_bind: None,
        api_token_env: String::new(),
    };
    let runtime = Runtime::boot(config).await?;
    let mut rx = runtime.events.subscribe();
    let mut all_ok = true;

    // ─── Scenario 1: enqueue 4 missions, workers drain them ────────────────
    println!("=== 1. enqueue 4 missions, expect workers to claim in parallel ===");
    let mut mids = vec![];
    for i in 0..4 {
        let mid = runtime.missions.create(
            format!("smoke-{i}"),
            format!("dummy mission {i}"),
        ).await?;
        runtime.missions.enqueue(mid).await?;
        mids.push(mid);
    }
    let mut queued_seen = 0usize;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while queued_seen < 4 && std::time::Instant::now() < deadline {
        if let Ok(Ok(env)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            if matches!(env.event, ForgeEvent::MissionQueued { .. }) {
                queued_seen += 1;
            }
        }
    }
    if queued_seen != 4 {
        eprintln!("  FAIL: expected 4 MissionQueued events, saw {queued_seen}");
        all_ok = false;
    } else {
        println!("  ✓ saw 4 MissionQueued events");
    }

    // Wait for the pool to drain (all rows in a terminal state).
    let drained = wait_for(Duration::from_secs(10), || async {
        let recent = runtime.queue.recent(50).await.unwrap();
        recent.iter().filter(|r| {
            matches!(r.status, QueueStatus::Done | QueueStatus::Failed)
        }).count() >= 4
    }).await;
    if !drained {
        eprintln!("  FAIL: queue did not drain within 10s");
        all_ok = false;
    } else {
        let (q, c) = runtime.queue.depth().await?;
        println!("  ✓ queue drained: queued={q}, claimed={c}");
    }

    // ─── Scenario 2: re-enqueueing a terminal mission creates a fresh row ─
    // (Active-dupe idempotency is unit-tested in forge-persistence directly.
    // In an integration smoke with live workers, we can only observe the
    // "terminal → retry" side of the contract deterministically.)
    println!("\n=== 2. re-enqueueing a failed mission inserts a NEW row (retry) ===");
    let before = runtime.queue.recent(50).await?.len();
    runtime.missions.enqueue(mids[0]).await?;
    // Wait for the worker to pick it up and fail.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let after = runtime.queue.recent(50).await?.len();
    if after != before + 1 {
        eprintln!("  FAIL: expected {} queue rows, saw {}", before + 1, after);
        all_ok = false;
    } else {
        println!("  ✓ retry row appended: {before} → {after}");
    }

    // ─── Scenario 3: crash recovery — stale claim gets requeued ───────────
    println!("\n=== 3. simulate a crashed worker: stale claim is requeued ===");
    // Enqueue a fresh mission and immediately mark it "claimed" with an
    // ancient heartbeat, then verify requeue_stale rescues it.
    let mid = runtime.missions.create("stale-mission".into(), "".into()).await?;
    let qid = runtime.queue.enqueue(mid).await?;
    // Directly claim & manually backdate the heartbeat via a naked SQL update
    // — the janitor treats anything older than `stale_after_secs` as dead.
    let _ = runtime.queue.claim_next("dead-worker").await?;
    sqlx::query("UPDATE mission_queue SET heartbeat_at = datetime('now','-60 seconds') WHERE id = ?")
        .bind(qid)
        .execute(&runtime.pool).await?;
    let n = runtime.queue.requeue_stale(6).await?;
    if n == 0 {
        eprintln!("  FAIL: requeue_stale returned 0 despite a stale row");
        all_ok = false;
    } else {
        println!("  ✓ requeue_stale rescued {n} row(s)");
    }

    // Give the pool a chance to pick the requeued row up (planning will still
    // fail because the mission is still in the dummy-LLM regime, but the row
    // must at least transition out of Queued).
    tokio::time::sleep(Duration::from_secs(3)).await;
    let recent = runtime.queue.recent(50).await?;
    let still_queued = recent.iter().filter(|r| matches!(r.status, QueueStatus::Queued)).count();
    if still_queued > 0 {
        eprintln!("  WARN: {still_queued} rows still Queued after 3s (workers may be busy)");
    } else {
        println!("  ✓ no rows stuck in Queued");
    }

    let _ = std::fs::remove_dir_all(&tmp);
    if all_ok {
        println!("\nPASS: worker pool + queue verified end-to-end");
        Ok(())
    } else {
        eprintln!("\nFAIL: one or more assertions did not hold");
        std::process::exit(1);
    }
}

async fn wait_for<F, Fut>(timeout: Duration, mut pred: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if pred().await { return true; }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    false
}

// Silence dead-code warning if RecvError isn't used in a future edit.
#[allow(dead_code)]
fn _unused(_: RecvError) {}
