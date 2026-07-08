//! JSON-RPC 2.0 wire types + MCP-specific method payloads.
//!
//! MCP uses **JSON-RPC 2.0** over line-delimited JSON. That means each frame
//! is one line of JSON with a mandatory `"jsonrpc": "2.0"` marker and either
//! an `id` (request/response) or no `id` (notification).
//!
//! We only implement the subset Forge actually needs:
//!
//! * `initialize` (client→server request, response has server capabilities)
//! * `notifications/initialized` (client→server notification, no id)
//! * `tools/list` (client→server request, response is list of tools)
//! * `tools/call` (client→server request, response is the tool result)
//!
//! Everything else (resources, prompts, sampling, roots, notifications from
//! server) is currently ignored — we skip incoming notifications and log
//! anything we don't recognise.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// The MCP protocol version we advertise during `initialize`. The MCP spec
/// evolves; `2024-11-05` is the stable version most public servers speak.
/// If a server negotiates a different one it goes into `McpServerInfo`.
pub const CLIENT_PROTOCOL_VERSION: &str = "2024-11-05";

/// JSON-RPC 2.0 method names we care about.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum McpMethod {
    Initialize,
    NotificationsInitialized,
    ToolsList,
    ToolsCall,
}

impl McpMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            McpMethod::Initialize => "initialize",
            McpMethod::NotificationsInitialized => "notifications/initialized",
            McpMethod::ToolsList => "tools/list",
            McpMethod::ToolsCall => "tools/call",
        }
    }
}

/// A JSON-RPC 2.0 request frame written to the server.
#[derive(Debug, Serialize)]
pub struct JsonRpcRequest<'a> {
    pub jsonrpc: &'static str,
    /// Request id. Omit on notifications; MCP only defines a few
    /// notifications from the client side (`notifications/initialized`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl<'a> JsonRpcRequest<'a> {
    pub fn call(id: u64, method: &'a str, params: Option<Value>) -> Self {
        Self { jsonrpc: "2.0", id: Some(id), method, params }
    }
    pub fn notify(method: &'a str, params: Option<Value>) -> Self {
        Self { jsonrpc: "2.0", id: None, method, params }
    }
}

/// A JSON-RPC 2.0 response frame received from the server.
///
/// Either `result` or `error` is set. `id` correlates back to the request.
#[derive(Debug, Deserialize)]
pub struct JsonRpcResponse {
    #[allow(dead_code)]
    pub jsonrpc: Option<String>,
    /// Absent on notifications; present on responses to our requests.
    /// Servers may echo any of the id types we sent; we always send u64.
    pub id: Option<Value>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<JsonRpcErrorObj>,
    /// If neither `id` nor `result` nor `error` is present but `method` is,
    /// this frame is a server-initiated notification (e.g.
    /// `notifications/tools/list_changed`). We currently ignore these.
    #[serde(default)]
    pub method: Option<String>,
}

impl JsonRpcResponse {
    /// True when this frame is a server-to-client notification rather than
    /// a response to one of our requests.
    pub fn is_notification(&self) -> bool {
        self.id.is_none() && self.method.is_some()
    }

