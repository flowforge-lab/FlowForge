//! Integration tests for the M4.2 supervisor. Each test exercises one of the issue
//! #89 acceptance criteria with real child processes.
//!
//! Tests use compressed timings (tick = 50ms, base = 50ms, max = 200ms,
//! max_failures = 3) so the whole file finishes in under a couple of seconds.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use ff_core::{McpScope, McpServerConfig, McpServerState};
use ff_mcp::{spawn_supervisor, InstanceKey, SharedConfig, SupervisorConfig, SupervisorHandle};
use tokio::sync::mpsc;

fn fast_config() -> SupervisorConfig {
    SupervisorConfig {
        tick: Duration::from_millis(50),
        health_interval: Duration::from_millis(200),
        backoff_base: Duration::from_millis(50),
        backoff_max: Duration::from_millis(200),
        max_failures: 3,
        // ZERO so a transport close right after Running is treated as a recoverable
        // clean exit; per-test overrides set a real threshold to exercise flapping.
        min_healthy_uptime: Duration::ZERO,
        // PATH only — sufficient because tests use absolute `CARGO_BIN_EXE_*` paths.
        env_allowlist: vec!["PATH".into()],
    }
}

fn idle_exit_cfg() -> McpServerConfig {
    McpServerConfig {
        id: "idle".into(),
        command: env!("CARGO_BIN_EXE_mcp_idle_exit").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        disabled: false,
        scope: McpScope::Global,
        reaches_network: None,
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
        reaches_network: None,
    }
}

fn exit_cfg() -> McpServerConfig {
    McpServerConfig {
        id: "exit".into(),
        command: env!("CARGO_BIN_EXE_mcp_exit").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        disabled: false,
        scope: McpScope::Global,
        reaches_network: None,
    }
}

fn slow_cfg() -> McpServerConfig {
    McpServerConfig {
        id: "slow".into(),
        command: env!("CARGO_BIN_EXE_mcp_slow").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        disabled: false,
        scope: McpScope::Global,
        reaches_network: None,
    }
}

/// Poll up to `timeout` for `pred(snapshot)` to return `Some(value)`.
async fn wait_for<F, T>(handle: &SupervisorHandle, timeout: Duration, mut pred: F) -> Option<T>
where
    F: FnMut(&[ff_core::McpServerStatus]) -> Option<T>,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let snap = handle.status.read().unwrap().clone();
        if let Some(v) = pred(&snap) {
            return Some(v);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

/// Liveness probe via `kill(pid, 0)`. Returns `false` (ESRCH) once the process is
/// reaped.
fn pid_alive(pid: u32) -> bool {
    // SAFETY: `kill` with signal 0 only checks for the existence of a process and
    // never delivers a signal.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[tokio::test]
async fn crash_auto_restarts_with_backoff() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![echo_cfg()]));
    let (_change_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    // Wait for first Running and capture the pid.
    let pid = wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| s.id == "echo" && s.state == McpServerState::Running)
            .and_then(|s| s.pid)
    })
    .await
    .expect("server reaches Running");

    // Kill the child out from under the supervisor — simulates a crash.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }

    // Health probe (every 200ms) detects the dead connection, supervisor restarts,
    // and we land back in Running with `restarts >= 1`.
    let restarts = wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| s.id == "echo" && s.state == McpServerState::Running && s.restarts >= 1)
            .map(|s| s.restarts)
    })
    .await
    .expect("server auto-restarts after crash");
    assert!(restarts >= 1, "restarts should increment, got {restarts}");

    sup.stop_all().await;
}

#[tokio::test]
async fn parks_in_failed_after_n_failures() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![exit_cfg()]));
    let (_change_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    let last_error = wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| s.id == "exit" && s.state == McpServerState::Failed)
            .and_then(|s| s.last_error.clone())
    })
    .await
    .expect("server parks in Failed after max_failures");
    assert!(
        !last_error.is_empty(),
        "last_error should describe the connect failure"
    );

    sup.stop_all().await;
}

