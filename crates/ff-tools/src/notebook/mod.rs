//! `notebook_runner` tool — a session-scoped, stateful Python kernel (#859,
//! epic #856). Phase 1: `start` / `run_cell` / `status` / `stop`, one kernel per
//! session. The kernel (see [`kernel`]) is a persistent `python3` subprocess with
//! module globals that survive across cells; the supervisor keys kernels by
//! session id and is reaped when the session ends (host wiring), mirroring
//! [`crate::process::ProcessSupervisor`].

mod kernel;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;

use crate::registry::{Safety, Tool, ToolOutcome, NO_SESSION};
use kernel::{KernelState, DEFAULT_CELL_TIMEOUT_SECS, MAX_CELL_TIMEOUT_SECS};

/// Manages one persistent Python kernel per session (Phase 1). Behind a `Mutex`
/// so the single-threaded kernel stdin/stdout is never interleaved across cells.
#[derive(Default)]
pub struct KernelSupervisor {
    kernels: Mutex<HashMap<String, KernelState>>,
}

impl KernelSupervisor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start (or replace a dead) kernel for `session_id`. Refuses to clobber a
    /// live kernel — the caller should `stop` first, so state isn't lost by
    /// accident.
    async fn start(&self, session_id: &str, dir: &Path) -> Result<String, String> {
        let mut kernels = self.kernels.lock().await;
        if let Some(k) = kernels.get(session_id) {
            if !k.dead {
                return Err(format!(
                    "a kernel is already running for this session ({}); stop it first",
                    k.kernel_id
                ));
            }
        }
        let kernel = KernelState::spawn(dir).await?;
        let id = kernel.kernel_id.clone();
        kernels.insert(session_id.to_string(), kernel);
        Ok(id)
    }

    async fn run_cell(
        &self,
        session_id: &str,
        code: &str,
        timeout_secs: u64,
    ) -> Result<kernel::CellResult, String> {
        let mut kernels = self.kernels.lock().await;
        let kernel = kernels
            .get_mut(session_id)
            .ok_or("no kernel for this session; call action=start first")?;
        kernel.run_cell(code, timeout_secs).await
    }

    async fn status(&self, session_id: &str) -> String {
        let kernels = self.kernels.lock().await;
        match kernels.get(session_id) {
            None => "no kernel running for this session".to_string(),
            Some(k) => {
                let state = if k.dead { "dead" } else { "running" };
                format!(
                    "kernel {} — {state}; pid={}; cells executed={}",
                    k.kernel_id,
                    k.pid().map(|p| p.to_string()).unwrap_or_else(|| "?".into()),
                    k.execution_count
                )
            }
        }
    }

    async fn stop(&self, session_id: &str) -> Result<String, String> {
        let mut kernels = self.kernels.lock().await;
        match kernels.remove(session_id) {
            None => Err("no kernel running for this session".into()),
            Some(mut k) => {
                let id = k.kernel_id.clone();
                k.stop().await;
                Ok(format!("stopped kernel {id}"))
            }
        }
    }

    /// Kill and drop every kernel for `session_id` (host calls this on session
    /// end so a long-lived kernel never leaks). Returns how many were reaped.
    pub async fn reap_session(&self, session_id: &str) -> usize {
        let mut kernels = self.kernels.lock().await;
        match kernels.remove(session_id) {
            Some(mut k) => {
                k.stop().await;
                1
            }
            None => 0,
        }
    }
}

/// The `notebook_runner` tool. Holds a shared [`KernelSupervisor`]; session
/// scoping arrives via [`Tool::run_streaming`]'s `session_id` (the base
/// [`Tool::run`] has none, so it uses [`NO_SESSION`]).
pub struct NotebookTool {
    supervisor: Arc<KernelSupervisor>,
}

impl NotebookTool {
    pub fn new(supervisor: Arc<KernelSupervisor>) -> Self {
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

    fn resolve_timeout(args: &Value) -> u64 {
        args.get("timeout_secs")
            .and_then(Value::as_u64)
            .map(|s| s.clamp(1, MAX_CELL_TIMEOUT_SECS))
            .unwrap_or(DEFAULT_CELL_TIMEOUT_SECS)
    }
}

#[async_trait]
impl Tool for NotebookTool {
    fn name(&self) -> &str {
        "notebook_runner"
    }

