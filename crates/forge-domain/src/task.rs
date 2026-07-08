use crate::{GoalId, TaskId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    PendingApproval,
    Running,
    Completed,
    Failed,
    Cancelled,
    Denied,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub goal_id: GoalId,
    pub tool: String,
    pub input: serde_json::Value,
    pub status: TaskStatus,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub attempts: u8,
}

impl Task {
    pub fn new(goal_id: GoalId, tool: impl Into<String>, input: serde_json::Value) -> Self {
        Self {
            id: TaskId::new(),
            goal_id,
            tool: tool.into(),
            input,
            status: TaskStatus::Pending,
            result: None,
            error: None,
            attempts: 0,
        }
    }
}