#[tokio::test]
async fn no_orphan_processes_after_stop_all() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![echo_cfg()]));
    let (_change_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    let pid = wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| s.id == "echo" && s.state == McpServerState::Running)
            .and_then(|s| s.pid)
    })
    .await
    .expect("server reaches Running");
    assert!(pid_alive(pid), "child should be alive before stop_all");

    sup.stop_all().await;

    // The child may take a beat to exit after stdin EOF; give it a small window.
    let deadline = Instant::now() + Duration::from_secs(2);
    while pid_alive(pid) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !pid_alive(pid),
        "child pid {pid} still alive after stop_all"
    );
}

#[tokio::test]
async fn manual_restart_reconnects_running_server() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![echo_cfg()]));
    let (_change_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    let first_pid = wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| s.id == "echo" && s.state == McpServerState::Running)
            .and_then(|s| s.pid)
    })
    .await
    .expect("server reaches Running");

    // Manual restart spawns a fresh child, so the pid must change.
    sup.restart("echo").await;

    let second_pid = wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| {
                s.id == "echo"
                    && s.state == McpServerState::Running
                    && s.pid.is_some()
                    && s.pid != Some(first_pid)
            })
            .and_then(|s| s.pid)
    })
    .await
    .expect("server reconnects after manual restart");
    assert_ne!(first_pid, second_pid, "restart should spawn a new child");

    sup.stop_all().await;
}

#[tokio::test]
async fn manual_restart_unknown_id_is_noop() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![echo_cfg()]));
    let (_change_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| s.id == "echo" && s.state == McpServerState::Running)
            .map(|_| ())
    })
    .await
    .expect("server reaches Running");

    // Restarting a server that was never configured must not panic or disturb others.
    sup.restart("does-not-exist").await;

    let still_running = wait_for(&sup, Duration::from_secs(2), |snap| {
        snap.iter()
            .find(|s| s.id == "echo" && s.state == McpServerState::Running)
            .map(|_| ())
    })
    .await;
    assert!(
        still_running.is_some(),
        "unknown-id restart left echo running"
    );

    sup.stop_all().await;
}

/// Quitting mid-call must not stall up to `CALL_TIMEOUT` (#119). `stop_all` flips a
/// latch that `do_call_tool` races, so an in-flight 60s tool call is abandoned and the
/// stop completes within the graceful-close budget rather than waiting for the call.
#[tokio::test]
async fn stop_all_preempts_in_flight_tool_call() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![slow_cfg()]));
    let (_change_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    let pid = wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| s.id == "slow" && s.state == McpServerState::Running)
            .and_then(|s| s.pid)
    })
    .await
    .expect("slow server reaches Running");

    // Fire a tool call that would block the actor for 60s without preemption.
    let caller = sup.clone();
    let call = tokio::spawn(async move {
        caller
            .call_tool(
                &InstanceKey::global("slow"),
                "sleep",
                serde_json::json!({ "ms": 60_000 }),
            )
            .await
    });

    // Give the call time to reach the actor and start awaiting on the client.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let started = Instant::now();
    sup.stop_all().await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "stop_all should preempt the in-flight call, not wait for it; took {elapsed:?}"
    );

    // The abandoned call resolves with an error rather than hanging.
    let result = tokio::time::timeout(Duration::from_secs(2), call)
        .await
        .expect("call task should resolve, not hang")
        .expect("call task should not panic");
    assert!(
        result.is_err(),
        "preempted call should return an error, got {result:?}"
    );

    // `stop_all` drops the client and relies on kill-on-drop to reap the child;
    // that reap is async, so wait for the slow server to actually exit before
    // returning. Without this the `mcp_slow` child (mid-60s `sleep`) outlives
    // the test process and nextest's process-per-test leak detection flags it
    // (#1072). Mirrors `no_orphan_processes_after_stop_all`.
    let deadline = Instant::now() + Duration::from_secs(2);
    while pid_alive(pid) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !pid_alive(pid),
        "slow server pid {pid} still alive after stop_all"
    );
}

