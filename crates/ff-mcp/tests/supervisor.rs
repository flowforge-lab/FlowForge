//! Integration tests for the M4.2 supervisor. Each test exercises one of the issue
//! #89 acceptance criteria with real child processes.
//!
//! Tests use compressed timings (tick = 50ms, base = 50ms, max = 200ms,
//! max_failures = 3) so the whole file finishes in under a couple of seconds.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use ff_core::{McpServerConfig, McpServerState};
use ff_mcp::{spawn_supervisor, SharedConfig, SupervisorConfig, SupervisorHandle};
use tokio::sync::mpsc;

fn fast_config() -> SupervisorConfig {
    SupervisorConfig {
        tick: Duration::from_millis(50),
        health_interval: Duration::from_millis(200),
        backoff_base: Duration::from_millis(50),
        backoff_max: Duration::from_millis(200),
        max_failures: 3,
        // PATH only — sufficient because tests use absolute `CARGO_BIN_EXE_*` paths.
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
    }
}

fn exit_cfg() -> McpServerConfig {
    McpServerConfig {
        id: "exit".into(),
        command: env!("CARGO_BIN_EXE_mcp_exit").to_string(),
        args: vec![],
        env: BTreeMap::new(),
        disabled: false,
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
