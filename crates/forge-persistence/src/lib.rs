//! SQLite-backed persistence.
//!
//! Repositories are defined as async traits; the SQLite implementations live
//! in submodules. Because the traits do not leak SQLite types, we can add a
//! PostgreSQL backend in Phase 4 without touching any domain or engine code.

pub mod migrations;
pub mod sqlite;

use async_trait::async_trait;
use forge_domain::{EventId, EventEnvelope, ForgeEvent, Goal, GoalId, Mission, MissionId, Task, TaskId};
use thiserror::Error;
use time::OffsetDateTime;

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("sql error: {0}")]
    Sql(#[from] sqlx::Error),
    #[error("json (de)serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found: {kind} {id}")]
    NotFound { kind: &'static str, id: String },
    /// Emitted by the Postgres scaffold module (Phase 4e). The trait
    /// boundary is proven but the concrete impl is future work.
    #[error("not yet implemented: {0}")]
    NotYetImplemented(&'static str),
}

#[async_trait]
pub trait EventStore: Send + Sync {
    async fn append(&self, event: &ForgeEvent, ts: OffsetDateTime) -> Result<EventId, PersistenceError>;
    async fn read_since(&self, since: Option<EventId>) -> Result<Vec<EventEnvelope>, PersistenceError>;
    async fn read_for_aggregate(&self, aggregate_id: &str) -> Result<Vec<EventEnvelope>, PersistenceError>;
}

#[async_trait]
pub trait MissionRepository: Send + Sync {
    async fn insert(&self, mission: &Mission) -> Result<(), PersistenceError>;
    async fn update(&self, mission: &Mission) -> Result<(), PersistenceError>;
    async fn get(&self, id: MissionId) -> Result<Mission, PersistenceError>;
    async fn list(&self) -> Result<Vec<Mission>, PersistenceError>;

    /// Retrieve past *terminal* missions whose title or description contains
    /// any of the given keywords, ordered by number of matches then most
    /// recent first. Used for episodic recall — injecting a summary of prior
    /// similar attempts into the planner prompt.
    ///
    /// Keywords are matched with a case-insensitive LIKE `%kw%`. Empty
    /// keyword list returns an empty vec (no recall). Terminal = Completed,
    /// Failed, or Cancelled.
    async fn search_similar(
        &self,
        keywords: &[String],
        limit: usize,
    ) -> Result<Vec<Mission>, PersistenceError>;
}

#[async_trait]
pub trait GoalRepository: Send + Sync {
    async fn insert(&self, goal: &Goal) -> Result<(), PersistenceError>;
    async fn update(&self, goal: &Goal) -> Result<(), PersistenceError>;
    async fn get(&self, id: GoalId) -> Result<Goal, PersistenceError>;
    async fn list_for_mission(&self, mission_id: MissionId) -> Result<Vec<Goal>, PersistenceError>;
}

#[async_trait]
pub trait TaskRepository: Send + Sync {
    async fn insert(&self, task: &Task) -> Result<(), PersistenceError>;
    async fn update(&self, task: &Task) -> Result<(), PersistenceError>;
    async fn get(&self, id: TaskId) -> Result<Task, PersistenceError>;
    async fn list_for_goal(&self, goal_id: GoalId) -> Result<Vec<Task>, PersistenceError>;
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReflectionRecord {
    pub mission_id: MissionId,
    pub created_at: String,
    pub outcome:    String,
    pub payload:    String,
}

#[async_trait]
pub trait ReflectionRepository: Send + Sync {
    async fn insert(
        &self,
        mission_id: MissionId,
        outcome: &str,
        payload_json: &str,
    ) -> Result<(), PersistenceError>;
    async fn list_for_mission(
        &self,
        mission_id: MissionId,
    ) -> Result<Vec<ReflectionRecord>, PersistenceError>;
}

// ---------------------------------------------------------------------------
// Phase 4a — Version-controlled skills
// ---------------------------------------------------------------------------

/// Provenance of a skill version. Governs which review flow it goes through
/// and how the curator scores it. `Handcrafted` and `Proposal` are the two
/// origins we ship in Phase 4a; `Curated` reserves space for the auto-merge
/// pipeline in Phase 4b.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillOrigin {
    /// Human-authored SKILL.md dropped into `<skills_root>/` (or the
    /// bundled defaults). No reflection produced it.
    Handcrafted,
    /// Emitted by the reflector for a specific mission, then approved.
    Proposal,
    /// Synthesized by the curator (merge, refactor). Not yet used.
    Curated,
    /// Restored from a prior sha via `rollback_skill`.
    Rollback,
}

impl SkillOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            SkillOrigin::Handcrafted => "handcrafted",
            SkillOrigin::Proposal    => "proposal",
            SkillOrigin::Curated     => "curated",
            SkillOrigin::Rollback    => "rollback",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "handcrafted" => SkillOrigin::Handcrafted,
            "proposal"    => SkillOrigin::Proposal,
            "curated"     => SkillOrigin::Curated,
            "rollback"    => SkillOrigin::Rollback,
            _             => SkillOrigin::Handcrafted,
        }
    }
}

