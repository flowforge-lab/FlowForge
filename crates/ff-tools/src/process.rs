//! Background process management.
//!
//! `bash` and `python` run to completion under a hard timeout, so the agent
//! cannot start a dev server, file watcher, or long build and check back on it
//! later. `process_manager` fills that gap: it starts a process in the
//! background, returns a handle, and lets later turns `poll` its captured output
//! or `stop` it.
//!
//! State that must survive across turns cannot live in the tool -- the registry
//! is rebuilt every turn and `Tool::run` gets no session id. The live process
//! table therefore lives in [`ProcessSupervisor`], owned by the host's
//! `AppState` and injected at registry-build time (the same pattern as the
//! memory/skills tools). v1 is app-global; per-session auto-reap needs a session
//! id threaded into the `Tool` trait and is a tracked follow-up.
//!
//! Honesty notes:
//! - Like [`crate::bash`] and [`crate::python`], a started command is **not**
//!   sandboxed and is classified [`Safety::Write`] so the host's approval gate
//!   covers it (`poll`/`list` are read-only).
//! - On Unix each process is spawned in its **own process group** so `stop` can
//!   signal the whole tree (a dev server and its children), SIGTERM then SIGKILL
//!   after a grace window. Non-Unix platforms can start/poll/list but not stop.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::registry::{Safety, Tool, ToolOutcome};

/// Most concurrent *running* processes the supervisor will hold. A `start` past
/// this is rejected so a runaway agent cannot leak unbounded children.
const MAX_CONCURRENT: usize = 16;
/// Per-stream capture cap. A chatty server would otherwise grow the buffer without
/// bound; the oldest bytes are dropped and the truncation is reported on `poll`.
const MAX_BUFFER_BYTES: usize = 64 * 1024;
/// How long `stop` waits after SIGTERM before escalating to SIGKILL.
const STOP_GRACE: Duration = Duration::from_millis(2000);

#[derive(Clone, Debug)]
enum Status {
    Running,
    Exited(i32),
    Killed,
    Failed(String),
}

impl Status {
    fn label(&self) -> String {
        match self {
            Status::Running => "running".to_string(),
            Status::Exited(c) => format!("exited({c})"),
            Status::Killed => "killed".to_string(),
            Status::Failed(e) => format!("failed: {e}"),
        }
    }
    fn is_running(&self) -> bool {
        matches!(self, Status::Running)
    }
}

/// A byte ring with a hard cap; once full, appending drops the oldest bytes and
/// counts them so `snapshot` can flag the loss.
struct RingBuffer {
    buf: VecDeque<u8>,
    cap: usize,
    dropped: usize,
}

impl RingBuffer {
    fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::new(),
            cap,
            dropped: 0,
        }
    }
    fn extend(&mut self, bytes: &[u8]) {
        self.buf.extend(bytes.iter().copied());
        while self.buf.len() > self.cap {
            self.buf.pop_front();
            self.dropped += 1;
        }
    }
    fn snapshot(&self) -> String {
        let bytes: Vec<u8> = self.buf.iter().copied().collect();
        let text = String::from_utf8_lossy(&bytes);
        if self.dropped > 0 {
            format!("[... {} earlier bytes truncated ...]\n{text}", self.dropped)
        } else {
            text.into_owned()
        }
    }
}

/// Output buffers and live status shared between a process's detached reader/exit
/// tasks and the supervisor map entry.
struct Shared {
    stdout: Mutex<RingBuffer>,
    stderr: Mutex<RingBuffer>,
    status: Mutex<Status>,
}

impl Shared {
    fn new(cap: usize) -> Self {
        Self {
            stdout: Mutex::new(RingBuffer::new(cap)),
            stderr: Mutex::new(RingBuffer::new(cap)),
            status: Mutex::new(Status::Running),
        }
    }
    fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }
}

struct ManagedProcess {
    command: String,
    started_at: DateTime<Utc>,
    pid: Option<u32>,
    shared: Arc<Shared>,
}

/// App-global table of background processes. Cloneable handle is the `Arc` the
/// host holds; methods take `&self`.
pub struct ProcessSupervisor {
    procs: Mutex<HashMap<u64, ManagedProcess>>,
    next_id: AtomicU64,
}

