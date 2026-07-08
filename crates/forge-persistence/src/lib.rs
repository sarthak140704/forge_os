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

pub use sqlite::{
    connect, SqliteEventStore, SqliteGoalRepository, SqliteMissionRepository, SqlitePool,
    SqliteReflectionRepository, SqliteSkillHistoryRepository, SqliteTaskRepository,
};