    fn description(&self) -> &str {
        "Run Python cell-at-a-time in a persistent kernel whose variables PERSIST \
         across calls — unlike `python` (fresh interpreter each call). Use it to \
         build up state incrementally: define something in one cell, use it in the \
         next. The kernel is scoped to this session and shares its `.venv`. \
         Actions: `start` (spawn the kernel), `run_cell` (run inline `code`, returns \
         its stdout/stderr), `status` (kernel state + cells run), `stop` (kill it). \
         Each cell has a per-call timeout; a hung cell is interrupted then killed."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start", "run_cell", "status", "stop"],
                    "description": "What to do."
                },
                "code": {
                    "type": "string",
                    "description": "For `run_cell`: the Python source to execute in the kernel."
                },
                "working_dir": {
                    "type": "string",
                    "description": "For `start`: directory to run in (venv discovery + cwd), \
                                    relative to the workspace root or absolute. Defaults to root."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "For `run_cell`: per-cell wall-clock budget (default 60, \
                                    max 600). A cell that overruns is interrupted, then killed."
                }
            },
            "required": ["action"]
        })
    }

    fn safety(&self, args: &Value) -> Safety {
        match args.get("action").and_then(Value::as_str) {
            // Reading kernel state mutates nothing → usable in Plan.
            Some("status") => Safety::ReadOnly,
            // Stopping the kernel is a benign, reversible teardown.
            Some("stop") => Safety::Write,
            // start / run_cell spawn and execute arbitrary Python.
            _ => Safety::Dangerous,
        }
    }

    fn min_safety(&self) -> Safety {
        // `status` is read-only, so advertise in Plan; the per-call `safety`
        // still gates `start`/`run_cell` (Dangerous) out of Plan.
        Safety::ReadOnly
    }

    fn max_safety(&self) -> Safety {
        Safety::Dangerous
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        self.run_with_session(args, root, NO_SESSION).await
    }

    async fn run_with_session(&self, args: Value, root: &Path, session_id: &str) -> ToolOutcome {
        match args.get("action").and_then(Value::as_str) {
            Some("start") => {
                let dir = Self::resolve_dir(&args, root);
                if !dir.is_dir() {
                    return ToolOutcome::error(format!(
                        "working_dir does not exist or is not a directory: {}",
                        dir.display()
                    ));
                }
                match self.supervisor.start(session_id, &dir).await {
                    Ok(id) => ToolOutcome::ok(format!(
                        "started kernel {id}; run code with action=run_cell"
                    )),
                    Err(e) => ToolOutcome::error(e),
                }
            }
            Some("run_cell") => {
                let Some(code) = args.get("code").and_then(Value::as_str) else {
                    return ToolOutcome::error("run_cell requires a `code` string");
                };
                let timeout = Self::resolve_timeout(&args);
                match self.supervisor.run_cell(session_id, code, timeout).await {
                    Ok(res) => {
                        // `cap_output` already prepends a truncation notice when
                        // needed, so `res.output` is display-ready.
                        let mut body = res.output;
                        if res.errored {
                            body.push_str("\n[cell raised an exception]");
                        }
                        ToolOutcome::ok(body)
                    }
                    Err(e) => ToolOutcome::error(e),
                }
            }
            Some("status") => ToolOutcome::ok(self.supervisor.status(session_id).await),
            Some("stop") => match self.supervisor.stop(session_id).await {
                Ok(body) => ToolOutcome::ok(body),
                Err(e) => ToolOutcome::error(e),
            },
            Some(other) => ToolOutcome::error(format!(
                "unknown action '{other}'; expected start|run_cell|status|stop"
            )),
            None => {
                ToolOutcome::error("missing required argument: action (start|run_cell|status|stop)")
            }
        }
    }

    async fn run_streaming(
        &self,
        args: Value,
        root: &Path,
        session_id: &str,
        _sink: Option<crate::OutputSink>,
    ) -> ToolOutcome {
        // Kernel output is delivered whole per cell (bounded by the sentinel), so
        // there's nothing to stream incrementally; session scoping is the reason
        // we override this method rather than relying on the base `run`.
        self.run_with_session(args, root, session_id).await
    }
}
