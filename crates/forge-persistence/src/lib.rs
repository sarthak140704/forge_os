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

pub use sqlite::{
    connect, SqliteEventStore, SqliteGoalRepository, SqliteMissionRepository, SqlitePool,
    SqliteReflectionRepository, SqliteTaskRepository,
};
