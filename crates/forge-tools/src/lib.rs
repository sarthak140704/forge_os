//! Tool runtime.
//!
//! A tool is a small, single-purpose capability that the planner can compose
//! into tasks. Tools are intentionally *granular* — `fs.read`, `shell.run`,
//! `code.search` — rather than coarse-grained (`deploy()`). This matches the
//! spec's guidance and lets the policy engine reason about intent at a
//! fine-grained level.

use async_trait::async_trait;
use forge_domain::{Permission, ToolSchema};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;

pub mod builtins;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("invalid input for tool `{tool}`: {reason}")]
    InvalidInput { tool: String, reason: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("execution error: {0}")]
    Exec(String),
    #[error("policy denied execution of tool `{0}`")]
    PolicyDenied(String),
}

/// Context passed to every tool invocation. Currently carries the workspace
/// root — Phase-2 will add session id, current mission, secrets store, etc.
#[derive(Clone, Debug)]
pub struct ToolCtx {
    pub workspace_root: PathBuf,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;

    async fn invoke(
        &self,
        ctx: &ToolCtx,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError>;
}

/// A concrete tool registry — a map from tool name to boxed tool.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self { Self::default() }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.schema().name.clone(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.values().map(|t| t.schema()).collect()
    }

    /// All registered tool names. Used by the Phase 4b skill validator to
    /// hard-reject proposals that reference tools the runtime doesn't have.
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub tool: String,
    pub input: serde_json::Value,
    pub permissions: Vec<Permission>,
}
