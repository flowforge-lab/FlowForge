//! Integration tests for the M4.3 tool bridge. Verifies that MCP tools surface in
//! the supervisor's tool snapshot, route calls through the actor, and disappear on stop.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use ff_core::{McpScope, McpServerConfig};
use ff_mcp::{
    build_bridged_tools, spawn_supervisor, InstanceKey, McpBridgedTool, SharedConfig,
    SupervisorConfig, SupervisorHandle,
};
use ff_tools::Safety;
use tokio::sync::mpsc;

fn fast_config() -> SupervisorConfig {
    SupervisorConfig {
        tick: Duration::from_millis(50),
        health_interval: Duration::from_millis(200),
        backoff_base: Duration::from_millis(50),
        backoff_max: Duration::from_millis(200),
        max_failures: 3,
        min_healthy_uptime: Duration::ZERO,
        env_allowlist: vec!["PATH".into()],
    }
}

fn echo_cfg() -> McpServerConfig {
    McpServerConfig {
        id: "echo".into(),
        command: env!("CARGO_BIN_EXE_mcp_echo").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        disabled: false,
        scope: McpScope::Global,
    }
}

fn cwd_cfg() -> McpServerConfig {
    McpServerConfig {
        id: "cwd".into(),
        command: env!("CARGO_BIN_EXE_mcp_cwd").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        // Workspace-scoped: one instance per session root (RFC 0018 §4.2).
        scope: McpScope::Workspace,
        disabled: false,
    }
}

async fn wait_running(handle: &SupervisorHandle, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let snap = handle.status.read().unwrap().clone();
        if snap
            .iter()
            .any(|s| s.state == ff_core::McpServerState::Running)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("server did not reach Running within {:?}", timeout);
}

#[tokio::test]
async fn tools_snapshot_contains_running_server_tools() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![echo_cfg()]));
    let (_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    wait_running(&sup, Duration::from_secs(5)).await;

    let tools = sup.tools_snapshot();
    assert!(!tools.is_empty(), "should have tools after Running");
    let echo_tool = tools.iter().find(|t| t.info.name == "echo");
    assert!(
        echo_tool.is_some(),
        "echo tool should be advertised; got: {:?}",
        tools.iter().map(|t| &t.info.name).collect::<Vec<_>>()
    );
    assert_eq!(echo_tool.unwrap().info.server, "echo");

    sup.stop_all().await;
}

#[tokio::test]
async fn call_tool_routes_through_supervisor() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![echo_cfg()]));
    let (_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    wait_running(&sup, Duration::from_secs(5)).await;

    let result = sup
        .call_tool(
            &InstanceKey::global("echo"),
            "echo",
            serde_json::json!({"message": "bridge-test"}),
        )
        .await;
    assert!(result.is_ok(), "call_tool failed: {:?}", result.err());
    assert!(
        result.unwrap().contains("bridge-test"),
        "echoed content expected"
    );

    sup.stop_all().await;
}

#[tokio::test]
async fn tools_snapshot_empty_after_stop() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![echo_cfg()]));
    let (_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    wait_running(&sup, Duration::from_secs(5)).await;
    assert!(!sup.tools_snapshot().is_empty());

    sup.stop_all().await;
    assert!(
        sup.tools_snapshot().is_empty(),
        "tools should be empty after stop_all"
    );
}

#[tokio::test]
async fn bridged_tool_name_and_safety() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![echo_cfg()]));
    let (_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    wait_running(&sup, Duration::from_secs(5)).await;

    let bridged = build_bridged_tools(&sup, std::path::Path::new("."));
    assert!(!bridged.is_empty());
    let tool = &bridged[0];
    assert_eq!(tool.name(), "mcp__echo__echo");
    assert_eq!(tool.safety(&serde_json::Value::Null), Safety::Write);
    assert!(tool.parameters().is_object());

    sup.stop_all().await;
}

#[tokio::test]
async fn bridged_tool_run_returns_result() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![echo_cfg()]));
    let (_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    wait_running(&sup, Duration::from_secs(5)).await;

    let bridged = build_bridged_tools(&sup, std::path::Path::new("."));
    let tool = bridged
        .iter()
        .find(|t| t.name() == "mcp__echo__echo")
        .expect("echo tool bridged");
    let outcome = tool
        .run(
            serde_json::json!({"message": "hello from tool"}),
            std::path::Path::new("."),
        )
        .await;
    assert!(outcome.success);
    assert!(outcome.content.contains("hello from tool"));

    sup.stop_all().await;
}

/// RFC 0018 §4.6 acceptance: under two concurrent workspace sessions, a bridged tool
/// built for one root routes its call to that root's instance. Both turns see the same
/// model-facing name (`mcp__cwd__pwd`) but bind to distinct instances.
#[tokio::test]
async fn bridge_routes_to_per_root_instance() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![cwd_cfg()]));
    let (_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let root_a = std::fs::canonicalize(dir_a.path()).unwrap();
    let root_b = std::fs::canonicalize(dir_b.path()).unwrap();
    sup.align_session("sa", root_a.clone()).await;
    sup.align_session("sb", root_b.clone()).await;

    // Wait for both per-root instances to be Running.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let running = sup
            .status_snapshot()
            .into_iter()
            .filter(|s| s.id == "cwd" && s.state == ff_core::McpServerState::Running)
            .count();
        if running == 2 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "two per-root instances should reach Running"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // A turn on root_a sees exactly the cwd tool bound to root_a's instance.
    let tools_a = build_bridged_tools(&sup, &root_a);
    let pwd_a = tools_a
        .iter()
        .find(|t| t.name() == "mcp__cwd__pwd")
        .expect("cwd tool bridged for root_a");
    let out_a = pwd_a
        .run(serde_json::Value::Null, std::path::Path::new("."))
        .await;
    assert!(out_a.success);
    assert_eq!(
        std::fs::canonicalize(out_a.content.trim()).unwrap(),
        root_a,
        "tool built for root_a must route to root_a's instance"
    );

    let tools_b = build_bridged_tools(&sup, &root_b);
    let pwd_b = tools_b
        .iter()
        .find(|t| t.name() == "mcp__cwd__pwd")
        .expect("cwd tool bridged for root_b");
    let out_b = pwd_b
        .run(serde_json::Value::Null, std::path::Path::new("."))
        .await;
    assert!(out_b.success);
    assert_eq!(
        std::fs::canonicalize(out_b.content.trim()).unwrap(),
        root_b,
        "tool built for root_b must route to root_b's instance"
    );

    sup.stop_all().await;
}

#[test]
fn namespaced_name_format() {
    assert_eq!(
        McpBridgedTool::namespaced_name("my-server", "do_thing"),
        "mcp__my-server__do_thing"
    );
    assert_eq!(
        McpBridgedTool::namespaced_name("fs", "read_file"),
        "mcp__fs__read_file"
    );
}
