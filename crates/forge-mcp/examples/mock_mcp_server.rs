//! Minimal MCP echo server used as an integration-test fixture.
//!
//! Speaks MCP 2024-11-05 over stdio:
//!   * `initialize` → returns `{ protocolVersion, serverInfo, capabilities: { tools: {} } }`
//!   * `notifications/initialized` → ignored
//!   * `tools/list` → returns one tool named `echo`
//!   * `tools/call` with `name=echo` → returns the `arguments.text` field back
//!
//! No dependencies beyond `serde_json` (already in the workspace) so tests
//! can spawn `cargo run -q --example mock_mcp_server -p forge-mcp` and get a
//! real subprocess connection.

use std::io::{self, BufRead, Write};

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() { continue; }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = v.get("id").cloned();
        let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");

        // Notifications carry no id — ignore.
        if id.is_none() {
            eprintln!("mock: notification {method}");
            continue;
        }

        let response = match method {
            "initialize" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "serverInfo": { "name": "mock-mcp", "version": "0.1.0" },
                    "capabilities": { "tools": {} }
                }
            }),
            "tools/list" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [{
                        "name": "echo",
                        "description": "Echo the text back",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "text": { "type": "string" } },
                            "required": ["text"]
                        }
                    }]
                }
            }),
            "tools/call" => {
                let params = v.get("params").cloned().unwrap_or(serde_json::json!({}));
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if name == "echo" {
                    let text = params
                        .get("arguments")
                        .and_then(|a| a.get("text"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": text }]
                        }
                    })
                } else {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": format!("unknown tool: {name}") }
                    })
                }
            }
            _ => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {method}") }
            }),
        };

        let mut bytes = serde_json::to_vec(&response).unwrap();
        bytes.push(b'\n');
        if out.write_all(&bytes).is_err() { break; }
        if out.flush().is_err() { break; }
    }
}
