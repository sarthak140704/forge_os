//! Model Context Protocol (MCP) client for Forge OS.
//!
//! # Why MCP
//!
//! The agent spec is explicit: *"Prefer MCP whenever available."* MCP is
//! Anthropic's open protocol for LLM tool servers — a single client wired to a
//! stdio subprocess unlocks the entire community ecosystem (filesystem, git,
//! github, playwright, brave-search, postgres, sqlite, ...). One protocol,
//! dozens of tools, zero per-tool code in this repo.
//!
//! # Architecture
//!
//! ```text
//!   config/mcp.yaml
//!         │
//!         ▼
//!   McpRegistry ── spawns ──▶ McpServer (child process, stdio)
//!         │                       │
//!         │                       │ JSON-RPC 2.0 (line-delimited JSON)
//!         │                       │
//!         │      handshake ───▶ initialize / initialized
//!         │      tools/list  ───▶ list of {name, description, input_schema}
//!         │
//!         ▼
//!   for each tool: register an McpToolProxy with the forge ToolRegistry.
//!   Name becomes `mcp.<server>.<tool>`; input_schema flows straight through.
//!   invoke() forwards to tools/call and returns the content payload.
//! ```
//!
//! # Scope of this module
//!
//! * **Stdio transport only.** SSE / websocket are a future story.
//! * **Tools only.** MCP also exposes resources and prompts; those come later.
//! * **Best-effort boot.** A failing MCP server never blocks the runtime — it
//!   emits `McpServerFailed` and the rest of Forge keeps working.

pub mod adapter;
pub mod client;
pub mod config;
pub mod protocol;
pub mod registry;
pub mod transport;

pub use adapter::McpToolProxy;
pub use client::{McpClient, McpServerInfo, McpToolDescriptor};
pub use config::{McpConfig, McpServerConfig, McpTransportKind};
pub use protocol::{McpError, McpMethod};
pub use registry::{McpRegistry, McpServerStatus};
