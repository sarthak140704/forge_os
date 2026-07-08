use crate::Permission;
use serde::{Deserialize, Serialize};

/// Describes a tool sufficiently for the planner LLM to decide when to call it.
/// The `input_schema` is JSON Schema Draft 2020-12 for the tool's input object.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub permissions: Vec<Permission>,
}