/// One row of the append-only `skills_history` log.
///
/// Semantics:
/// - A skill named `X` is "active" iff the most-recent row for `name=X` has
///   `retired_at IS NULL`.
/// - `sha` is the SHA-256 of the SKILL.md bytes we snapshotted at promotion.
/// - `parent_sha` links a promotion back to what it replaced (None for the
///   very first promotion of a name).
/// - `retired_at` is set by `retire_skill`. Nothing ever mutates history rows.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SkillVersionRecord {
    pub id:                i64,
    pub name:              String,
    pub sha:               String,
    pub version:           String,
    pub origin:            SkillOrigin,
    pub origin_mission_id: Option<String>,
    pub parent_sha:        Option<String>,
    pub promoted_at:       String,
    pub retired_at:        Option<String>,
    pub reason:            Option<String>,
}

/// Parameters for a new promotion (create the row, don't touch content store).
#[derive(Clone, Debug)]
pub struct NewSkillVersion {
    pub name:              String,
    pub sha:               String,
    pub version:           String,
    pub origin:            SkillOrigin,
    pub origin_mission_id: Option<String>,
    pub parent_sha:        Option<String>,
    pub reason:            Option<String>,
}

#[async_trait]
pub trait SkillHistoryRepository: Send + Sync {
    /// Append a new promotion row. Returns the assigned id.
    async fn promote(&self, v: &NewSkillVersion) -> Result<i64, PersistenceError>;

    /// Mark the currently-active row for `name` as retired. No-op if
    /// nothing is active (returns Ok(false)).
    async fn retire_active(&self, name: &str, reason: &str)
        -> Result<bool, PersistenceError>;

    /// Return the currently-active row for `name`, if any.
    async fn active(&self, name: &str) -> Result<Option<SkillVersionRecord>, PersistenceError>;

    /// Return every row for `name`, newest first. Includes retired rows.
    async fn history(&self, name: &str) -> Result<Vec<SkillVersionRecord>, PersistenceError>;

    /// Return the currently-active row for every distinct name.
    async fn list_active(&self) -> Result<Vec<SkillVersionRecord>, PersistenceError>;
}

// ---------------------------------------------------------------------------
// Phase 4d — Persisted mission-execution queue.
// ---------------------------------------------------------------------------

/// A row in the mission execution queue. Multiple queue rows per mission_id
/// are allowed (e.g. after crash recovery), but at any time only ONE row per
/// mission_id should be in `Queued` or `Claimed` state — enforced by
/// `enqueue`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueStatus { Queued, Claimed, Done, Failed }

impl QueueStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            QueueStatus::Queued  => "queued",
            QueueStatus::Claimed => "claimed",
            QueueStatus::Done    => "done",
            QueueStatus::Failed  => "failed",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "claimed" => QueueStatus::Claimed,
            "done"    => QueueStatus::Done,
            "failed"  => QueueStatus::Failed,
            _         => QueueStatus::Queued,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MissionQueueRow {
    pub id:           i64,
    pub mission_id:   String,
    pub status:       QueueStatus,
    pub claimed_by:   Option<String>,
    pub claimed_at:   Option<String>,
    pub heartbeat_at: Option<String>,
    pub finished_at:  Option<String>,
    pub error:        Option<String>,
    pub enqueued_at:  String,
}

