use crate::{DomainError, GoalId, MissionId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MissionStatus {
    Draft,
    Planning,
    Ready,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl MissionStatus {
    pub fn can_transition(&self, next: &MissionStatus) -> bool {
        use MissionStatus::*;
        matches!(
            (self, next),
            (Draft,     Planning) |
            (Planning,  Ready)     | (Planning, Failed) | (Planning, Cancelled) |
            (Ready,     Running)   | (Ready, Cancelled) |
            (Running,   Paused)    | (Running, Completed) | (Running, Failed) | (Running, Cancelled) |
            (Paused,    Running)   | (Paused, Cancelled) |
            // Re-open a terminal mission for extension via `MissionService::extend`.
            // Going back to `Draft` restarts the plan → ready → running cycle
            // while preserving already-persisted goals/tasks.
            (Completed, Draft)     | (Failed, Draft) | (Cancelled, Draft)
        )
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, MissionStatus::Completed | MissionStatus::Failed | MissionStatus::Cancelled)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mission {
    pub id: MissionId,
    pub title: String,
    pub description: String,
    pub status: MissionStatus,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub goals: Vec<GoalId>,
}

impl Mission {
    pub fn new_draft(title: impl Into<String>, description: impl Into<String>) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id: MissionId::new(),
            title: title.into(),
            description: description.into(),
            status: MissionStatus::Draft,
            created_at: now,
            updated_at: now,
            goals: Vec::new(),
        }
    }

    pub fn transition_to(&mut self, next: MissionStatus) -> Result<(), DomainError> {
        if !self.status.can_transition(&next) {
            return Err(DomainError::InvalidTransition {
                from: format!("{:?}", self.status),
                to: format!("{:?}", next),
            });
        }
        self.status = next;
        self.updated_at = OffsetDateTime::now_utc();
        Ok(())
    }
}

/// Lightweight projection for lists / dashboards. Cheaper to serialize than the
/// full Mission.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MissionSummary {
    pub id: MissionId,
    pub title: String,
    pub status: MissionStatus,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub goal_count: usize,
    pub completed_goal_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_and_invalid_transitions() {
        let mut m = Mission::new_draft("t", "d");
        assert!(m.transition_to(MissionStatus::Planning).is_ok());
        assert!(m.transition_to(MissionStatus::Ready).is_ok());
        // Ready cannot jump straight to Paused
        assert!(m.transition_to(MissionStatus::Paused).is_err());
        assert!(m.transition_to(MissionStatus::Running).is_ok());
        assert!(m.transition_to(MissionStatus::Completed).is_ok());
        assert!(m.status.is_terminal());
    }
}