impl Default for ProcessSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSupervisor {
    pub fn new() -> Self {
        Self {
            procs: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Spawn `command` (via the user's `$SHELL -c`) in `dir`, capturing stdout and
    /// stderr into bounded buffers. Returns the new process id. Rejected if the
    /// live-process cap is reached. Must be called from within a Tokio runtime
    /// (the agent loop always is): it spawns detached reader and exit-watcher tasks.
    pub fn start(&self, command: &str, dir: &Path) -> Result<u64, String> {
        {
            let map = self.procs.lock().unwrap();
            let live = map
                .values()
                .filter(|p| p.shared.status().is_running())
                .count();
            if live >= MAX_CONCURRENT {
                return Err(format!(
                    "too many concurrent processes (max {MAX_CONCURRENT}); stop one first"
                ));
            }
        }

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let mut cmd = Command::new(&shell);
        cmd.arg("-c")
            .arg(command)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);
        // Own process group so `stop` can signal the whole tree, not just the shell.
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn process ({shell} -c): {e}"))?;
        let pid = child.id();
        let shared = Arc::new(Shared::new(MAX_BUFFER_BYTES));

        if let Some(out) = child.stdout.take() {
            tokio::spawn(drain(out, shared.clone(), false));
        }
        if let Some(err) = child.stderr.take() {
            tokio::spawn(drain(err, shared.clone(), true));
        }
        let watch = shared.clone();
        tokio::spawn(async move {
            let status = match child.wait().await {
                Ok(es) => es.code().map(Status::Exited).unwrap_or(Status::Killed),
                Err(e) => Status::Failed(e.to_string()),
            };
            *watch.status.lock().unwrap() = status;
        });

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.procs.lock().unwrap().insert(
            id,
            ManagedProcess {
                command: command.to_string(),
                started_at: Utc::now(),
                pid,
                shared,
            },
        );
        Ok(id)
    }

    /// Captured stdout/stderr and current status for one process.
    pub fn poll(&self, id: u64) -> Result<String, String> {
        let map = self.procs.lock().unwrap();
        let p = map
            .get(&id)
            .ok_or_else(|| format!("no such process: {id}"))?;
        let status = p.shared.status();
        let out = p.shared.stdout.lock().unwrap().snapshot();
        let err = p.shared.stderr.lock().unwrap().snapshot();
        Ok(format!(
            "process {id} [{}]\ncommand: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            status.label(),
            p.command,
            out.trim_end(),
            err.trim_end()
        ))
    }

    /// One line per known process, oldest id first.
    pub fn list(&self) -> String {
        let map = self.procs.lock().unwrap();
        if map.is_empty() {
            return "No processes.".to_string();
        }
        let mut ids: Vec<u64> = map.keys().copied().collect();
        ids.sort_unstable();
        let mut s = String::new();
        for id in ids {
            let p = &map[&id];
            s.push_str(&format!(
                "#{id} [{}] {} (started {})\n",
                p.shared.status().label(),
                p.command,
                p.started_at.to_rfc3339()
            ));
        }
        s.trim_end().to_string()
    }

    /// SIGTERM the process group, then SIGKILL after [`STOP_GRACE`] if it is still
    /// running. A no-op for an already-finished process.
    pub async fn stop(&self, id: u64) -> Result<String, String> {
        let (pid, shared) = {
            let map = self.procs.lock().unwrap();
            let p = map
                .get(&id)
                .ok_or_else(|| format!("no such process: {id}"))?;
            (p.pid, p.shared.clone())
        };
        if !shared.status().is_running() {
            return Ok(format!("process {id} already {}", shared.status().label()));
        }
        let Some(pid) = pid else {
            return Err(format!("process {id} has no pid; cannot stop"));
        };

        #[cfg(unix)]
        {
            signal_group(pid, libc::SIGTERM);
            wait_until_exited(&shared, STOP_GRACE).await;
            if shared.status().is_running() {
                signal_group(pid, libc::SIGKILL);
                wait_until_exited(&shared, Duration::from_secs(2)).await;
            }
            Ok(format!(
                "process {id} stopped ({})",
                shared.status().label()
            ))
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            Err("stopping processes is not supported on this platform".to_string())
        }
    }
}

impl Drop for ProcessSupervisor {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Ok(map) = self.procs.lock() {
            for p in map.values() {
                if let (Some(pid), true) = (p.pid, p.shared.status().is_running()) {
                    signal_group(pid, libc::SIGKILL);
                }
            }
        }
    }
}

/// Drain a child pipe into the shared ring until EOF.
async fn drain<R: AsyncRead + Unpin>(mut reader: R, shared: Arc<Shared>, is_err: bool) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let ring = if is_err {
                    &shared.stderr
                } else {
                    &shared.stdout
                };
                ring.lock().unwrap().extend(&buf[..n]);
            }
        }
    }
}

