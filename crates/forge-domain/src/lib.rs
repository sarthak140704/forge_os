//! Forge OS domain types.
//!
//! This crate is the leaf of the dependency graph. It has zero I/O dependencies —
//! no async runtime, no database, no HTTP client. Every other crate depends on
//! these types, so keeping them pure keeps the whole workspace fast to compile
//! and easy to test.

pub mod ids;
pub mod mission;
pub mod goal;
pub mod task;
pub mod event;
pub mod policy;
pub mod tool;

pub use ids::{EventId, GoalId, MissionId, TaskId};
pub use mission::{Mission, MissionStatus, MissionSummary};
pub use goal::{Goal, GoalStatus, GoalNode};
pub use task::{Task, TaskStatus};
pub use event::{ForgeEvent, EventEnvelope, AggregateKind};
pub use policy::{PolicyDecision, Permission};
pub use tool::{ToolSchema};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("invalid state transition: {from:?} -> {to:?}")]
    InvalidTransition { from: String, to: String },
    #[error("dependency cycle detected in mission {mission_id}")]
    DependencyCycle { mission_id: MissionId },
    #[error("goal {0} referenced but not defined")]
    UnknownGoal(GoalId),
}
