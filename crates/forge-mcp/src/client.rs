//! High-level MCP client.
//!
//! One `McpClient` == one live connection to one MCP server. It owns the
//! transport, a request id counter, and a map of in-flight requests keyed by
//! id. A single background task pumps frames off the transport and either
//! completes an in-flight request (via a oneshot sender) or logs and drops
//! server-initiated notifications.
//!
//! Concurrency model:
//! * Callers `.await` on `call()` — the future completes when the pump task
//!   receives the matching response.
//! * `call()` is `&self`, so many callers can hold Arc<McpClient> and issue
//!   requests concurrently — the id counter uses `AtomicU64` and the pending
//!   map is a `DashMap`.

use crate::protocol::*;
use crate::transport::{Frame, StdioTransport};
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, Mutex};

/// Timeout for the `initialize` handshake. Longer than a normal call because
/// stdio servers often do lazy loading on the first request.
const INIT_TIMEOUT: Duration = Duration::from_secs(15);

/// Default timeout for a `tools/list` or `tools/call`. Individual servers
/// may need longer for genuinely slow operations (e.g. running a shell
/// command); we'll expose a per-call override once we have a use case.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Snapshot of what a server told us about itself during `initialize`.
#[derive(Clone, Debug)]
pub struct McpServerInfo {
    /// Logical name from `config/mcp.yaml`, e.g. `"filesystem"`.
    pub logical_name: String,
    /// `name` field from server_info, if any.
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    pub protocol_version: Option<String>,
    pub supports_tools: bool,
}

/// Metadata for one tool the server exposes.
#[derive(Clone, Debug)]
pub struct McpToolDescriptor {
    /// Original name as reported by the server (unqualified).
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// An open MCP client, post-handshake.
pub struct McpClient {
    logical_name: String,
    info: McpServerInfo,
    next_id: AtomicU64,
    pending: Arc<DashMap<u64, oneshot::Sender<Result<serde_json::Value, McpError>>>>,
    writer: Arc<Mutex<crate::transport::FrameWriter>>,
    tools: Vec<McpToolDescriptor>,
}

impl McpClient {
    /// Perform the MCP handshake and list tools. On success the returned
    /// client is ready for `call_tool`.
    pub async fn connect(logical_name: String, transport: StdioTransport) -> Result<Self, McpError> {
        let StdioTransport { writer, rx, child } = transport;
        let writer = Arc::new(Mutex::new(writer));
        let pending: Arc<DashMap<u64, oneshot::Sender<Result<serde_json::Value, McpError>>>> =
            Arc::new(DashMap::new());

        // The child guard travels with the pump task so the child stays alive
        // for as long as we're pumping frames from it.
        let child_guard = Arc::new(child);

        // Spawn the pump task.
        let pending_for_pump = pending.clone();
        let name_for_pump = logical_name.clone();
        tokio::spawn(async move {
            let _keep_alive = child_guard;
            pump(rx, pending_for_pump, name_for_pump).await;
        });

        // Id counter shared by handshake and steady-state calls.
        let counter = AtomicU64::new(0);

        // Handshake: initialize.
        let init_params = serde_json::to_value(InitializeParams::default_for_forge())?;
        let raw = call_raw(
            &writer, &pending, &counter,
            McpMethod::Initialize, Some(init_params), INIT_TIMEOUT,
        )
        .await?;
        let init: InitializeResult = serde_json::from_value(raw)
            .map_err(|e| McpError::InvalidResponse { method: "initialize", reason: e.to_string() })?;

        // Notify server we're ready.
        {
            let mut w = writer.lock().await;
            let notify = JsonRpcRequest::notify(McpMethod::NotificationsInitialized.as_str(), None);
            w.write_json(&serde_json::to_value(&notify)?).await?;
        }

        let supports_tools = init
            .capabilities
            .as_ref()
            .and_then(|c| c.get("tools"))
            .is_some();

        let info = McpServerInfo {
            logical_name: logical_name.clone(),
            server_name: init.server_info.as_ref().and_then(|s| s.name.clone()),
            server_version: init.server_info.as_ref().and_then(|s| s.version.clone()),
            protocol_version: init.protocol_version.clone(),
            supports_tools,
        };
        tracing::info!(
            mcp = %logical_name,
            server = ?info.server_name,
            proto = ?info.protocol_version,
            supports_tools,
            "mcp handshake complete"
        );

        // List tools (best-effort — a server without tools capability just
        // returns an empty list or a method_not_found we treat as empty).
        let tools = match call_raw(
            &writer, &pending, &counter,
            McpMethod::ToolsList, Some(serde_json::json!({})), CALL_TIMEOUT,
        ).await {
            Ok(raw) => {
                let listed: ToolsListResult = serde_json::from_value(raw)
                    .map_err(|e| McpError::InvalidResponse { method: "tools/list", reason: e.to_string() })?;
                listed.tools.into_iter().map(|t| McpToolDescriptor {
                    name: t.name.clone(),
                    description: t.description.unwrap_or_default(),
                    input_schema: t.input_schema.unwrap_or_else(|| serde_json::json!({
                        "type": "object", "properties": {}
                    })),
                }).collect()
            }
            Err(McpError::Rpc { code: -32601, .. }) => {
                tracing::info!(mcp = %logical_name, "server does not implement tools/list");
                Vec::new()
            }
            Err(e) => return Err(e),
        };

        Ok(Self {
            logical_name,
            info,
            next_id: counter,
            pending,
            writer,
            tools,
        })
    }

