//! Integration test: the client handshakes a real MCP server over child-process
//! stdio, lists its tools, and calls one. The server is `mcp_echo`, a crate bin, so
//! the whole path runs in CI with no network or external server.

use std::collections::BTreeMap;

use ff_core::{McpScope, McpServerConfig};
use ff_mcp::McpClient;

fn echo_config() -> McpServerConfig {
    McpServerConfig {
        id: "echo".into(),
        // Absolute path to the test server bin — resolves even though the child env is
        // cleared (no PATH), which is the env-isolation behaviour we want.
        command: env!("CARGO_BIN_EXE_mcp_echo").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        disabled: false,
        scope: McpScope::Global,
        reaches_network: None,
        defer: None,
    }
}

#[tokio::test]
async fn handshake_lists_and_calls_a_tool_over_stdio() {
    let client = McpClient::connect(&echo_config(), &[], None, &[])
        .await
        .expect("connect + initialize");

    let tools = client.list_tools().await.expect("list_tools");
    let echo = tools
        .iter()
        .find(|t| t.name == "echo")
        .unwrap_or_else(|| panic!("echo tool present; got {tools:?}"));
    assert_eq!(echo.server, "echo");
    assert!(
        echo.input_schema.is_object(),
        "schema: {:?}",
        echo.input_schema
    );

    let out = client
        .call_tool("echo", serde_json::json!({ "message": "hello mcp" }))
        .await
        .expect("call_tool");
    assert!(out.contains("hello mcp"), "echoed content: {out:?}");

    client.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn bad_arguments_are_rejected_before_dispatch() {
    let client = McpClient::connect(&echo_config(), &[], None, &[])
        .await
        .expect("connect + initialize");
    let err = client
        .call_tool("echo", serde_json::json!("not an object"))
        .await
        .expect_err("non-object arguments must error");
    assert!(matches!(err, ff_mcp::McpError::BadArguments), "{err:?}");
    client.shutdown().await.expect("shutdown");
}

fn cwd_config() -> McpServerConfig {
    McpServerConfig {
        id: "cwd".into(),
        command: env!("CARGO_BIN_EXE_mcp_cwd").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        disabled: false,
        scope: McpScope::Global,
        reaches_network: None,
        defer: None,
    }
}

/// #548 W1b: a configured cwd is applied to the spawned child, so a workspace-aware
/// server runs in (and indexes) the requested directory rather than the launcher's.
#[tokio::test]
async fn connect_runs_the_child_in_the_configured_cwd() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Canonicalize: on macOS the tempdir lives under a /var -> /private/var symlink,
    // and the child reports its resolved cwd.
    let want = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");

    let client = McpClient::connect(&cwd_config(), &["PATH"], Some(&want), &[want.as_path()])
        .await
        .expect("connect + initialize");
    let out = client
        .call_tool("pwd", serde_json::Value::Null)
        .await
        .expect("call_tool pwd");
    let got = std::fs::canonicalize(out.trim()).expect("canonicalize reported cwd");
    assert_eq!(got, want, "child cwd should be the configured directory");
    client.shutdown().await.expect("shutdown");
}