/// Poll the shared status until it leaves `Running` or `budget` elapses.
#[cfg(unix)]
async fn wait_until_exited(shared: &Shared, budget: Duration) {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if !shared.status().is_running() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Signal the process group led by `pid` (negative pid targets the group). The
/// process was spawned with `process_group(0)`, so its pid is the group id.
#[cfg(unix)]
fn signal_group(pid: u32, sig: i32) {
    unsafe {
        libc::kill(-(pid as i32), sig);
    }
}

/// The single agent-facing tool; dispatches on an `action` discriminator.
pub struct ProcessManagerTool {
    supervisor: Arc<ProcessSupervisor>,
}

impl ProcessManagerTool {
    pub fn new(supervisor: Arc<ProcessSupervisor>) -> Self {
        Self { supervisor }
    }

    fn resolve_dir(args: &Value, root: &Path) -> PathBuf {
        match args.get("working_dir").and_then(Value::as_str) {
            Some(dir) if !dir.is_empty() => {
                let p = Path::new(dir);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    root.join(p)
                }
            }
            _ => root.to_path_buf(),
        }
    }

    /// Accept `process_id` as a JSON number or a numeric string.
    fn id_arg(args: &Value) -> Option<u64> {
        match args.get("process_id") {
            Some(Value::Number(n)) => n.as_u64(),
            Some(Value::String(s)) => s.trim().parse().ok(),
            _ => None,
        }
    }
}

#[async_trait]
impl Tool for ProcessManagerTool {
    fn name(&self) -> &str {
        "process_manager"
    }

