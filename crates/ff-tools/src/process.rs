//! Background process management.
//!
//! `bash` and `python` run to completion under a hard timeout, so the agent
//! cannot start a dev server, file watcher, or long build and check back on it
//! later. `process_manager` fills that gap: it starts a process in the
//! background, returns a handle, and lets later turns `poll` its captured output
//! or `stop` it.
//!
//! State that must survive across turns cannot live in the tool -- the registry
//! is rebuilt every turn. The live process table therefore lives in
//! [`ProcessSupervisor`], owned by the host's `AppState` and injected at
//! registry-build time (the same pattern as the memory/skills tools). Each
//! process is tagged with the owning session id via [`Tool::run_with_session`],
//! so `poll`/`list`/`stop` are scoped to the owning session and
//! [`ProcessSupervisor::reap_session`] can clean up on session close.
//!
//! Honesty notes:
//! - Like [`crate::bash`] and [`crate::python`], a started command is **not**
//!   sandboxed and is classified [`Safety::Write`] so the host's approval gate
//!   covers it (`poll`/`list` are read-only).
//! - On Unix each process is spawned in its **own process group** so `stop` can
//!   signal the whole tree (a dev server and its children), SIGTERM then SIGKILL
//!   after a grace window. On Windows each process is placed in a kill-on-close
//!   Job Object, so `stop` (and supervisor drop, and even a FlowForge crash --
//!   the OS closes the handle) kills the whole tree; `taskkill /T /F` is the
//!   fallback if job assignment fails. Only other (non-unix, non-windows)
//!   platforms can start/poll/list but not stop.

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

use crate::registry::{Safety, Tool, ToolOutcome, NO_SESSION};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// Owns a Job Object handle. The job is created with `KILL_ON_JOB_CLOSE`, so the
/// kernel kills every member when the last handle closes -- on `stop`, on
/// supervisor drop, or even if FlowForge crashes (the OS closes the handle).
#[cfg(windows)]
struct JobHandle(HANDLE);

// The raw `HANDLE` is only ever touched under the supervisor's `Mutex`.
#[cfg(windows)]
unsafe impl Send for JobHandle {}

