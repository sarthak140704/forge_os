//! Phase 4e — Postgres backend scaffold.
//!
//! The persistence layer's whole point (see `crates/forge-persistence/src/lib.rs`
//! head comment) is that every repository is a trait; nothing in the domain,
//! executor, planner, or runtime touches `sqlx::SqlitePool` directly. That means
//! swapping in Postgres requires *only* implementing this file — no changes to
//! any consumer.
//!
//! ## What's here today
//!
//! - A `connect(url)` function that parses the URL, refuses non-postgres
//!   schemes, and returns `PersistenceError::NotYetImplemented` — a *honest*
//!   stub, not a silent fake. `PersistenceHandles::open("postgres://…")`
//!   dispatches here and surfaces the error to the caller.
//!
//! ## What lands next (Phase 5)
//!
//! 1. Rewrite `V001_INIT` / `V002_SKILLS_HISTORY` / `V003_MISSION_QUEUE` /
//!    `V004_ORG_MEMORY` migrations for PG (mostly `TEXT` → `TEXT`, `STRICT`
//!    drops, `?N` → `$N` placeholders, `RETURNING` already works).
//! 2. Add `sqlx = { features = ["postgres"] }` to `Cargo.toml`.
//! 3. Copy `sqlite.rs` → `postgres_impl.rs` and mechanically rewrite:
//!    - `sqlx::SqlitePool` → `sqlx::PgPool`
//!    - `?N` placeholders → `$N`
//!    - `INSERT … ON CONFLICT` semantics (equivalent to `INSERT OR IGNORE`)
//! 4. Wire `PersistenceHandles::postgres(url)` to build the PG-backed bundle.
//! 5. Add a smoke test gated on `$FORGE_POSTGRES_URL` (skip if unset).
//!
//! ## Why now
//!
//! Because the shape of `PersistenceHandles` is what other subsystems consume,
//! we lock in the swap boundary today. When Phase 5 flips this stub to a real
//! impl, no other crate needs to be edited.

use crate::PersistenceError;

/// Parse-only stub. Validates the URL is a Postgres URL and then returns
/// `NotYetImplemented`. Real connection pool comes in Phase 5.
pub async fn connect(url: &str) -> Result<(), PersistenceError> {
    if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
        return Err(PersistenceError::NotYetImplemented(
            "postgres::connect requires a postgres:// or postgresql:// URL",
        ));
    }
    Err(PersistenceError::NotYetImplemented(
        "postgres backend is scaffolded but not yet wired — see \
         crates/forge-persistence/src/postgres.rs head comment for the \
         Phase 5 rollout plan",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_non_pg_url() {
        let err = connect("sqlite://foo.db").await.unwrap_err();
        assert!(matches!(err, PersistenceError::NotYetImplemented(_)));
    }

    #[tokio::test]
    async fn accepts_pg_url_shape_but_returns_not_yet_implemented() {
        let err = connect("postgres://user:pw@localhost/forge").await.unwrap_err();
        assert!(matches!(err, PersistenceError::NotYetImplemented(_)));
    }
}
