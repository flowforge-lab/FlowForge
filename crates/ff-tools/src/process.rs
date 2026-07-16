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
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::{broadcast, mpsc};

use crate::registry::{Safety, Tool, ToolOutcome, NO_SESSION};
use crate::sink::{OutputSink, OutputStream};
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
/// Capacity of the per-process broadcast channel observers (Phase 3, #893)
/// subscribe to. Bounded so a chatty process can't grow the channel unbounded
/// for slow subscribers; the oldest chunks get dropped on overflow. Chosen
/// generously so a normal dev server is unlikely to lag, and a chatty one
/// sees lagged (not closed) receivers — `ObserverTool`'s docstring warns
/// about this.
const SUBSCRIBER_CAPACITY: usize = 64;

/// One chunk of process output delivered to a broadcast subscriber. Each
/// `drain` task pushes one of these per `read()` so the receiver sees the
/// same stream of bytes the ring buffer captures — the only difference is
/// shape (`Bytes` instead of a UTF-8 lossy snapshot).
#[derive(Clone, Debug)]
pub struct ProcessChunk {
    pub stream: OutputStream,
    pub bytes: Bytes,
}

/// A process-lifecycle notification the supervisor emits to an opt-in host
/// listener (#873). The desktop installs one listener via
/// [`ProcessSupervisor::lifecycle_channel`] and, on each `Started`, spawns a
/// bridge task that forwards `rx` chunks to the frontend as `process:output`
/// events — live output that outlives the `start` tool call and any single
/// turn. Headless callers (tests, the CLI) never install a listener, so this
/// costs nothing and process behavior is unchanged.
///
/// `rx` is subscribed *inside* `start`, before the drain tasks spawn, so the
/// bridge receives output from the first byte with no head-of-stream gap.
#[derive(Debug)]
pub enum ProcessLifecycle {
    Started {
        id: u64,
        session_id: String,
        command: String,
        rx: broadcast::Receiver<ProcessChunk>,
    },
}

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
    /// Phase 3 (#893): broadcast sender observers subscribe to. `None` until
    /// the first `subscribe()` call (lazily created to avoid a channel per
    /// unobserved process). Dropped by the exit-watcher when the process
    /// terminates, so subscribers see `RecvError::Closed` and exit.
    subscribers: Mutex<Option<broadcast::Sender<ProcessChunk>>>,
}

impl Shared {
    fn new(cap: usize) -> Self {
        Self {
            stdout: Mutex::new(RingBuffer::new(cap)),
            stderr: Mutex::new(RingBuffer::new(cap)),
            status: Mutex::new(Status::Running),
            subscribers: Mutex::new(None),
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
    /// Opt-in host listener for process lifecycle events (#873). `None` until
    /// [`lifecycle_channel`](Self::lifecycle_channel) installs it. When present,
    /// `start` arms the per-process broadcast eagerly and emits a
    /// [`ProcessLifecycle::Started`]; when absent, `start` behaves exactly as
    /// before (lazy broadcast, no notification).
    lifecycle_tx: Mutex<Option<mpsc::UnboundedSender<ProcessLifecycle>>>,
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
            lifecycle_tx: Mutex::new(None),
        }
    }

    /// Install the (single) host lifecycle listener and return its receiver
    /// (#873). Call once at startup — a second call replaces the sender and
    /// returns a fresh receiver, orphaning the previous one (a misuse; the
    /// desktop calls it exactly once when building [`AppState`]). While a
    /// listener is installed, [`start`](Self::start) pre-arms each process's
    /// output broadcast so the bridge sees output from the first byte.
    pub fn lifecycle_channel(&self) -> mpsc::UnboundedReceiver<ProcessLifecycle> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.lifecycle_tx.lock().unwrap() = Some(tx);
        rx
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

        // #873: if a host lifecycle listener is installed, pre-arm the output
        // broadcast and subscribe *before* the drain tasks spawn, so the bridge
        // receives output from the first byte (a lazily-created channel would
        // miss everything written before the first `subscribe`). No listener ->
        // `None`, and the broadcast stays lazy exactly as before.
        let lifecycle = self.lifecycle_tx.lock().unwrap().clone();
        let lifecycle_rx = lifecycle.as_ref().map(|_| {
            shared
                .subscribers
                .lock()
                .unwrap()
                .get_or_insert_with(|| broadcast::channel(SUBSCRIBER_CAPACITY).0)
                .subscribe()
        });

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
            // Phase 3 (#893): drop the broadcast sender so any
            // observers see `RecvError::Closed` and their `next_event`
            // returns `None` (supervisor task ends, entry reaped).
            *watch.subscribers.lock().unwrap() = None;
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

