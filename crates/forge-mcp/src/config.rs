//! Config loading for MCP servers.
//!
//! Users describe their MCP servers in `mcp.yaml`:
//!
//! ```yaml
//! servers:
//!   - name: filesystem
//!     transport: stdio
//!     command: npx
//!     args: ["-y", "@modelcontextprotocol/server-filesystem", "${workspace_root}"]
//!     env: {}
//!     enabled: true
//! ```
//!
//! `${workspace_root}` is expanded at load time so servers get the right
//! sandbox root. Missing config files or bad entries never crash — they
//! log a warning and fall through to an empty list.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Root of the MCP config document.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

impl McpConfig {
    pub fn empty() -> Self { Self::default() }

    /// Load and substitute placeholders. Returns `Ok(empty)` if the file
    /// doesn't exist — MCP is opt-in.
    pub fn load_or_empty(path: &Path, workspace_root: &Path) -> Self {
        if !path.exists() {
            return Self::empty();
        }
        match std::fs::read_to_string(path) {
            Ok(raw) => match serde_yaml::from_str::<McpConfig>(&raw) {
                Ok(mut cfg) => {
                    for s in &mut cfg.servers {
                        s.substitute(workspace_root);
                    }
                    cfg
                }
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "mcp config parse failed; ignoring");
                    Self::empty()
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "mcp config read failed; ignoring");
                Self::empty()
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default = "default_transport")]
    pub transport: McpTransportKind,
    /// Command to spawn (e.g. `npx`, `python`, `/usr/local/bin/mcp-fs`).
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Working directory. Rare — most MCP servers are stateless.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool { true }
fn default_transport() -> McpTransportKind { McpTransportKind::Stdio }

/// Transport family. Only stdio is implemented; sse/websocket land in a
/// later phase (per the spec's "Prefer MCP whenever available" note we
/// covered the ecosystem's dominant case first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpTransportKind {
    Stdio,
}

impl McpServerConfig {
    /// Expand `${workspace_root}` in `args`, `env` values, and `cwd`. Any
    /// other placeholders are left alone so future extensions can add them
    /// non-breakingly.
    fn substitute(&mut self, workspace_root: &Path) {
        let ws = workspace_root.display().to_string();
        let sub = |s: &String| s.replace("${workspace_root}", &ws);
        self.args = self.args.iter().map(&sub).collect();
        for v in self.env.values_mut() {
            *v = sub(v);
        }
        if let Some(cwd) = &self.cwd {
            let expanded = sub(&cwd.display().to_string());
            self.cwd = Some(PathBuf::from(expanded));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_yaml() {
        let raw = r#"
servers:
  - name: filesystem
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "${workspace_root}"]
"#;
        let mut cfg: McpConfig = serde_yaml::from_str(raw).unwrap();
        assert_eq!(cfg.servers.len(), 1);
        let s = &cfg.servers[0];
        assert!(s.enabled, "enabled defaults to true");
        assert_eq!(s.transport, McpTransportKind::Stdio);
        assert_eq!(s.command, "npx");
        assert_eq!(s.args.len(), 3);

        // Substitute after parse.
        cfg.servers[0].substitute(Path::new("/tmp/work"));
        assert!(cfg.servers[0].args[2].contains("/tmp/work"));
    }

    #[test]
    fn missing_file_returns_empty() {
        let cfg = McpConfig::load_or_empty(Path::new("/no/such/file"), Path::new("/tmp"));
        assert!(cfg.servers.is_empty());
    }

    #[test]
    fn env_substitution() {
        let mut s = McpServerConfig {
            name: "x".into(),
            transport: McpTransportKind::Stdio,
            command: "echo".into(),
            args: vec![],
            env: HashMap::from([("ROOT".to_string(), "${workspace_root}/data".to_string())]),
            cwd: Some(PathBuf::from("${workspace_root}")),
            enabled: true,
        };
        s.substitute(Path::new("/w"));
        assert_eq!(s.env.get("ROOT").unwrap(), "/w/data");
        assert_eq!(s.cwd.as_ref().unwrap().to_str().unwrap(), "/w");
    }
}
