//! Fleet of MCP clients: spawn every configured server on boot, hold the
//! handles, expose the resulting tools as an Arc<dyn Tool> list ready to be
//! registered with the forge `ToolRegistry`.
//!
//! Failure is non-fatal: any single server that fails to start emits an
//! `Err(name, error)` into the return alongside the successful clients.
//! Callers turn those into `McpServerFailed` events.

use crate::adapter::McpToolProxy;
use crate::client::McpClient;
use crate::config::{McpConfig, McpTransportKind};
use crate::protocol::McpError;
use crate::transport::StdioTransport;
use forge_tools::Tool;
use std::sync::Arc;

/// Status snapshot for one server after the boot pass.
#[derive(Clone, Debug)]
pub enum McpServerStatus {
    Started {
        name: String,
        tools: Vec<String>,
    },
    Failed {
        name: String,
        error: String,
    },
    Disabled {
        name: String,
    },
}

impl McpServerStatus {
    pub fn name(&self) -> &str {
        match self {
            McpServerStatus::Started { name, .. }
            | McpServerStatus::Failed { name, .. }
            | McpServerStatus::Disabled { name } => name,
        }
    }
}

/// The outcome of `McpRegistry::start`: a live registry plus a per-server
/// status list so the runtime can log/emit events.
pub struct McpBootReport {
    pub registry: McpRegistry,
    pub statuses: Vec<McpServerStatus>,
    /// Tools flattened across every started server, already
    /// forge-namespaced (`mcp.<server>.<tool>`), ready for
    /// `ToolRegistry::register`.
    pub tools: Vec<Arc<dyn Tool>>,
}

/// Owns every live MCP client. Dropping the registry drops the clients,
/// which drops the transports, which kills the child processes.
#[derive(Default)]
pub struct McpRegistry {
    clients: Vec<Arc<McpClient>>,
}

impl McpRegistry {
    /// Attempt to start every enabled server in `config`. Errors surface via
    /// the status list — the returned registry always contains whatever did
    /// come up.
    pub async fn start(config: &McpConfig) -> McpBootReport {
        let mut clients: Vec<Arc<McpClient>> = Vec::new();
        let mut statuses: Vec<McpServerStatus> = Vec::new();
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

        for srv in &config.servers {
            if !srv.enabled {
                statuses.push(McpServerStatus::Disabled { name: srv.name.clone() });
                continue;
            }
            match srv.transport {
                McpTransportKind::Stdio => {
                    match StdioTransport::spawn(&srv.command, &srv.args, &srv.env, srv.cwd.as_ref()) {
                        Ok(transport) => {
                            match McpClient::connect(srv.name.clone(), transport).await {
                                Ok(client) => {
                                    let client = Arc::new(client);
                                    let tool_names: Vec<String> = client
                                        .tools()
                                        .iter()
                                        .map(|t| format!("mcp.{}.{}", client.logical_name(), t.name))
                                        .collect();
                                    for t in client.tools() {
                                        let proxy = McpToolProxy::new(
                                            client.clone(),
                                            client.logical_name(),
                                            &t.name,
                                            t.description.clone(),
                                            t.input_schema.clone(),
                                        );
                                        tools.push(Arc::new(proxy));
                                    }
                                    statuses.push(McpServerStatus::Started {
                                        name: srv.name.clone(),
                                        tools: tool_names,
                                    });
                                    clients.push(client);
                                }
                                Err(e) => {
                                    tracing::warn!(mcp = %srv.name, error = %e, "mcp handshake failed");
                                    statuses.push(McpServerStatus::Failed {
                                        name: srv.name.clone(),
                                        error: e.to_string(),
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(mcp = %srv.name, error = %e, "mcp spawn failed");
                            statuses.push(McpServerStatus::Failed {
                                name: srv.name.clone(),
                                error: e.to_string(),
                            });
                        }
                    }
                }
            }
        }

        McpBootReport {
            registry: McpRegistry { clients },
            statuses,
            tools,
        }
    }

    pub fn clients(&self) -> &[Arc<McpClient>] { &self.clients }

    /// Look up a client by logical (config) name.
    pub fn get(&self, name: &str) -> Option<Arc<McpClient>> {
        self.clients.iter().find(|c| c.logical_name() == name).cloned()
    }
}

impl std::fmt::Debug for McpRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpRegistry")
            .field("clients", &self.clients.iter().map(|c| c.logical_name()).collect::<Vec<_>>())
            .finish()
    }
}

// Result helper: not currently needed at call sites but kept so the runtime
// can extend to hot-reload / restart later.
pub type McpResult<T> = Result<T, McpError>;
