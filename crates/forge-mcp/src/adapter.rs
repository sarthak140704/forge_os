//! Adapts an MCP-hosted tool into the forge `Tool` trait.
//!
//! # Naming
//!
//! MCP tools are namespaced by their server: `mcp.<server>.<tool>`. This
//! avoids collisions with built-ins (`fs.read` etc.) and makes it obvious in
//! the event log where a call went.
//!
//! # Permissions
//!
//! We treat every MCP tool as potentially side-effectful: it goes in with
//! `FsWrite | Shell | Network` in its schema. The `PolicyEngine` decides
//! whether the specific server/tool combination is allowed — that's where
//! per-server allowlists belong, not here.
//!
//! # Result shape
//!
//! MCP `tools/call` returns a list of content parts. We flatten to:
//!
//! ```json
//! { "text": "...concatenated text parts...", "content": [...raw parts...] }
//! ```
//!
//! plus `structuredContent` if the server provided one. Downstream code
//! (execution engine → task result summariser) already stringifies JSON via
//! `serde_json::to_string`, so this shape is friendly.

use crate::client::McpClient;
use async_trait::async_trait;
use forge_domain::{Permission, ToolSchema};
use forge_tools::{Tool, ToolCtx, ToolError};
use serde_json::Value;
use std::sync::Arc;

pub struct McpToolProxy {
    /// Owning client. Shared via Arc so many proxies reuse one connection.
    client: Arc<McpClient>,
    /// Namespaced forge tool name: `mcp.<server>.<tool>`.
    qualified_name: String,
    /// Original tool name as reported by the server (used on the wire).
    remote_name: String,
    description: String,
    input_schema: Value,
    permissions: Vec<Permission>,
}

impl McpToolProxy {
    pub fn new(
        client: Arc<McpClient>,
        server_name: &str,
        remote_name: &str,
        description: String,
        input_schema: Value,
    ) -> Self {
        Self {
            client,
            qualified_name: format!("mcp.{server_name}.{remote_name}"),
            remote_name: remote_name.to_string(),
            description,
            input_schema,
            // MCP tools are treated as potentially side-effectful. Policy
            // engine narrows this down per rule.
            permissions: vec![Permission::FsRead, Permission::FsWrite, Permission::Network, Permission::Shell],
        }
    }

    pub fn qualified_name(&self) -> &str { &self.qualified_name }
}

#[async_trait]
impl Tool for McpToolProxy {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.qualified_name.clone(),
            description: if self.description.is_empty() {
                format!("MCP tool `{}` from server `{}`", self.remote_name, self.client.logical_name())
            } else {
                self.description.clone()
            },
            input_schema: self.input_schema.clone(),
            permissions: self.permissions.clone(),
        }
    }

    async fn invoke(&self, _ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let result = self.client.call_tool(&self.remote_name, input).await
            .map_err(|e| ToolError::Exec(format!("mcp call `{}` failed: {e}", self.qualified_name)))?;

        // Concatenate every text part; keep the raw list around too.
        let mut text = String::new();
        for part in &result.content {
            if let crate::protocol::ContentPart::Text { text: t } = part {
                if !text.is_empty() { text.push('\n'); }
                text.push_str(t);
            }
        }

        if result.is_error {
            return Err(ToolError::Exec(if text.is_empty() {
                format!("mcp tool `{}` reported isError=true", self.qualified_name)
            } else {
                text
            }));
        }

        // Re-serialise content for callers that want structured access.
        let raw_content = serde_json::to_value(RawContentSummary {
            text: if text.is_empty() { None } else { Some(text.clone()) },
            structured_content: result.structured_content.clone(),
        }).unwrap_or(Value::Null);

        Ok(raw_content)
    }
}

#[derive(serde::Serialize)]
struct RawContentSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "structuredContent")]
    structured_content: Option<Value>,
}