fn cwd_cfg() -> McpServerConfig {
    McpServerConfig {
        id: "cwd".into(),
        command: env!("CARGO_BIN_EXE_mcp_cwd").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        disabled: false,
        // Workspace-scoped: one instance per session root (RFC 0018 §4.2).
        scope: McpScope::Workspace,
        reaches_network: None,
    }
}

/// RFC 0018 §4.4/§4.5: `align_session` starts a workspace-scoped server as a per-root
/// instance, and the child runs in that root (the `pwd` tool reports its cwd). This
/// replaces the retired `set_server_cwd` hack -- the root now rides the instance key
/// (and is advertised as an MCP root). The status snapshot tags the instance with its
/// root so the UI can disambiguate two instances of the same id.
#[tokio::test]
async fn align_session_runs_workspace_instance_in_its_root() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![cwd_cfg()]));
    let (_change_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    let dir = tempfile::tempdir().expect("tempdir");
    let want = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");
    sup.align_session("s1", want.clone(), vec![cwd_cfg()]).await;

    wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| s.id == "cwd" && s.state == McpServerState::Running)
            .map(|_| ())
    })
    .await
    .expect("workspace instance reaches Running");

    let out = sup
        .call_tool(
            &InstanceKey::workspace("cwd", &want),
            "pwd",
            serde_json::Value::Null,
        )
        .await
        .expect("pwd call");
    let got = std::fs::canonicalize(out.trim()).expect("canonicalize reported cwd");
    assert_eq!(
        got, want,
        "child should run in the session's workspace root"
    );

    let scope_key = sup
        .status_snapshot()
        .into_iter()
        .find(|s| s.id == "cwd")
        .and_then(|s| s.scope_key);
    assert_eq!(scope_key, Some(want.display().to_string()));

    sup.stop_all().await;
}

/// RFC 0018 §4.2 / #557: two sessions on distinct workspace roots get two separate
/// instances of the same server id, and a call routes to the instance for its root.
#[tokio::test]
async fn distinct_roots_get_distinct_instances() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![cwd_cfg()]));
    let (_change_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let root_a = std::fs::canonicalize(dir_a.path()).unwrap();
    let root_b = std::fs::canonicalize(dir_b.path()).unwrap();
    sup.align_session("sa", root_a.clone(), vec![cwd_cfg()])
        .await;
    sup.align_session("sb", root_b.clone(), vec![cwd_cfg()])
        .await;

    wait_for(&sup, Duration::from_secs(5), |snap| {
        let running = snap
            .iter()
            .filter(|s| s.id == "cwd" && s.state == McpServerState::Running)
            .count();
        (running == 2).then_some(())
    })
    .await
    .expect("two distinct workspace instances reach Running");

    let out_a = sup
        .call_tool(
            &InstanceKey::workspace("cwd", &root_a),
            "pwd",
            serde_json::Value::Null,
        )
        .await
        .expect("pwd a");
    let out_b = sup
        .call_tool(
            &InstanceKey::workspace("cwd", &root_b),
            "pwd",
            serde_json::Value::Null,
        )
        .await
        .expect("pwd b");
    assert_eq!(std::fs::canonicalize(out_a.trim()).unwrap(), root_a);
    assert_eq!(std::fs::canonicalize(out_b.trim()).unwrap(), root_b);

    sup.stop_all().await;
}

/// RFC 0018 §4.3: a workspace instance is ref-counted across sessions and evicted only
/// when the last referencing session is released.
#[tokio::test]
async fn workspace_instance_evicted_on_last_session_release() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![cwd_cfg()]));
    let (_change_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // Two sessions share the same root -> one instance, ref-count 2.
    sup.align_session("s1", root.clone(), vec![cwd_cfg()]).await;
    sup.align_session("s2", root.clone(), vec![cwd_cfg()]).await;

    wait_for(&sup, Duration::from_secs(5), |snap| {
        (snap.iter().filter(|s| s.id == "cwd").count() == 1).then_some(())
    })
    .await
    .expect("shared root yields exactly one instance");

    // Releasing one session keeps the instance alive (still referenced by the other).
    sup.release_session("s1").await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        sup.status_snapshot().iter().any(|s| s.id == "cwd"),
        "instance must survive while another session references it"
    );

    // Releasing the last session evicts it.
    sup.release_session("s2").await;
    wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter().all(|s| s.id != "cwd").then_some(())
    })
    .await
    .expect("instance evicted once its last session is released");

    sup.stop_all().await;
}