    pub fn info(&self) -> &McpServerInfo { &self.info }
    pub fn logical_name(&self) -> &str { &self.logical_name }
    pub fn tools(&self) -> &[McpToolDescriptor] { &self.tools }

    /// Invoke a tool on the server. Returns the raw `tools/call` result.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolsCallResult, McpError> {
        let params = serde_json::to_value(ToolsCallParams {
            name: name.to_string(),
            arguments: Some(arguments),
        })?;
        let raw = call_raw(
            &self.writer, &self.pending, &self.next_id,
            McpMethod::ToolsCall, Some(params), CALL_TIMEOUT,
        )
        .await?;
        let parsed: ToolsCallResult = serde_json::from_value(raw)
            .map_err(|e| McpError::InvalidResponse { method: "tools/call", reason: e.to_string() })?;
        Ok(parsed)
    }
}

/// Background task: read frames off the transport, complete pending
/// requests, ignore server notifications.
async fn pump(
    mut rx: tokio::sync::mpsc::Receiver<Frame>,
    pending: Arc<DashMap<u64, oneshot::Sender<Result<serde_json::Value, McpError>>>>,
    name: String,
) {
    while let Some(frame) = rx.recv().await {
        match frame {
            Frame::Line(line) => {
                let resp: JsonRpcResponse = match serde_json::from_str(&line) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(mcp = %name, error = %e, raw = %line, "unparseable frame");
                        continue;
                    }
                };
                if resp.is_notification() {
                    tracing::debug!(mcp = %name, method = ?resp.method, "server notification ignored");
                    continue;
                }
                let Some(id) = resp.id_u64() else {
                    tracing::warn!(mcp = %name, raw = %line, "response missing id");
                    continue;
                };
                let Some((_, tx)) = pending.remove(&id) else {
                    tracing::warn!(mcp = %name, id, "response for unknown id");
                    continue;
                };
                if let Some(err) = resp.error {
                    let _ = tx.send(Err(err.into()));
                } else if let Some(result) = resp.result {
                    let _ = tx.send(Ok(result));
                } else {
                    let _ = tx.send(Ok(serde_json::Value::Null));
                }
            }
            Frame::Closed => {
                tracing::info!(mcp = %name, "server closed");
                // Fail every pending request so callers don't hang forever.
                let ids: Vec<u64> = pending.iter().map(|kv| *kv.key()).collect();
                for id in ids {
                    if let Some((_, tx)) = pending.remove(&id) {
                        let _ = tx.send(Err(McpError::ServerClosed { phase: "runtime" }));
                    }
                }
                break;
            }
        }
    }
}

/// Send a JSON-RPC request and await its response with a timeout.
///
/// Kept as a free function so the handshake can use it before the client is
/// fully constructed.
async fn call_raw(
    writer: &Arc<Mutex<crate::transport::FrameWriter>>,
    pending: &Arc<DashMap<u64, oneshot::Sender<Result<serde_json::Value, McpError>>>>,
    counter: &AtomicU64,
    method: McpMethod,
    params: Option<serde_json::Value>,
    timeout: Duration,
) -> Result<serde_json::Value, McpError> {
    let id = counter.fetch_add(1, Ordering::Relaxed) + 1;
    let (tx, rx) = oneshot::channel();
    pending.insert(id, tx);

    let req = JsonRpcRequest::call(id, method.as_str(), params);
    let frame = serde_json::to_value(&req)?;
    {
        let mut w = writer.lock().await;
        if let Err(e) = w.write_json(&frame).await {
            pending.remove(&id);
            return Err(e);
        }
    }

    let millis = timeout.as_millis() as u64;
    let method_name: &'static str = method.as_str();
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(res)) => res,
        Ok(Err(_)) => Err(McpError::ChannelClosed),
        Err(_) => {
            pending.remove(&id);
            Err(McpError::Timeout { method: method_name, millis })
        }
    }
}
