//! Integration tests for the MCP client using a mock MCP server built as a
//! cargo example. We spawn `cargo run -q --example mock_mcp_server` as the
//! child so the tests exercise the *actual* stdio transport, not an
//! in-process fake.

use forge_mcp::{
    config::{McpConfig, McpServerConfig, McpTransportKind},
    registry::McpRegistry,
};
use std::collections::HashMap;

fn mock_server_config(name: &str) -> McpServerConfig {
    McpServerConfig {
        name: name.into(),
        transport: McpTransportKind::Stdio,
        command: "cargo".into(),
        args: vec![
            "run".into(), "-q".into(),
            "--example".into(), "mock_mcp_server".into(),
            "-p".into(), "forge-mcp".into(),
        ],
        env: HashMap::new(),
        cwd: None,
        enabled: true,
    }
}

#[tokio::test]
async fn boot_lists_and_calls_mock_tool() {
    let cfg = McpConfig { servers: vec![mock_server_config("mock")] };
    let boot = McpRegistry::start(&cfg).await;

    // One server, one tool.
    assert_eq!(boot.tools.len(), 1, "expected one tool from mock server");
    let statuses = &boot.statuses;
    assert_eq!(statuses.len(), 1);
    match &statuses[0] {
        forge_mcp::McpServerStatus::Started { name, tools } => {
            assert_eq!(name, "mock");
            assert_eq!(tools, &["mcp.mock.echo".to_string()]);
        }
        other => panic!("expected Started, got {other:?}"),
    }

    // Invoke it via the client directly.
    let client = boot.registry.get("mock").expect("mock client present");
    let result = client
        .call_tool("echo", serde_json::json!({ "text": "hello world" }))
        .await
        .expect("echo call succeeds");
    assert!(!result.is_error);
    // The result is a list of text parts — we should see "hello world".
    let joined: String = result.content.iter().filter_map(|p| match p {
        forge_mcp::protocol::ContentPart::Text { text } => Some(text.clone()),
        _ => None,
    }).collect::<Vec<_>>().join("");
    assert_eq!(joined, "hello world");
}

#[tokio::test]
async fn boot_reports_failed_command_without_panicking() {
    let cfg = McpConfig {
        servers: vec![McpServerConfig {
            name: "does-not-exist".into(),
            transport: McpTransportKind::Stdio,
            command: "this_binary_should_not_exist_forge_mcp_probe".into(),
            args: vec![],
            env: HashMap::new(),
            cwd: None,
            enabled: true,
        }],
    };
    let boot = McpRegistry::start(&cfg).await;
    assert!(boot.tools.is_empty());
    assert!(matches!(&boot.statuses[0], forge_mcp::McpServerStatus::Failed { .. }));
}

#[tokio::test]
async fn disabled_server_is_skipped_entirely() {
    let cfg = McpConfig {
        servers: vec![McpServerConfig {
            name: "off".into(),
            transport: McpTransportKind::Stdio,
            command: "cargo".into(),
            args: vec![],
            env: HashMap::new(),
            cwd: None,
            enabled: false,
        }],
    };
    let boot = McpRegistry::start(&cfg).await;
    assert!(boot.tools.is_empty());
    assert!(matches!(&boot.statuses[0], forge_mcp::McpServerStatus::Disabled { .. }));
}
