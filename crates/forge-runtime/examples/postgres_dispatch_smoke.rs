//! Phase 4e smoke — Postgres URL dispatch stub.
//!
//! Confirms that `PersistenceHandles::open` correctly routes `sqlite://` URLs
//! to the real SQLite backend and `postgres://` URLs to the honest
//! NotYetImplemented scaffold. This exercises the swap boundary itself, not
//! any Postgres server (which we don't run in dev).
//!
//! Run:   cargo run -p forge-runtime --example postgres_dispatch_smoke

use forge_persistence::{PersistenceError, PersistenceHandles};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = std::env::temp_dir().join(format!(
        "forge-pgdispatch-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis()
    ));
    std::fs::create_dir_all(&tmp)?;
    let db_path = tmp.join("forge.sqlite");
    let mut all_ok = true;

    // ─── 1. sqlite:// URL boots successfully ────────────────────────────────
    println!("=== 1. sqlite:// URL should boot ===");
    let sqlite_url = format!(
        "sqlite://{}?mode=rwc",
        db_path.display().to_string().replace('\\', "/")
    );
    match PersistenceHandles::open(&sqlite_url).await {
        Ok(h) => {
            println!("  ✓ opened sqlite handle: pool={:?}", h.pool_kind);
            // Prove the bundle really contains a working repo.
            let (queued, claimed) = h.queue.depth().await?;
            println!("  ✓ queue.depth() = ({queued}, {claimed})");
        }
        Err(e) => {
            eprintln!("  FAIL: expected sqlite open to succeed, got {e}");
            all_ok = false;
        }
    }

    // ─── 2. postgres:// URL returns NotYetImplemented ───────────────────────
    println!("\n=== 2. postgres:// URL should hit the honest stub ===");
    let pg_url = "postgres://forge:forge@localhost:5432/forge";
    match PersistenceHandles::open(pg_url).await {
        Ok(_) => {
            eprintln!("  FAIL: postgres stub returned Ok — must be NotYetImplemented");
            all_ok = false;
        }
        Err(PersistenceError::NotYetImplemented(feature)) => {
            println!("  ✓ got NotYetImplemented(\"{feature}\")");
            if !feature.to_lowercase().contains("postgres") {
                eprintln!("  WARN: error text should mention postgres, got {feature:?}");
            }
        }
        Err(other) => {
            eprintln!("  FAIL: wrong error variant: {other}");
            all_ok = false;
        }
    }

    // ─── 3. malformed URL is rejected up-front ──────────────────────────────
    println!("\n=== 3. unrecognized scheme should be rejected ===");
    match PersistenceHandles::open("mysql://nope").await {
        Ok(_) => {
            eprintln!("  FAIL: mysql:// should not be accepted");
            all_ok = false;
        }
        Err(e) => println!("  ✓ rejected: {e}"),
    }

    let _ = std::fs::remove_dir_all(&tmp);
    if all_ok {
        println!("\nPASS: PersistenceHandles URL dispatch verified");
        Ok(())
    } else {
        eprintln!("\nFAIL: dispatch smoke had assertion failures");
        std::process::exit(1);
    }
}