#[async_trait]
pub trait MissionQueueRepository: Send + Sync {
    /// Insert a `Queued` row for `mission_id` iff no active
    /// (Queued|Claimed) row already exists for that mission. Returns the
    /// new (or existing) row's id. Idempotent on active dupes.
    async fn enqueue(&self, mission_id: MissionId) -> Result<i64, PersistenceError>;

    /// Atomically claim the oldest queued row for a worker. Returns None
    /// when the queue is empty.
    async fn claim_next(&self, worker_id: &str) -> Result<Option<MissionQueueRow>, PersistenceError>;

    /// Update `heartbeat_at` for a claimed row. No-op if the row moved.
    async fn heartbeat(&self, id: i64) -> Result<(), PersistenceError>;

    /// Terminal state — `error` populated iff `success=false`.
    async fn finish(&self, id: i64, success: bool, error: Option<&str>) -> Result<(), PersistenceError>;

    /// Requeue any `Claimed` row whose heartbeat is older than
    /// `stale_after_secs` OR whose `heartbeat_at IS NULL` and was claimed
    /// more than `stale_after_secs` ago. Returns the number requeued.
    /// Called at boot and periodically to recover from worker crashes.
    async fn requeue_stale(&self, stale_after_secs: i64) -> Result<usize, PersistenceError>;

    /// Live snapshot: (queued_count, claimed_count).
    async fn depth(&self) -> Result<(usize, usize), PersistenceError>;

    /// Most-recent N rows (any status), newest first.
    async fn recent(&self, limit: usize) -> Result<Vec<MissionQueueRow>, PersistenceError>;
}

// ---------------------------------------------------------------------------
// Phase 4f — Organizational memory.
// ---------------------------------------------------------------------------

/// One durable fact learned across missions. `tags` is used for cheap
/// keyword-based recall in the planner prompt (LIKE-search on the JSON
/// array of lowercased strings). `retired_at` is set by the UI to hide a
/// memory without deleting it.
///
/// Phase 6a: `embedding` is the raw little-endian f32 bytes of a fixed-size
/// vector produced by whichever embedding provider is wired at boot; `None`
/// means the row was written before Phase 6a or with no provider configured,
/// in which case only the keyword search sees it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OrgMemoryRow {
    pub id:                i64,
    pub key:               String,
    pub value:             String,
    pub tags:              Vec<String>,
    pub source_mission_id: Option<String>,
    pub created_at:        String,
    pub retired_at:        Option<String>,
    #[serde(skip)]
    pub embedding:         Option<Vec<f32>>,
}

#[derive(Clone, Debug)]
pub struct NewOrgMemory {
    pub key:               String,
    pub value:             String,
    pub tags:              Vec<String>,
    pub source_mission_id: Option<MissionId>,
    /// Phase 6a: if the runtime has an embedding provider wired, planners
    /// should embed the (key, value) tuple and pass the vector here so
    /// semantic recall can find it. `None` still works — the row just
    /// won't be reachable via semantic search until backfilled.
    pub embedding:         Option<Vec<f32>>,
}

#[async_trait]
pub trait OrgMemoryRepository: Send + Sync {
    /// Append a new memory row.
    async fn insert(&self, m: &NewOrgMemory) -> Result<i64, PersistenceError>;

    /// Soft-delete: sets retired_at. No-op on already-retired or missing.
    async fn retire(&self, id: i64) -> Result<bool, PersistenceError>;

    /// All non-retired rows, newest first.
    async fn list_active(&self, limit: usize) -> Result<Vec<OrgMemoryRow>, PersistenceError>;

    /// Return up to `limit` non-retired rows whose key/value/tags contain
    /// ANY of the given keywords (case-insensitive), scored by match
    /// count. Empty keyword list returns an empty vec.
    async fn search(&self, keywords: &[String], limit: usize) -> Result<Vec<OrgMemoryRow>, PersistenceError>;

