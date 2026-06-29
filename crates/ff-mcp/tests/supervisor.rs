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
use ff_mcp::{spawn_supervisor, SharedConfig, SupervisorConfig, SupervisorHandle};
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

fn exit_cfg() -> McpServerConfig {
    McpServerConfig {
        id: "exit".into(),
        command: env!("CARGO_BIN_EXE_mcp_exit").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        disabled: false,
        scope: McpScope::Global,
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

    wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| s.id == "slow" && s.state == McpServerState::Running)
            .map(|_| ())
    })
    .await
    .expect("slow server reaches Running");

    // Fire a tool call that would block the actor for 60s without preemption.
    let caller = sup.clone();
    let call = tokio::spawn(async move {
        caller
            .call_tool("slow", "sleep", serde_json::json!({ "ms": 60_000 }))
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
}

fn cwd_cfg() -> McpServerConfig {
    McpServerConfig {
        id: "cwd".into(),
        command: env!("CARGO_BIN_EXE_mcp_cwd").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        disabled: false,
        scope: McpScope::Global,
    }
}

/// #548 W1b: `set_server_cwd` restarts the server in the requested directory, so the
/// workspace-aware server tracks the active workspace. The `pwd` tool reports the
/// child's cwd; after the override it must equal the new directory.
#[tokio::test]
async fn set_server_cwd_restarts_child_in_new_directory() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![cwd_cfg()]));
    let (_change_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(shared, change_rx, fast_config());

    // A manual restart re-spawns a fresh handle, so detect the change via the pid
    // (the restarts counter resets on a manual restart -- see manual_restart test).
    let first_pid = wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| s.id == "cwd" && s.state == McpServerState::Running)
            .and_then(|s| s.pid)
    })
    .await
    .expect("cwd server reaches Running");

    let dir = tempfile::tempdir().expect("tempdir");
    let want = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");
    sup.set_server_cwd("cwd", Some(want.clone())).await;

    // It restarts in the new dir: wait for a fresh child (different pid).
    let second_pid = wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| {
                s.id == "cwd"
                    && s.state == McpServerState::Running
                    && s.pid.is_some()
                    && s.pid != Some(first_pid)
            })
            .and_then(|s| s.pid)
    })
    .await
    .expect("cwd server restarts after set_server_cwd");
    assert_ne!(
        first_pid, second_pid,
        "set_server_cwd should respawn the child"
    );

    let out = sup
        .call_tool("cwd", "pwd", serde_json::Value::Null)
        .await
        .expect("pwd call");
    let got = std::fs::canonicalize(out.trim()).expect("canonicalize reported cwd");
    assert_eq!(
        got, want,
        "child should run in the directory set via set_server_cwd"
    );

    // Setting the same dir again is a no-op: no respawn, so the pid is unchanged.
    sup.set_server_cwd("cwd", Some(want.clone())).await;
    let pid_after = sup
        .status
        .read()
        .unwrap()
        .iter()
        .find(|s| s.id == "cwd")
        .and_then(|s| s.pid);
    assert_eq!(
        pid_after,
        Some(second_pid),
        "an unchanged cwd must not respawn the child"
    );

    sup.stop_all().await;
}

/// #548 W1b follow-up: removing a server via reconcile prunes its cwd override, so a
/// later re-add starts in the default working directory rather than inheriting the
/// stale workspace.
#[tokio::test]
async fn reconcile_removal_prunes_cwd_override() {
    let shared: SharedConfig = Arc::new(RwLock::new(vec![cwd_cfg()]));
    let (change_tx, change_rx) = mpsc::unbounded_channel::<()>();
    let sup = spawn_supervisor(Arc::clone(&shared), change_rx, fast_config());

    let first_pid = wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| s.id == "cwd" && s.state == McpServerState::Running)
            .and_then(|s| s.pid)
    })
    .await
    .expect("cwd server reaches Running");

    // Point the server at a tempdir and wait for the override-driven restart.
    let dir = tempfile::tempdir().expect("tempdir");
    let want = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");
    sup.set_server_cwd("cwd", Some(want.clone())).await;
    wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| {
                s.id == "cwd"
                    && s.state == McpServerState::Running
                    && s.pid.is_some()
                    && s.pid != Some(first_pid)
            })
            .map(|_| ())
    })
    .await
    .expect("cwd server restarts in the override dir");

    // Remove the server: reconcile issues Stop, which must drop the override too.
    shared.write().unwrap().clear();
    change_tx.send(()).expect("signal config change");
    wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter().all(|s| s.id != "cwd").then_some(())
    })
    .await
    .expect("cwd server is removed");

    // Re-add it: with the override pruned, the child must start in the default cwd,
    // not the stale tempdir.
    *shared.write().unwrap() = vec![cwd_cfg()];
    change_tx.send(()).expect("signal config change");
    wait_for(&sup, Duration::from_secs(5), |snap| {
        snap.iter()
            .find(|s| s.id == "cwd" && s.state == McpServerState::Running)
            .map(|_| ())
    })
    .await
    .expect("cwd server is re-added and Running");

    let out = sup
        .call_tool("cwd", "pwd", serde_json::Value::Null)
        .await
        .expect("pwd call");
    let got = std::fs::canonicalize(out.trim()).expect("canonicalize reported cwd");
    let default_cwd = std::fs::canonicalize(std::env::current_dir().expect("current_dir"))
        .expect("canonicalize cwd");
    assert_eq!(
        got, default_cwd,
        "a re-added server must not inherit the pruned cwd override"
    );
    assert_ne!(got, want, "stale override must not apply after re-add");

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