/// #548 W1: a stdio server that idle-exits cleanly after a healthy run must be
/// restarted and its tools re-bridged, and it must never be parked in `Failed`
/// (the regression that left codegraph un-bridged for a whole session). The fixture
/// exits 0 every ~300ms; with `min_healthy_uptime = 0` each exit is a recoverable
/// clean exit, so the supervisor keeps reviving it and `restarts` climbs.
#[tokio::test]
async fn clean_idle_exit_recovers_without_parking() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![idle_exit_cfg()]));
    let (_change_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    // First Running with its tool bridged.
    wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| s.id == "idle" && s.state == McpServerState::Running && s.tool_count >= 1)
            .map(|_| ())
    })
    .await
    .expect("idle server reaches Running with its tool");

    // It exits and is revived repeatedly: restarts climbs past one clean cycle.
    let restarts = wait_for(&sup, Duration::from_secs(8), |snap| {
        snap.iter()
            .find(|s| s.id == "idle" && s.state == McpServerState::Running && s.restarts >= 2)
            .map(|s| s.restarts)
    })
    .await
    .expect("idle server auto-recovers across multiple clean exits");
    assert!(restarts >= 2, "expected repeated recovery, got {restarts}");

    // It must never have been parked in Failed.
    let parked = sup
        .status
        .read()
        .unwrap()
        .iter()
        .any(|s| s.id == "idle" && s.state == McpServerState::Failed);
    assert!(
        !parked,
        "a cleanly idle-exiting server must not be parked in Failed"
    );

    sup.stop_all().await;
}

/// #548 W1 hot-loop guard: a server that exits *before* `min_healthy_uptime` is
/// flapping, not idle-exiting, so each exit counts as a failure and it still parks
/// in `Failed` after `max_failures`. The fixture exits at ~300ms; a 2s threshold
/// makes every exit count.
#[tokio::test]
async fn fast_flapping_server_still_parks_in_failed() {
    let cfg = SupervisorConfig {
        min_healthy_uptime: Duration::from_secs(2),
        ..fast_config()
    };
    let shared: SharedConfig = Arc::new(RwLock::new(vec![idle_exit_cfg()]));
    let (_change_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, cfg);

    let last_error = wait_for(&sup, Duration::from_secs(10), |snap| {
        snap.iter()
            .find(|s| s.id == "idle" && s.state == McpServerState::Failed)
            .and_then(|s| s.last_error.clone())
    })
    .await
    .expect("a server that exits faster than min_healthy_uptime parks in Failed");
    assert!(
        !last_error.is_empty(),
        "last_error should describe the early exit"
    );

    sup.stop_all().await;
}

/// A disabled Workspace-scoped server (e.g. the seeded codegraph) never produces a
/// `ServerHandle` -- reconcile skips non-Global, and `set_session_root` skips
/// disabled. Without a synthetic row Settings -> MCP could neither show nor enable
/// it (review #595). The supervisor surfaces it as a `Disabled` row keyed by id with
/// no scope (no live instance to disambiguate).
#[tokio::test]
async fn disabled_workspace_server_surfaces_as_synthetic_row() {
    let mut cfg = cwd_cfg();
    cfg.disabled = true;
    let shared: SharedConfig = Arc::new(RwLock::new(vec![cfg]));
    let (_change_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    let row = wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter().find(|s| s.id == "cwd").cloned()
    })
    .await
    .expect("disabled workspace server appears as a synthetic status row");

    assert_eq!(row.state, McpServerState::Disabled);
    assert_eq!(row.scope_key, None, "no live instance -> no scope key");
    assert_eq!(row.pid, None);

    sup.stop_all().await;
}