        // #873: notify the host so it can bridge this process's output to the
        // frontend. `lifecycle_rx` is `Some` iff a listener was installed, in
        // which case it was subscribed above (pre-drain), so no output is lost.
        if let (Some(tx), Some(rx)) = (lifecycle, lifecycle_rx) {
            let _ = tx.send(ProcessLifecycle::Started {
                id,
                session_id: session_id.to_string(),
                command: command.to_string(),
                rx,
            });
        }

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

    /// The current status label for a process owned by `session_id`
    /// (`"running"`, `"exited(0)"`, `"killed"`, `"failed: ..."`), or `None`
    /// for an unknown id or one owned by another session (#873). The desktop
    /// bridge reads this when the output broadcast closes, to fill the
    /// terminal `process:exited` event's `status`.
    pub fn status_label(&self, id: u64, session_id: &str) -> Option<String> {
        let map = self.procs.lock().unwrap();
        let p = map.get(&id)?;
        if p.session_id != session_id {
            return None;
        }
        Some(p.shared.status().label())
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

    /// True iff `id` exists, is owned by `session_id`, and is still running.
    /// Phase 3 (#893): the observer supervisor uses this to gate
    /// `ProcessSource` construction — a process that already exited (or
    /// belongs to a different session, or never existed) gets a clean
    /// "no such process" error before any broadcast channel is created.
    pub fn is_alive(&self, id: u64, session_id: &str) -> bool {
        let map = self.procs.lock().unwrap();
        match map.get(&id) {
            Some(p) => p.session_id == session_id && p.shared.status().is_running(),
            None => false,
        }
    }

    /// Subscribe to appended bytes for a running process owned by `session_id`.
    /// The receiver yields stdout and stderr chunks as they are appended to
    /// the ring buffers. Returns `None` for an unknown id, an id owned by a
    /// different session, or a process that has already exited. Phase 3
    /// (#893): backs the `process` observer source.
    ///
    /// Lazily creates the per-process broadcast channel on the first
    /// subscriber, so a process with no observers pays no cost beyond the
    /// `Mutex<Option<...>>` lock check on every drain. The sender is
    /// dropped by the exit-watcher task, so subscribers see
    /// `RecvError::Closed` and their `next_event` returns `None` when the
    /// process ends.
    pub fn subscribe(
        &self,
        id: u64,
        session_id: &str,
    ) -> Option<tokio::sync::broadcast::Receiver<ProcessChunk>> {
        let shared = {
            let map = self.procs.lock().unwrap();
            let p = map.get(&id)?;
            if p.session_id != session_id || !p.shared.status().is_running() {
                return None;
            }
            p.shared.clone()
        };
        // TOCTOU: the exit-watcher can run between releasing `procs`
        // above and acquiring `subscribers` here, completing both
        // `*status = Exited` and `*subscribers = None`. Re-check status
        // *under the subscribers lock* — if the process has exited,
        // bail before `get_or_insert_with` materializes a fresh
        // `Sender` that nobody will ever drop, leaving the
        // would-be subscriber's `rx.recv()` to hang forever.
        let mut guard = shared.subscribers.lock().unwrap();
        if !shared.status().is_running() {
            return None;
        }
        let tx = guard
            .get_or_insert_with(|| broadcast::channel(SUBSCRIBER_CAPACITY).0)
            .clone();
        drop(guard);
        Some(tx.subscribe())
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

/// Drain a child pipe into the shared ring until EOF. Each read is also
/// pushed to the broadcast channel (Phase 3, #893) so subscribed observers
/// see the same byte stream the ring buffer captures. Locking: take the
/// ring lock just long enough to extend, then release; broadcast::send is
/// non-blocking and only briefly grabs the subscribers mutex.
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
                // Fire the broadcast after releasing the ring lock. A
                // sender may be absent (no observers yet) or full (slow
                // observer); both cases are `send` returning an error,
                // which we ignore — lag drops chunks, not the process.
                if let Some(tx) = shared.subscribers.lock().unwrap().as_ref() {
                    let _ = tx.send(ProcessChunk {
                        stream: if is_err {
                            OutputStream::Stderr
                        } else {
                            OutputStream::Stdout
                        },
                        bytes: Bytes::copy_from_slice(&buf[..n]),
                    });
                }
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
///
/// Each failure branch emits a `tracing::warn!` with `last_os_error()` so a
/// persistent failure (nested-job restrictions, sandboxed env, etc.) shows up
/// in the host log instead of silently degrading to `taskkill` with no
/// explanation. A `tracing` subscriber is not installed in this crate, so the
/// warnings are a no-op until a host binary (`apps/cli`, `apps/desktop`) wires
/// one up — same pattern as the rest of the workspace.
#[cfg(windows)]
fn assign_to_new_job(process: std::os::windows::io::RawHandle) -> Option<JobHandle> {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            tracing::warn!(
                error = ?std::io::Error::last_os_error(),
                "process: CreateJobObjectW failed; stop will fall back to taskkill"
            );
            return None;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            tracing::warn!(
                error = ?std::io::Error::last_os_error(),
                "process: SetInformationJobObject failed; stop will fall back to taskkill"
            );
            CloseHandle(job);
            return None;
        }
        if AssignProcessToJobObject(job, process as HANDLE) == 0 {
            tracing::warn!(
                error = ?std::io::Error::last_os_error(),
                "process: AssignProcessToJobObject failed (process may already be in a job); \
                 stop will fall back to taskkill"
            );
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
    // reaches_network: keeps the fail-safe `true` default (RFC 0013) — it
    // launches arbitrary background processes (a dev server, `curl`, anything).
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

    /// Streaming override (#680 V2): for `poll` and `list` actions, emit the captured
    /// output to the sink so the frontend renders it progressively in the tool-call
    /// block. For `start`/`stop`, delegates to `run_with_session` (one-shot output).
    async fn run_streaming(
        &self,
        args: Value,
        root: &Path,
        session_id: &str,
        sink: Option<OutputSink>,
    ) -> ToolOutcome {
        let outcome = self.run_with_session(args, root, session_id).await;
        // Emit the result to the live sink so the FE can render it progressively.
        // This is most useful for `poll` (potentially large buffered output from a
        // running server) but we emit for all actions uniformly.
        if let Some(sink) = sink {
            if !outcome.content.is_empty() {
                sink.emit(OutputStream::Stdout, outcome.content.clone());
            }
        }
        outcome
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_streaming_emits_poll_output_to_sink() {
        let dir = TempDir::new().unwrap();
        let sup = Arc::new(ProcessSupervisor::new());
        let tool = ProcessManagerTool::new(sup.clone());

        // Start a process that finishes quickly with known output.
        let start_out = tool
            .run_with_session(
                json!({"action": "start", "command": "echo hello_stream"}),
                dir.path(),
                "s1",
            )
            .await;
        assert!(start_out.success, "{}", start_out.content);
        // Extract process id from "started process 1: ..."
        let id: u64 = start_out
            .content
            .split_whitespace()
            .nth(2)
            .unwrap()
            .trim_end_matches(':')
            .parse()
            .unwrap();

        // Wait for process to finish.
        wait_done(&sup, id, 5).await;

        // Poll with a sink — should emit the output.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = OutputSink::new(tx);
        let poll_out = tool
            .run_streaming(
                json!({"action": "poll", "process_id": id}),
                dir.path(),
                "s1",
                Some(sink),
            )
            .await;
        assert!(poll_out.success);
        assert!(poll_out.content.contains("hello_stream"));

        // The sink received the same content.
        let mut streamed = String::new();
        while let Ok((_stream, delta)) = rx.try_recv() {
            streamed.push_str(&delta);
        }
        assert!(
            streamed.contains("hello_stream"),
            "sink should contain poll output: {streamed}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_yields_new_bytes_only() {
        // Phase 3 (#893): a subscriber sees bytes appended after the
        // `subscribe` call, never the bytes already in the ring buffer.
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        // Use a file-gate so the process blocks until we explicitly release
        // it — no timing assumptions. The process polls for a gate file
        // before echoing, guaranteeing we can subscribe first (#958).
        let gate = dir.path().join("gate");
        #[cfg(not(windows))]
        let cmd = format!(
            "while [ ! -f '{}' ]; do sleep 0.01; done; echo hello-stream",
            gate.display()
        );
        #[cfg(windows)]
        let cmd = format!(
            "while (-not (Test-Path '{}')) {{ Start-Sleep -Milliseconds 10 }}; Write-Output 'hello-stream'",
            gate.display().to_string().replace('\\', "/")
        );
        let id = sup.start(&cmd, dir.path(), "s1").unwrap();
        // Subscribe while process is blocked on the gate.
        let mut rx = sup
            .subscribe(id, "s1")
            .expect("subscribe returns a receiver for a live process");
        // Release the gate — process will echo now.
        std::fs::write(&gate, "go").unwrap();
        // Wait for the first chunk to arrive (or fail the test).
        let chunk = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("chunk arrives within budget")
            .expect("subscribe recv ok");
        assert_eq!(chunk.stream, OutputStream::Stdout);
        let text = std::str::from_utf8(&chunk.bytes).unwrap_or("");
        assert!(
            text.contains("hello-stream"),
            "chunk should contain echoed text, got {text:?}"
        );
        let _ = sup.stop(id, "s1").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_cross_session_returns_none() {
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        let id = sup.start("sleep 30", dir.path(), "session-a").unwrap();
        // Foreign session is invisible — same wording class as `poll`.
        assert!(sup.subscribe(id, "session-b").is_none());
        // Owning session can subscribe.
        assert!(sup.subscribe(id, "session-a").is_some());
        let _ = sup.stop(id, "session-a").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_unknown_id_returns_none() {
        let sup = ProcessSupervisor::new();
        assert!(sup.subscribe(999, "s1").is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_after_exit_returns_none() {
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        let id = sup.start("true", dir.path(), "s1").unwrap();
        // Wait for the process to actually exit before subscribing.
        let _ = wait_done(&sup, id, 5).await;
        assert!(
            sup.subscribe(id, "s1").is_none(),
            "subscribe after exit must return None"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_recv_closed_after_exit() {
        // The exit-watcher drops the broadcast sender, so an existing
        // subscriber's next recv returns `RecvError::Closed`.
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        let id = sup.start("true", dir.path(), "s1").unwrap();
        let mut rx = sup.subscribe(id, "s1").expect("subscribe while running");
        // Wait for exit; the drain task and exit watcher are detached,
        // so a small sleep is the pragmatic synchronization point.
        let _ = wait_done(&sup, id, 5).await;
        // Give the exit-watcher a moment to drop the sender.
        for _ in 0..50 {
            if matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Closed)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("subscriber should observe RecvError::Closed after exit");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lifecycle_started_carries_zero_loss_receiver() {
        // #873: with a lifecycle listener installed, `start` emits a
        // `Started` whose `rx` was subscribed *before* the drain tasks, so it
        // sees output from the first byte — even output emitted before the
        // host has a chance to observe the `Started` event.
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        let mut lc_rx = sup.lifecycle_channel();
        // Emits immediately, then lingers so the exit-watcher does not drop the
        // sender before we read the head chunk.
        let id = sup
            .start("echo head-line; sleep 30", dir.path(), "s1")
            .unwrap();

        let ev = tokio::time::timeout(Duration::from_secs(2), lc_rx.recv())
            .await
            .expect("Started arrives within budget")
            .expect("lifecycle channel open");
        let ProcessLifecycle::Started {
            id: ev_id,
            session_id,
            command,
            mut rx,
        } = ev;
        assert_eq!(ev_id, id);
        assert_eq!(session_id, "s1");
        assert_eq!(command, "echo head-line; sleep 30");

        // The head line must be delivered even though it was echoed before we
        // pulled the Started event off the channel.
        let mut seen = String::new();
        while let Ok(Ok(chunk)) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            seen.push_str(&String::from_utf8_lossy(&chunk.bytes));
            if seen.contains("head-line") {
                break;
            }
        }
        assert!(
            seen.contains("head-line"),
            "pre-subscribed rx must capture head output, got {seen:?}"
        );
        let _ = sup.stop(id, "s1").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_lifecycle_listener_is_noop() {
        // Without a listener installed, `start` behaves exactly as before: no
        // notification, broadcast stays lazy, poll still works.
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        let id = sup.start("echo hi", dir.path(), "s1").unwrap();
        let st = wait_done(&sup, id, 5).await;
        assert!(matches!(st, Status::Exited(0)), "status was {st:?}");
        assert!(sup.poll(id, "s1").unwrap().contains("hi"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn status_label_reports_exit() {
        let dir = TempDir::new().unwrap();
        let sup = ProcessSupervisor::new();
        let id = sup.start("exit 3", dir.path(), "s1").unwrap();
        let _ = wait_done(&sup, id, 5).await;
        assert_eq!(sup.status_label(id, "s1").as_deref(), Some("exited(3)"));
        // Foreign session / unknown id are invisible.
        assert!(sup.status_label(id, "other").is_none());
        assert!(sup.status_label(9999, "s1").is_none());
    }

    // ----- Windows-only runtime coverage for the Job Object kill path (#607) -----
    //
    // The unix tests above use `echo` / `sleep` / `exit`, which aren't on
    // stock Windows. `cmd /C` is, so we exercise the full `start` -> `stop`
    // round-trip with a real cmd -> ping tree. The unix path is byte-for-byte
    // unchanged; these tests are pure CI coverage for the `#605` Windows code
    // path (and the `#607` `tracing::warn!` diagnostics, which fire if any
    // step of `assign_to_new_job` fails on the runner image).
    #[cfg(windows)]
    mod windows {
        use super::*;

        /// Run `tasklist /FI "IMAGENAME eq <name>"` and return true if any
        /// process with that image name is listed. Polls until the predicate
        /// matches or the budget elapses, returning the final result. We
        /// shell out rather than `OpenProcess`/`EnumProcesses` to keep the
        /// test dependency-free.
        async fn tasklist_has_image(name: &str, budget: Duration) -> bool {
            let deadline = Instant::now() + budget;
            loop {
                let out = std::process::Command::new("tasklist")
                    .args(["/FI", &format!("IMAGENAME eq {name}"), "/NH"])
                    .output();
                let has = matches!(out, Ok(o) if {
                    let s = String::from_utf8_lossy(&o.stdout);
                    // `tasklist` prints "INFO: No tasks are running..." when
                    // the filter matches nothing, and a row with the image
                    // name when something matches. Substring check is
                    // sufficient and case-insensitive matches `tasklist`'s
                    // canonical capitalisation.
                    s.to_ascii_uppercase().contains(&name.to_ascii_uppercase())
                });
                if has || Instant::now() >= deadline {
                    return has;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }

        /// Run `cmd /C "ping -t 127.0.0.1 -n 60"`, verify both the cmd parent
        /// and the ping child are alive via `tasklist`, call `stop`, and
        /// assert the *whole tree* is gone (no `PING.EXE` left) within a
        /// short window. This is the runtime proof that the Job Object
        /// (`KILL_ON_JOB_CLOSE` + `TerminateJobObject`) terminates
        /// descendants, not just the immediate child.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn stop_kills_a_cmd_spawned_ping_tree() {
            let dir = TempDir::new().unwrap();
            let sup = ProcessSupervisor::new();
            // `ping -t` runs until told to stop; `-n 60` is a 60-packet
            // ceiling (a safety net -- `stop` should beat it by minutes).
            // `cmd /C` waits for the child, so we have a real cmd -> ping
            // tree rather than a self-terminating shell.
            let id = sup
                .start("cmd /C \"ping -t 127.0.0.1 -n 60\"", dir.path(), "s1")
                .expect("start should succeed");
            // Give cmd a moment to spawn ping. If we stop before ping
            // appears, the "tree is gone" assertion would pass trivially
            // (there was never a tree), so we *first* assert ping is alive
            // -- otherwise the test would silently test nothing.
            assert!(
                tasklist_has_image("PING.EXE", Duration::from_secs(5)).await,
                "ping child never appeared; test cannot prove tree-kill"
            );

            let out = sup.stop(id, "s1").await.expect("stop should succeed");
            assert!(out.contains("stopped"), "{out}");

            // Give the kernel a moment to reap the tree; ping should be
            // gone well within a couple of seconds.
            let deadline = Instant::now() + Duration::from_secs(5);
            while tasklist_has_image("PING.EXE", Duration::from_millis(0)).await {
                assert!(
                    Instant::now() < deadline,
                    "PING.EXE still running 5s after stop -- tree was not killed"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}