    fn description(&self) -> &str {
        "Start, poll, and stop background processes (dev server, file watcher, \
         long-running build) that outlive a single turn. Unlike `bash`/`python`, a \
         started process keeps running so you can check its output later. Actions: \
         `start` (run a command in the background, returns a process_id), `poll` \
         (read a process's captured stdout/stderr and status), `list` (all known \
         processes), `stop` (terminate a process). Use `bash`/`python` for commands \
         that finish quickly."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start", "poll", "list", "stop"],
                    "description": "What to do."
                },
                "command": {
                    "type": "string",
                    "description": "For `start`: the shell command to run in the background."
                },
                "working_dir": {
                    "type": "string",
                    "description": "For `start`: directory to run in, relative to the \
                                    workspace root or absolute. Defaults to the workspace root."
                },
                "process_id": {
                    "type": "integer",
                    "description": "For `poll`/`stop`: the id returned by `start`."
                }
            },
            "required": ["action"]
        })
    }

    fn safety(&self, args: &Value) -> Safety {
        match args.get("action").and_then(Value::as_str) {
            Some("poll") | Some("list") => Safety::ReadOnly,
            _ => Safety::Write,
        }
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        match args.get("action").and_then(Value::as_str) {
            Some("start") => {
                let Some(command) = args
                    .get("command")
                    .and_then(Value::as_str)
                    .filter(|c| !c.trim().is_empty())
                else {
                    return ToolOutcome::error("start requires a non-empty `command`");
                };
                let dir = Self::resolve_dir(&args, root);
                if !dir.is_dir() {
                    return ToolOutcome::error(format!(
                        "working_dir does not exist or is not a directory: {}",
                        dir.display()
                    ));
                }
                match self.supervisor.start(command, &dir) {
                    Ok(id) => ToolOutcome::ok(format!(
                        "started process {id}: {command}\npoll with action=poll, process_id={id}"
                    )),
                    Err(e) => ToolOutcome::error(e),
                }
            }
            Some("poll") => {
                let Some(id) = Self::id_arg(&args) else {
                    return ToolOutcome::error("poll requires a numeric `process_id`");
                };
                match self.supervisor.poll(id) {
                    Ok(body) => ToolOutcome::ok(body),
                    Err(e) => ToolOutcome::error(e),
                }
            }
            Some("list") => ToolOutcome::ok(self.supervisor.list()),
            Some("stop") => {
                let Some(id) = Self::id_arg(&args) else {
                    return ToolOutcome::error("stop requires a numeric `process_id`");
                };
                match self.supervisor.stop(id).await {
                    Ok(body) => ToolOutcome::ok(body),
                    Err(e) => ToolOutcome::error(e),
                }
            }
            Some(other) => ToolOutcome::error(format!(
                "unknown action '{other}'; expected start|poll|list|stop"
            )),
            None => ToolOutcome::error("missing required argument: action (start|poll|list|stop)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    /// Poll the private status until the process leaves `Running` or `secs` elapses.
    async fn wait_done(sup: &ProcessSupervisor, id: u64, secs: u64) -> Status {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let st = sup.procs.lock().unwrap().get(&id).unwrap().shared.status();
            if !st.is_running() || Instant::now() > deadline {
                return st;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[test]
    fn ring_buffer_caps_and_flags_truncation() {
        let mut r = RingBuffer::new(10);
        r.extend(b"0123456789ABCDE"); // 15 bytes into a 10-byte cap
        let s = r.snapshot();
        assert!(s.contains("truncated"), "{s}");
        assert!(s.ends_with("56789ABCDE"), "{s}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_captures_stdout_and_exit_code() {
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        let id = sup.start("echo hello-proc", dir.path()).unwrap();
        let st = wait_done(&sup, id, 5).await;
        assert!(matches!(st, Status::Exited(0)), "status was {st:?}");
        let body = sup.poll(id).unwrap();
        assert!(body.contains("hello-proc"), "{body}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nonzero_exit_code_is_captured() {
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        let id = sup.start("exit 3", dir.path()).unwrap();
        let st = wait_done(&sup, id, 5).await;
        assert!(matches!(st, Status::Exited(3)), "status was {st:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sleeper_runs_then_stops() {
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        let id = sup.start("sleep 30", dir.path()).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            sup.procs
                .lock()
                .unwrap()
                .get(&id)
                .unwrap()
                .shared
                .status()
                .is_running(),
            "sleeper should be running before stop"
        );
        let out = sup.stop(id).await.unwrap();
        assert!(out.contains("stopped"), "{out}");
        assert!(
            !sup.procs
                .lock()
                .unwrap()
                .get(&id)
                .unwrap()
                .shared
                .status()
                .is_running(),
            "sleeper should be terminated after stop"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_start_past_concurrency_cap() {
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        for _ in 0..MAX_CONCURRENT {
            sup.start("sleep 30", dir.path()).unwrap();
        }
        let err = sup.start("sleep 30", dir.path()).unwrap_err();
        assert!(err.contains("too many"), "{err}");
        // Running children are SIGKILLed by ProcessSupervisor::drop at scope end.
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_process_id_is_an_error() {
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        let _ = dir;
        assert!(sup.poll(999).is_err());
        assert!(sup.stop(999).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tool_validates_args_and_classifies_safety() {
        let dir = TempDir::new().unwrap();
        let tool = ProcessManagerTool::new(Arc::new(ProcessSupervisor::new()));

        assert!(!tool.run(json!({}), dir.path()).await.success);
        assert!(
            !tool
                .run(json!({"action": "nope"}), dir.path())
                .await
                .success
        );
        assert!(
            !tool
                .run(json!({"action": "start"}), dir.path())
                .await
                .success
        );
        assert!(
            !tool
                .run(json!({"action": "poll"}), dir.path())
                .await
                .success
        );
        assert!(
            tool.run(json!({"action": "list"}), dir.path())
                .await
                .success
        );

        assert_eq!(tool.safety(&json!({"action": "poll"})), Safety::ReadOnly);
        assert_eq!(tool.safety(&json!({"action": "list"})), Safety::ReadOnly);
        assert_eq!(tool.safety(&json!({"action": "start"})), Safety::Write);
        assert_eq!(tool.safety(&json!({"action": "stop"})), Safety::Write);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tool_start_then_poll_by_string_id() {
        let dir = TempDir::new().unwrap();
        let tool = ProcessManagerTool::new(Arc::new(ProcessSupervisor::new()));
        let started = tool
            .run(
                json!({"action": "start", "command": "echo tool-roundtrip"}),
                dir.path(),
            )
            .await;
        assert!(started.success, "{}", started.content);
        let id: u64 = started
            .content
            .split_whitespace()
            .find_map(|w| w.trim_end_matches(':').parse().ok())
            .expect("process id in start output");

        // poll with the id as a string, retrying until output lands
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let out = tool
                .run(
                    json!({"action": "poll", "process_id": id.to_string()}),
                    dir.path(),
                )
                .await;
            assert!(out.success, "{}", out.content);
            if out.content.contains("tool-roundtrip") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "output never appeared: {}",
                out.content
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