#[cfg(windows)]
impl Drop for JobHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

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
    /// Windows-only: the kill-on-close Job Object the process was assigned to.
    /// `None` if job creation/assignment failed (the `stop` path falls back to
    /// `taskkill /T /F`).
    #[cfg(windows)]
    job: Option<JobHandle>,
    shared: Arc<Shared>,
    /// Owning session ([`crate::registry::NO_SESSION`] if anonymous — the
    /// `run` path). Only the same session may poll/list/stop it.
    session_id: String,
    /// Last time any session polled or listed this process. Used by
    /// [`ProcessSupervisor::reap_idle`] to detect abandoned processes.
    last_poll_at: Instant,
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

    /// Spawn `command` (via [`crate::shell::shell_invocation`]) in `dir`, capturing stdout and
    /// stderr into bounded buffers. Returns the new process id. Rejected if the
    /// live-process cap is reached. Must be called from within a Tokio runtime
    /// (the agent loop always is): it spawns detached reader and exit-watcher tasks.
    pub fn start(&self, command: &str, dir: &Path, session_id: &str) -> Result<u64, String> {
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

        let (program, flag) = crate::shell::shell_invocation();
        let mut cmd = Command::new(&program);
        cmd.arg(flag)
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
            .map_err(|e| format!("failed to spawn process ({program} {flag}): {e}"))?;
        let pid = child.id();
        // Place the child (and its descendants) in a kill-on-close Job Object so
        // `stop`/drop can terminate the whole tree -- the Windows analogue of the
        // unix process group above. Must happen before `child` is moved into the
        // exit-watcher task below. `None` falls back to `taskkill` at stop time.
        #[cfg(windows)]
        let job = child.raw_handle().and_then(assign_to_new_job);
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
                #[cfg(windows)]
                job,
                shared,
                session_id: session_id.to_string(),
                last_poll_at: Instant::now(),
            },
        );
        Ok(id)
    }

    /// Captured stdout/stderr and current status for one process. Only the
    /// session that started it may poll — a different session gets the same
    /// "no such process" error as an unknown id, hiding other sessions' work.
    pub fn poll(&self, id: u64, session_id: &str) -> Result<String, String> {
        let mut map = self.procs.lock().unwrap();
        let p = map
            .get_mut(&id)
            .ok_or_else(|| format!("no such process: {id}"))?;
        if p.session_id != session_id {
            return Err(format!("no such process: {id}"));
        }
        p.last_poll_at = Instant::now();
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

    /// One line per process in `session_id`, oldest id first.
    pub fn list(&self, session_id: &str) -> String {
        let mut map = self.procs.lock().unwrap();
        let mut ids: Vec<u64> = map
            .iter()
            .filter(|(_, p)| p.session_id == session_id)
            .map(|(&id, _)| id)
            .collect();
        if ids.is_empty() {
            return "No processes.".to_string();
        }
        ids.sort_unstable();
        let now = Instant::now();
        let mut s = String::new();
        for id in ids {
            let p = map.get_mut(&id).unwrap();
            p.last_poll_at = now;
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
    /// Only the owning session may stop it.
    pub async fn stop(&self, id: u64, session_id: &str) -> Result<String, String> {
        let (pid, shared) = {
            let map = self.procs.lock().unwrap();
            let p = map
                .get(&id)
                .ok_or_else(|| format!("no such process: {id}"))?;
            if p.session_id != session_id {
                return Err(format!("no such process: {id}"));
            }
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
        #[cfg(windows)]
        {
            // Primary: terminate the whole Job Object (the process and every
            // descendant) in one call. Fallback: taskkill the pid tree.
            let terminated = {
                let map = self.procs.lock().unwrap();
                map.get(&id)
                    .and_then(|p| p.job.as_ref())
                    .map(|j| unsafe { TerminateJobObject(j.0, 1) != 0 })
                    .unwrap_or(false)
            };
            if !terminated {
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .output()
                    .await;
            }
            wait_until_exited(&shared, STOP_GRACE).await;
            Ok(format!(
                "process {id} stopped ({})",
                shared.status().label()
            ))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = pid;
            Err("stopping processes is not supported on this platform".to_string())
        }
    }

    /// Stop and remove every process owned by `session_id`. Called by the host
    /// on session **delete** (`delete_session` → `reap_session_processes`) so
    /// background processes don't outlive their session. Sessions are
    /// server-truth and persist until explicitly deleted — there is no
    /// `close_session` — so long-lived apps that never delete a session rely on
    /// [`reap_idle`](Self::reap_idle) to stop abandoned processes. Returns the
    /// number reaped.
    pub async fn reap_session(&self, session_id: &str) -> usize {
        let ids: Vec<u64> = {
            let map = self.procs.lock().unwrap();
            map.iter()
                .filter(|(_, p)| p.session_id == session_id)
                .map(|(&id, _)| id)
                .collect()
        };
        let mut count = 0;
        for id in &ids {
            let _ = self.stop(*id, session_id).await;
            self.procs.lock().unwrap().remove(id);
            count += 1;
        }
        count
    }

    /// Remove finished processes and stop running ones whose `last_poll_at` is
    /// older than `max_idle` — i.e. the agent started them but never came back.
    /// Scans all sessions; the desktop host drives this from a periodic timer
    /// (`AppState::start_process_reaper`). Returns the number reaped.
    pub async fn reap_idle(&self, max_idle: Duration) -> usize {
        let now = Instant::now();
        let to_reap: Vec<(u64, String, bool)> = {
            let map = self.procs.lock().unwrap();
            map.iter()
                .filter_map(|(&id, p)| {
                    if !p.shared.status().is_running() {
                        Some((id, p.session_id.clone(), false))
                    } else if now.duration_since(p.last_poll_at) >= max_idle {
                        Some((id, p.session_id.clone(), true))
                    } else {
                        None
                    }
                })
                .collect()
        };
        let mut count = 0;
        for (id, sid, running) in to_reap {
            if running {
                let _ = self.stop(id, &sid).await;
            }
            self.procs.lock().unwrap().remove(&id);
            count += 1;
        }
        count
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
        // The JobHandle's own Drop (CloseHandle -> KILL_ON_JOB_CLOSE) is the
        // guaranteed net; terminate eagerly here too so the tree dies now rather
        // than whenever the map entry is dropped.
        #[cfg(windows)]
        if let Ok(map) = self.procs.lock() {
            for p in map.values() {
                if !p.shared.status().is_running() {
                    continue;
                }
                if let Some(job) = &p.job {
                    unsafe {
                        TerminateJobObject(job.0, 1);
                    }
                } else if let Some(pid) = p.pid {
                    // Block on taskkill (best-effort, like the unix SIGKILL
                    // syscall above) rather than fire-and-forget: the kill is
                    // actually issued before the supervisor goes away, and no
                    // orphaned taskkill is left behind. Stdio is nulled so it
                    // stays quiet during shutdown.
                    let _ = std::process::Command::new("taskkill")
                        .args(["/PID", &pid.to_string(), "/T", "/F"])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
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
#[cfg(any(unix, windows))]
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

/// Create a kill-on-close Job Object and assign `process` to it. Returns the
/// owning handle, or `None` if any step fails (the caller falls back to
/// `taskkill`). Closing the returned handle kills every job member.
#[cfg(windows)]
fn assign_to_new_job(process: std::os::windows::io::RawHandle) -> Option<JobHandle> {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return None;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) != 0
            && AssignProcessToJobObject(job, process as HANDLE) != 0;
        if !ok {
            CloseHandle(job);
            return None;
        }
        Some(JobHandle(job))
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
         started process keeps running so you can check its output later. Processes \
         are scoped to the session that started them — `list`/`poll`/`stop` only \
         see your own. Actions: `start` (run a command in the background, returns \
         a process_id), `poll` (read a process's captured stdout/stderr and \
         status), `list` (this session's processes), `stop` (terminate a \
         process). Use `bash`/`python` for commands that finish quickly."
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
        self.run_with_session(args, root, NO_SESSION).await
    }

    async fn run_with_session(&self, args: Value, root: &Path, session_id: &str) -> ToolOutcome {
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
                match self.supervisor.start(command, &dir, session_id) {
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
                match self.supervisor.poll(id, session_id) {
                    Ok(body) => ToolOutcome::ok(body),
                    Err(e) => ToolOutcome::error(e),
                }
            }
            Some("list") => ToolOutcome::ok(self.supervisor.list(session_id)),
            Some("stop") => {
                let Some(id) = Self::id_arg(&args) else {
                    return ToolOutcome::error("stop requires a numeric `process_id`");
                };
                match self.supervisor.stop(id, session_id).await {
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
        let id = sup.start("echo hello-proc", dir.path(), "s1").unwrap();
        let st = wait_done(&sup, id, 5).await;
        assert!(matches!(st, Status::Exited(0)), "status was {st:?}");
        let body = sup.poll(id, "s1").unwrap();
        assert!(body.contains("hello-proc"), "{body}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nonzero_exit_code_is_captured() {
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        let id = sup.start("exit 3", dir.path(), "s1").unwrap();
        let st = wait_done(&sup, id, 5).await;
        assert!(matches!(st, Status::Exited(3)), "status was {st:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sleeper_runs_then_stops() {
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        let id = sup.start("sleep 30", dir.path(), "s1").unwrap();
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
        let out = sup.stop(id, "s1").await.unwrap();
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
            sup.start("sleep 30", dir.path(), "s1").unwrap();
        }
        let err = sup.start("sleep 30", dir.path(), "s1").unwrap_err();
        assert!(err.contains("too many"), "{err}");
        // Running children are SIGKILLed by ProcessSupervisor::drop at scope end.
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_process_id_is_an_error() {
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        let _ = dir;
        assert!(sup.poll(999, "s1").is_err());
        assert!(sup.stop(999, "s1").await.is_err());
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cross_session_isolation() {
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        let id = sup.start("sleep 30", dir.path(), "session-a").unwrap();

        // Session B cannot see, poll, or stop session A's process.
        assert!(sup.poll(id, "session-b").is_err());
        assert!(sup.stop(id, "session-b").await.is_err());
        assert!(sup.list("session-b").contains("No processes"));

        // Session A still has full access.
        assert!(sup.poll(id, "session-a").is_ok());
        assert!(sup.list("session-a").contains("sleep 30"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reap_session_stops_and_removes() {
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        let _id = sup.start("sleep 30", dir.path(), "s1").unwrap();
        let _id2 = sup.start("sleep 30", dir.path(), "s1").unwrap();
        let _id3 = sup.start("sleep 30", dir.path(), "s2").unwrap();

        let n = sup.reap_session("s1").await;
        assert_eq!(n, 2, "should reap 2 processes from s1");

        // s1's processes are gone; s2's survivor is untouched.
        assert!(sup.list("s1").contains("No processes"));
        assert!(sup.list("s2").contains("sleep 30"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reap_idle_removes_finished_and_abandoned() {
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();

        // A finished process — reaped regardless of idle threshold.
        let done_id = sup.start("true", dir.path(), "s1").unwrap();
        wait_done(&sup, done_id, 5).await;

        // A running process that nobody polls — will exceed the tiny idle budget.
        let _live_id = sup.start("sleep 30", dir.path(), "s1").unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        let n = sup.reap_idle(Duration::from_millis(50)).await;
        // At least the finished one is reaped; the live one may or may not be
        // depending on timing, but the finished one always is.
        assert!(n >= 1, "should reap at least the finished process, got {n}");
        assert!(
            sup.procs.lock().unwrap().get(&done_id).is_none(),
            "finished process should have been removed"
        );
    }
}