    /// Extract the numeric id we originally sent. Servers spec-legally may
    /// return the id in various JSON shapes; we tolerate a bare u64 or a
    /// string that parses to u64.
    pub fn id_u64(&self) -> Option<u64> {
        match &self.id {
            Some(Value::Number(n)) => n.as_u64(),
            Some(Value::String(s)) => s.parse().ok(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JsonRpcErrorObj {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

// ---- MCP-specific payloads ----

/// Client capabilities announced during `initialize`. We keep this minimal:
/// we implement none of the optional client-side capabilities (sampling,
/// roots, elicitation) — servers that need them will simply not use them.
#[derive(Debug, Serialize, Default)]
pub struct ClientCapabilities {}

#[derive(Debug, Serialize)]
pub struct ClientInfo {
    pub name: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Serialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: &'static str,
    pub capabilities: ClientCapabilities,
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
}

impl InitializeParams {
    pub fn default_for_forge() -> Self {
        Self {
            protocol_version: CLIENT_PROTOCOL_VERSION,
            capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "forge-os",
                version: env!("CARGO_PKG_VERSION"),
            },
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion", default)]
    pub protocol_version: Option<String>,
    #[serde(rename = "serverInfo", default)]
    pub server_info: Option<ServerInfo>,
    /// Free-form: only observed to check for the `tools` key.
    #[serde(default)]
    pub capabilities: Option<Value>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerInfo {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ToolsListResult {
    pub tools: Vec<ToolDescriptor>,
}

#[derive(Debug, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema for the tool's input. MCP does not mandate a dialect;
    /// we forward it verbatim to the LLM.
    #[serde(rename = "inputSchema", default)]
    pub input_schema: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct ToolsCallParams {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

/// Result of `tools/call`. MCP returns a list of content parts (text, image,
/// resource) plus an `isError` flag. We flatten this to a JSON value for
/// consumption by forge_tools — text parts are joined, everything else is
/// preserved as-is.
#[derive(Debug, Deserialize)]
pub struct ToolsCallResult {
    #[serde(default)]
    pub content: Vec<ContentPart>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
    /// Newer MCP servers return a JSON payload here. Preserved verbatim so
    /// callers can access structured data alongside the text summary.
    #[serde(default, rename = "structuredContent")]
    pub structured_content: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Image {
        #[serde(default)] data: Option<String>,
        #[serde(default, rename = "mimeType")] mime_type: Option<String>,
    },
    #[serde(other)]
    Other,
}

// ---- error type used across the crate ----

#[derive(Debug, Error)]
pub enum McpError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("server closed connection during {phase}")]
    ServerClosed { phase: &'static str },
    #[error("request `{method}` timed out after {millis}ms")]
    Timeout { method: &'static str, millis: u64 },
    #[error("channel closed")]
    ChannelClosed,
    #[error("tool `{tool}` reported an error: {message}")]
    ToolFailed { tool: String, message: String },
    #[error("invalid response for {method}: {reason}")]
    InvalidResponse { method: &'static str, reason: String },
    #[error("server `{server}` not started")]
    ServerNotFound { server: String },
    #[error("mcp is disabled")]
    Disabled,
}

impl From<JsonRpcErrorObj> for McpError {
    fn from(e: JsonRpcErrorObj) -> Self {
        McpError::Rpc { code: e.code, message: e.message }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_without_id_when_notification() {
        let r = JsonRpcRequest::notify("notifications/initialized", None);
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("\"id\""), "notifications must not carry an id: {s}");
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains("\"method\":\"notifications/initialized\""));
    }

    #[test]
    fn request_serializes_with_id_when_call() {
        let r = JsonRpcRequest::call(42, "tools/list", None);
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"id\":42"));
    }

    #[test]
    fn response_parses_success() {
        let raw = r#"{"jsonrpc":"2.0","id":7,"result":{"tools":[]}}"#;
        let r: JsonRpcResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(r.id_u64(), Some(7));
        assert!(r.result.is_some());
        assert!(r.error.is_none());
        assert!(!r.is_notification());
    }

    #[test]
    fn response_parses_error() {
        let raw = r#"{"jsonrpc":"2.0","id":8,"error":{"code":-32601,"message":"method not found"}}"#;
        let r: JsonRpcResponse = serde_json::from_str(raw).unwrap();
        let err = r.error.unwrap();
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "method not found");
    }

    #[test]
    fn response_parses_notification() {
        let raw = r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;
        let r: JsonRpcResponse = serde_json::from_str(raw).unwrap();
        assert!(r.is_notification());
        assert_eq!(r.method.as_deref(), Some("notifications/tools/list_changed"));
    }

    #[test]
    fn tool_descriptor_tolerates_missing_schema() {
        let raw = r#"{"name":"echo"}"#;
        let t: ToolDescriptor = serde_json::from_str(raw).unwrap();
        assert_eq!(t.name, "echo");
        assert!(t.input_schema.is_none());
    }

    #[test]
    fn tools_call_result_text_only() {
        let raw = r#"{"content":[{"type":"text","text":"hi"}]}"#;
        let r: ToolsCallResult = serde_json::from_str(raw).unwrap();
        assert_eq!(r.content.len(), 1);
        matches!(r.content[0], ContentPart::Text { .. });
        assert!(!r.is_error);
    }

    #[test]
    fn tools_call_result_flags_error() {
        let raw = r#"{"content":[{"type":"text","text":"nope"}],"isError":true}"#;
        let r: ToolsCallResult = serde_json::from_str(raw).unwrap();
        assert!(r.is_error);
    }

    #[test]
    fn content_part_unknown_type_becomes_other() {
        let raw = r#"{"type":"resource","uri":"foo://bar"}"#;
        let p: ContentPart = serde_json::from_str(raw).unwrap();
        matches!(p, ContentPart::Other);
    }
}