    /// Phase 6a — semantic search.
    ///
    /// Fetches every non-retired row that has a stored embedding of the
    /// same dimension as `query`, then computes cosine similarity in
    /// Rust and returns the top `limit` (score, row) pairs, best first.
    ///
    /// Falls back to an empty vec if no embedded rows match the query's
    /// dimension. Callers are expected to fall back to keyword `search`
    /// in that case so the personal-project MVP still recalls something.
    async fn semantic_search(
        &self,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<(f32, OrgMemoryRow)>, PersistenceError>;

    /// Phase 6a — backfill embedding on an existing row. Used by the
    /// UI's "re-embed" action and by lazy backfill in the mission
    /// service. No-op returning `false` if the row is missing.
    async fn set_embedding(&self, id: i64, embedding: &[f32]) -> Result<bool, PersistenceError>;
}

pub use sqlite::{
    connect, SqliteEventStore, SqliteGoalRepository, SqliteMissionRepository,
    SqliteMissionQueueRepository, SqliteOrgMemoryRepository, SqlitePool,
    SqliteReflectionRepository, SqliteSkillHistoryRepository, SqliteTaskRepository,
};

/// Postgres scaffold for the Phase 4e persistence swap point.
/// Compiles as a stub that returns `NotYetImplemented` from `connect`.
/// See `crates/forge-persistence/src/postgres.rs` for the roadmap.
pub mod postgres;

// ---------------------------------------------------------------------------
// Phase 4e — Persistence composite handle.
// ---------------------------------------------------------------------------

/// One handle bundling every repository trait. Runtime wires this once at
/// boot; nothing else in the codebase touches concrete `SqlitePool` types.
///
/// To swap in Postgres, implement a `PersistenceHandles::postgres(url)`
/// constructor that mirrors `sqlite(url)`. Everything downstream is
/// already trait-based.
#[derive(Clone)]
pub struct PersistenceHandles {
    pub pool_kind: PoolKind,
    pub events:      std::sync::Arc<dyn EventStore>,
    pub missions:    std::sync::Arc<dyn MissionRepository>,
    pub goals:       std::sync::Arc<dyn GoalRepository>,
    pub tasks:       std::sync::Arc<dyn TaskRepository>,
    pub reflections: std::sync::Arc<dyn ReflectionRepository>,
    pub skills:      std::sync::Arc<dyn SkillHistoryRepository>,
    pub queue:       std::sync::Arc<dyn MissionQueueRepository>,
    pub memory:      std::sync::Arc<dyn OrgMemoryRepository>,
    /// The raw SQLite pool, if this handle bundle was built via
    /// `sqlite()`. `None` when the backend is Postgres — callers that
    /// need to run raw SQLite queries (e.g. shadow-git snapshots) should
    /// use this to opt into single-backend behaviour.
    pub sqlite_pool: Option<SqlitePool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolKind { Sqlite, Postgres }

impl PersistenceHandles {
    /// Build the full trait bundle backed by SQLite at the given URL.
    /// This is the normal boot path.
    pub async fn sqlite(url: &str) -> Result<Self, PersistenceError> {
        let pool = connect(url).await?;
        use std::sync::Arc;
        Ok(Self {
            pool_kind: PoolKind::Sqlite,
            events:      Arc::new(SqliteEventStore::new(pool.clone())),
            missions:    Arc::new(SqliteMissionRepository::new(pool.clone())),
            goals:       Arc::new(SqliteGoalRepository::new(pool.clone())),
            tasks:       Arc::new(SqliteTaskRepository::new(pool.clone())),
            reflections: Arc::new(SqliteReflectionRepository::new(pool.clone())),
            skills:      Arc::new(SqliteSkillHistoryRepository::new(pool.clone())),
            queue:       Arc::new(SqliteMissionQueueRepository::new(pool.clone())),
            memory:      Arc::new(SqliteOrgMemoryRepository::new(pool.clone())),
            sqlite_pool: Some(pool),
        })
    }

    /// Build the full trait bundle backed by Postgres. Currently returns
    /// a `NotYetImplemented` error — the trait boundary is proven, the
    /// concrete impl is Phase 5 work.
    pub async fn postgres(url: &str) -> Result<Self, PersistenceError> {
        let _ = postgres::connect(url).await?;
        Err(PersistenceError::NotYetImplemented("postgres persistence backend"))
    }

    /// Dispatch by URL scheme:
    ///   sqlite://…, file:…, or a bare path → SQLite
    ///   postgres://…, postgresql://…       → Postgres (stub)
    pub async fn open(url: &str) -> Result<Self, PersistenceError> {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            Self::postgres(url).await
        } else {
            Self::sqlite(url).await
        }
    }
}
