use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    FsRead,
    FsWrite,
    Shell,
    Network,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PolicyDecision {
    Allow,
    Deny { rule: String, reason: String },
    RequireApproval { rule: String, reason: String },
}

impl PolicyDecision {
    pub fn is_allow(&self) -> bool {
        matches!(self, PolicyDecision::Allow)
    }
    pub fn is_deny(&self) -> bool {
        matches!(self, PolicyDecision::Deny { .. })
    }
}
