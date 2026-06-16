//! Integration test: the client handshakes a real MCP server over child-process
//! stdio, lists its tools, and calls one. The server is `mcp_echo`, a crate bin, so
//! the whole path runs in CI with no network or external server.

use std::collections::BTreeMap;

use ff_core::McpServerConfig;
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
    }
}

#[tokio::test]
async fn handshake_lists_and_calls_a_tool_over_stdio() {
    let client = McpClient::connect(&echo_config(), &[])
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
    let client = McpClient::connect(&echo_config(), &[])
        .await
        .expect("connect + initialize");
    let err = client
        .call_tool("echo", serde_json::json!("not an object"))
        .await
        .expect_err("non-object arguments must error");
    assert!(matches!(err, ff_mcp::McpError::BadArguments), "{err:?}");
    client.shutdown().await.expect("shutdown");
}
