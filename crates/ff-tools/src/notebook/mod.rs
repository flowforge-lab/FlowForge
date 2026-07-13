//! `notebook_runner` tool — a session-scoped, stateful Python kernel (#859,
//! epic #856). Phase 1: `start` / `run_cell` / `status` / `stop`. Phase 2 adds
//! `run_all` (ipynb file support). The kernel (see [`kernel`]) is a persistent
//! `python3` subprocess with module globals that survive across cells; the
//! supervisor keys kernels by session id and is reaped when the session ends
//! (host wiring), mirroring [`crate::process::ProcessSupervisor`].

mod kernel;
pub(crate) mod parse;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

use crate::registry::{Safety, Tool, ToolOutcome, NO_SESSION};
use kernel::{KernelState, DEFAULT_CELL_TIMEOUT_SECS, MAX_CELL_TIMEOUT_SECS};

/// Max live kernels per session (Phase 3, #856). Bounds resource use while still
/// letting a workflow keep a couple of independent namespaces side by side.
const MAX_KERNELS_PER_SESSION: usize = 3;

/// Lifecycle of a single kernel as surfaced to the desktop status panel (#871).
/// `"dead"` reflects a kernel that died on its own (EOF on its pipe); a user
/// Stop reaps the session instead, so it never produces `Dead`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum KernelLiveState {
    Running,
    Dead,
}

/// Single-kernel snapshot of a session's `notebook_runner` state for the desktop
/// status panel (#871). This is the canonical ts-rs source for the shape the FE
/// previously carried as a hand-written stub.
///
/// A session may hold up to [`MAX_KERNELS_PER_SESSION`] kernels (Phase 3), but
/// the panel's contract is single-kernel: the typed fields describe a
/// representative kernel (a live one if any, else the first by id), while
/// [`raw`](Self::raw) carries the full canonical multi-line status text so no
/// kernel is hidden when several are live.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct NotebookKernelState {
    pub session_id: String,
    /// False when the session has no kernel; the remaining fields are then
    /// null/zero. True means `state`, `kernel_id`, and `pid` describe the
    /// representative kernel.
    pub has_kernel: bool,
    /// The representative kernel's lifecycle; null when there is no kernel.
    pub state: Option<KernelLiveState>,
    /// The representative kernel's id (e.g. `kernel-abcd1234`); null when none.
    pub kernel_id: Option<String>,
    /// The representative kernel's process id; null when none or unavailable.
    pub pid: Option<u32>,
    /// Cells executed by the representative kernel so far; zero when none.
    #[ts(type = "number")]
    pub execution_count: u64,
    /// The full canonical status text (`kernel <id> — <state>; pid=…; cells
    /// executed=…`, one line per kernel). Empty when there is no kernel.
    pub raw: String,
    /// Every kernel in the session, structured (Phase 3 multi-kernel switcher,
    /// #871 FE-2 / #923). Sorted by kernel id, so the FE renders a stable tab
    /// order. `None` when the session has no kernel; otherwise one entry per
    /// kernel (the FE shows tabs only when there's more than one). The
    /// representative fields above still describe one of these (a live kernel if
    /// any) for the single-kernel panel contract; `kernels` is the superset for
    /// the switcher. Optional on the wire so a consumer that only needs the
    /// representative can ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kernels: Option<Vec<KernelInfo>>,
}

/// One kernel's structured state within a session — the per-tab data for the
/// multi-kernel switcher (#871 FE-2 / #923). Mirrors the per-kernel fields the
/// representative exposes on [`NotebookKernelState`], but for every kernel.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct KernelInfo {
    /// The kernel's id (e.g. `kernel-abcd1234`).
    pub kernel_id: String,
    /// The kernel's lifecycle (`running` / `dead`).
    pub state: KernelLiveState,
    /// The kernel's process id; null when unavailable.
    pub pid: Option<u32>,
    /// Cells this kernel has executed so far.
    #[ts(type = "number")]
    pub execution_count: u64,
}

impl KernelInfo {
    /// Project a live [`KernelState`] onto the FE-facing structured shape.
    fn of(k: &KernelState) -> Self {
        Self {
            kernel_id: k.kernel_id.clone(),
            state: if k.dead {
                KernelLiveState::Dead
            } else {
                KernelLiveState::Running
            },
            pid: k.pid(),
            execution_count: k.execution_count,
        }
    }
}

/// Per-cell result from a `run_all` invocation.
#[derive(Debug, Clone)]
struct CellRunResult {
    cell_index: usize,
    output: String,
    errored: bool,
    truncated: bool,
}

/// Manages persistent Python kernels, keyed by session id then kernel id. Behind
/// a `Mutex` so the single-threaded kernel stdin/stdout is never interleaved
/// across cells. Phase 3 allows up to [`MAX_KERNELS_PER_SESSION`] per session.
#[derive(Default)]
pub struct KernelSupervisor {
    kernels: Mutex<HashMap<String, HashMap<String, KernelState>>>,
}

impl KernelSupervisor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve the target kernel id within a session's map. Rules:
    /// - explicit `wanted` → that id (error if unknown);
    /// - omitted + exactly one → that one (back-compat with Phase 1/2 callers);
    /// - omitted + none → error "start first";
    /// - omitted + many → error listing the ids so the caller can disambiguate.
    fn resolve_id(
        session: &HashMap<String, KernelState>,
        wanted: Option<&str>,
    ) -> Result<String, String> {
        if let Some(id) = wanted {
            return if session.contains_key(id) {
                Ok(id.to_string())
            } else {
                Err(format!("no kernel `{id}` in this session"))
            };
        }
        match session.len() {
            0 => Err("no kernel for this session; call action=start first".into()),
            1 => Ok(session.keys().next().unwrap().clone()),
            _ => {
                let mut ids: Vec<&str> = session.keys().map(String::as_str).collect();
                ids.sort_unstable();
                Err(format!(
                    "multiple kernels in this session; specify `kernel` (ids: {})",
                    ids.join(", ")
                ))
            }
        }
    }

    /// Start a new kernel for `session_id`, enforcing the per-session cap. Dead
    /// kernels are pruned first so they don't count toward the cap. Returns the
    /// new kernel id.
    async fn start(&self, session_id: &str, dir: &Path) -> Result<String, String> {
        let mut kernels = self.kernels.lock().await;
        let session = kernels.entry(session_id.to_string()).or_default();
        // Reap any dead kernels so they free a slot.
        session.retain(|_, k| !k.dead);
        if session.len() >= MAX_KERNELS_PER_SESSION {
            return Err(format!(
                "kernel cap ({MAX_KERNELS_PER_SESSION}) reached for this session; stop one first"
            ));
        }
        let kernel = KernelState::spawn(dir).await?;
        let id = kernel.kernel_id.clone();
        session.insert(id.clone(), kernel);
        Ok(id)
    }

    async fn run_cell(
        &self,
        session_id: &str,
        kernel_id: Option<&str>,
        code: &str,
        timeout_secs: u64,
    ) -> Result<kernel::CellResult, String> {
        let mut kernels = self.kernels.lock().await;
        let session = kernels
            .get_mut(session_id)
            .ok_or("no kernel for this session; call action=start first")?;
        let id = Self::resolve_id(session, kernel_id)?;
        session
            .get_mut(&id)
            .unwrap()
            .run_cell(code, timeout_secs)
            .await
    }

    /// Restart a kernel: stop the existing one and spawn a fresh replacement,
    /// preserving the session mapping (a new kernel id is assigned). The old
    /// namespace is gone by design — that is the point of a restart.
    ///
    /// `pub` so the desktop `notebook_restart` command can drive it directly
    /// (mirrors [`snapshot`](Self::snapshot)); also used by the tool dispatch.
    pub async fn restart(
        &self,
        session_id: &str,
        kernel_id: Option<&str>,
        dir: &Path,
    ) -> Result<String, String> {
        let mut kernels = self.kernels.lock().await;
        let session = kernels.entry(session_id.to_string()).or_default();
        // If a specific/only kernel exists, tear it down; otherwise just start.
        if let Ok(id) = Self::resolve_id(session, kernel_id) {
            if let Some(mut old) = session.remove(&id) {
                old.stop().await;
            }
        } else if kernel_id.is_some() {
            // Caller named a kernel that doesn't exist.
            return Err(Self::resolve_id(session, kernel_id).unwrap_err());
        }
        let kernel = KernelState::spawn(dir).await?;
        let id = kernel.kernel_id.clone();
        session.insert(id.clone(), kernel);
        Ok(id)
    }

    /// Inspect the variables in a kernel's namespace. Returns the JSON array of
    /// `{name,type,repr}` the kernel emitted.
    async fn inspect(
        &self,
        session_id: &str,
        kernel_id: Option<&str>,
        timeout_secs: u64,
    ) -> Result<String, String> {
        let mut kernels = self.kernels.lock().await;
        let session = kernels
            .get_mut(session_id)
            .ok_or("no kernel for this session; call action=start first")?;
        let id = Self::resolve_id(session, kernel_id)?;
        session.get_mut(&id).unwrap().inspect(timeout_secs).await
    }

    /// Render the canonical multi-line status text for a session's kernels
    /// (one line per kernel, sorted by id). Assumes a non-empty session; the
    /// "no kernel" case is handled by callers.
    fn render_status(session: &HashMap<String, KernelState>) -> String {
        let mut ids: Vec<&String> = session.keys().collect();
        ids.sort();
        let mut lines = Vec::with_capacity(ids.len());
        for id in ids {
            let k = &session[id];
            let state = if k.dead { "dead" } else { "running" };
            lines.push(format!(
                "kernel {} — {state}; pid={}; cells executed={}",
                k.kernel_id,
                k.pid().map(|p| p.to_string()).unwrap_or_else(|| "?".into()),
                k.execution_count
            ));
        }
        lines.join("\n")
    }

    async fn status(&self, session_id: &str) -> String {
        let kernels = self.kernels.lock().await;
        match kernels.get(session_id) {
            Some(session) if !session.is_empty() => Self::render_status(session),
            _ => "no kernel running for this session".to_string(),
        }
    }

    /// Structured snapshot of a session's kernel state for the desktop status
    /// panel (#871). Projects the (possibly multi-kernel) session onto the
    /// single-kernel [`NotebookKernelState`] contract: the typed fields describe
    /// a representative kernel — a live one if any, else the first by id — while
    /// `raw` lists every kernel. No kernel → `has_kernel: false`.
    pub async fn snapshot(&self, session_id: &str) -> NotebookKernelState {
        let kernels = self.kernels.lock().await;
        match kernels.get(session_id).filter(|s| !s.is_empty()) {
            None => NotebookKernelState {
                session_id: session_id.to_string(),
                has_kernel: false,
                state: None,
                kernel_id: None,
                pid: None,
                execution_count: 0,
                raw: String::new(),
                kernels: None,
            },
            Some(session) => {
                let mut entries: Vec<&KernelState> = session.values().collect();
                entries.sort_by(|a, b| a.kernel_id.cmp(&b.kernel_id));
                // Prefer a live kernel as the panel's representative so its
                // `running` poll keeps ticking even if a dead kernel sorts ahead.
                let rep = entries
                    .iter()
                    .copied()
                    .find(|k| !k.dead)
                    .unwrap_or(entries[0]);
                // Structured per-kernel list (switcher, #923), same sorted order
                // as `raw`.
                let kernels = Some(entries.iter().map(|k| KernelInfo::of(k)).collect());
                NotebookKernelState {
                    session_id: session_id.to_string(),
                    has_kernel: true,
                    state: Some(if rep.dead {
                        KernelLiveState::Dead
                    } else {
                        KernelLiveState::Running
                    }),
                    kernel_id: Some(rep.kernel_id.clone()),
                    pid: rep.pid(),
                    execution_count: rep.execution_count,
                    raw: Self::render_status(session),
                    kernels,
                }
            }
        }
    }

    /// Stop one kernel (resolved from `kernel_id`) in a session. `pub` so the
    /// desktop `notebook_stop` command can target a single kernel (the switcher's
    /// per-tab Stop, #871 FE-2 / #923); session-wide teardown still goes through
    /// [`reap_session`](Self::reap_session).
    pub async fn stop(&self, session_id: &str, kernel_id: Option<&str>) -> Result<String, String> {
        let mut kernels = self.kernels.lock().await;
        let session = kernels
            .get_mut(session_id)
            .ok_or("no kernel running for this session")?;
        let id = Self::resolve_id(session, kernel_id)?;
        let mut k = session.remove(&id).unwrap();
        let kid = k.kernel_id.clone();
        k.stop().await;
        if session.is_empty() {
            kernels.remove(session_id);
        }
        Ok(format!("stopped kernel {kid}"))
    }

    /// Run all cells from a parsed notebook sequentially in the resolved kernel.
    /// Returns per-cell results. If `stop_on_error` is true, execution halts at
    /// the first failure.
    async fn run_all(
        &self,
        session_id: &str,
        kernel_id: Option<&str>,
        cells: &[parse::NotebookCell],
        timeout_secs: u64,
        stop_on_error: bool,
    ) -> Result<Vec<CellRunResult>, String> {
        let mut results = Vec::with_capacity(cells.len());
        for cell in cells {
            let res = self
                .run_cell(session_id, kernel_id, &cell.source, timeout_secs)
                .await?;
            let stopped = stop_on_error && res.errored;
            results.push(CellRunResult {
                cell_index: cell.index,
                output: res.output,
                errored: res.errored,
                truncated: res.truncated,
            });
            if stopped {
                break;
            }
        }
        Ok(results)
    }

    /// Kill and drop every kernel for `session_id` (host calls this on session
    /// end so a long-lived kernel never leaks). Returns how many were reaped.
    pub async fn reap_session(&self, session_id: &str) -> usize {
        let mut kernels = self.kernels.lock().await;
        match kernels.remove(session_id) {
            Some(session) => {
                let mut n = 0;
                for (_, mut k) in session {
                    k.stop().await;
                    n += 1;
                }
                n
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

    /// The optional `kernel` arg (a kernel id) if the caller supplied a non-empty
    /// string; `None` selects the session's sole kernel (see `resolve_id`).
    fn resolve_kernel_id(args: &Value) -> Option<String> {
        args.get("kernel")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }
}

#[async_trait]
impl Tool for NotebookTool {
    // reaches_network: keeps the fail-safe `true` default (RFC 0013) — it
    // executes arbitrary Python cells — can `import urllib` / open sockets.
    fn name(&self) -> &str {
        "notebook_runner"
    }

    fn description(&self) -> &str {
        "Run Python cell-at-a-time in a persistent kernel whose variables PERSIST \
         across calls — unlike `python` (fresh interpreter each call). Use it to \
         build up state incrementally: define something in one cell, use it in the \
         next. The kernel is scoped to this session and shares its `.venv`. \
         Actions: `start` (spawn a kernel, up to 3 per session; returns its id), \
         `run_cell` (run inline `code`, returns its stdout/stderr), `run_all` \
         (execute all code cells from a .ipynb file sequentially), `inspect` \
         (dump the variables in scope), `restart` (kill + respawn a fresh kernel, \
         keeping the session), `status` (kernel state + cells run), `stop` (kill \
         a kernel). With multiple kernels in a session, pass `kernel` (the id) to \
         pick one. Matplotlib figures are saved and their paths returned. Each \
         cell has a per-call timeout; a hung cell is interrupted then killed."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start", "run_cell", "run_all", "inspect", "restart", "status", "stop"],
                    "description": "What to do."
                },
                "code": {
                    "type": "string",
                    "description": "For `run_cell`: the Python source to execute in the kernel."
                },
                "notebook": {
                    "type": "string",
                    "description": "For `run_all`: path to a .ipynb file (relative to workspace root or absolute)."
                },
                "stop_on_error": {
                    "type": "boolean",
                    "description": "For `run_all`: stop on first cell error (default true)."
                },
                "kernel": {
                    "type": "string",
                    "description": "Kernel id (from `start`) to target when the session has more than one kernel. Omit when there is exactly one."
                },
                "working_dir": {
                    "type": "string",
                    "description": "For `start`/`restart`: directory to run in (venv discovery + cwd), relative to the workspace root or absolute. Defaults to root."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "For `run_cell`/`run_all`/`inspect`: per-cell wall-clock budget (default 60, max 600). A cell that overruns is interrupted, then killed."
                }
            },
            "required": ["action"]
        })
    }

    fn safety(&self, args: &Value) -> Safety {
        match args.get("action").and_then(Value::as_str) {
            // Reading kernel state / variables mutates nothing → usable in Plan.
            Some("status") | Some("inspect") => Safety::ReadOnly,
            // Stopping the kernel is a benign, reversible teardown.
            Some("stop") => Safety::Write,
            // start / restart / run_cell / run_all spawn and execute arbitrary Python.
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
        let kernel_id = Self::resolve_kernel_id(&args);
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
                match self
                    .supervisor
                    .run_cell(session_id, kernel_id.as_deref(), code, timeout)
                    .await
                {
                    Ok(res) => {
                        // `cap_output` already prepends a truncation notice when
                        // needed, so `res.output` is display-ready.
                        let mut body = res.output;
                        if res.errored {
                            body.push_str("\n[cell raised an exception]");
                        }
                        // Append the FF_NB_META trailer if the cell produced images
                        // (the FE strips it and renders the figures — see #879).
                        if let Some(meta) = meta_trailer(&res.images, None) {
                            body.push_str(&meta);
                        }
                        ToolOutcome::ok(body)
                    }
                    Err(e) => ToolOutcome::error(e),
                }
            }
            Some("run_all") => {
                let Some(notebook_path) = args.get("notebook").and_then(Value::as_str) else {
                    return ToolOutcome::error(
                        "run_all requires a `notebook` path to a .ipynb file",
                    );
                };
                let path = if Path::new(notebook_path).is_absolute() {
                    PathBuf::from(notebook_path)
                } else {
                    root.join(notebook_path)
                };
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(e) => {
                        return ToolOutcome::error(format!(
                            "cannot read notebook {}: {e}",
                            path.display()
                        ));
                    }
                };
                let cells = match parse::parse_notebook(&content) {
                    Ok(c) => c,
                    Err(e) => return ToolOutcome::error(format!("failed to parse notebook: {e}")),
                };
                if cells.is_empty() {
                    return ToolOutcome::ok("notebook contains no code cells".to_string());
                }
                let timeout = Self::resolve_timeout(&args);
                let stop_on_error = args
                    .get("stop_on_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                match self
                    .supervisor
                    .run_all(session_id, kernel_id.as_deref(), &cells, timeout, stop_on_error)
                    .await
                {
                    Ok(results) => ToolOutcome::ok(format_run_all(&results, &cells, notebook_path)),
                    Err(e) => ToolOutcome::error(e),
                }
            }
            Some("inspect") => {
                let timeout = Self::resolve_timeout(&args);
                match self
                    .supervisor
                    .inspect(session_id, kernel_id.as_deref(), timeout)
                    .await
                {
                    Ok(vars_json) => {
                        let mut body = format_variables(&vars_json);
                        if let Some(meta) = meta_trailer(&[], Some(&vars_json)) {
                            body.push_str(&meta);
                        }
                        ToolOutcome::ok(body)
                    }
                    Err(e) => ToolOutcome::error(e),
                }
            }
            Some("restart") => {
                let dir = Self::resolve_dir(&args, root);
                if !dir.is_dir() {
                    return ToolOutcome::error(format!(
                        "working_dir does not exist or is not a directory: {}",
                        dir.display()
                    ));
                }
                match self
                    .supervisor
                    .restart(session_id, kernel_id.as_deref(), &dir)
                    .await
                {
                    Ok(id) => {
                        ToolOutcome::ok(format!("restarted; fresh kernel {id} (previous state cleared)"))
                    }
                    Err(e) => ToolOutcome::error(e),
                }
            }
            Some("status") => ToolOutcome::ok(self.supervisor.status(session_id).await),
            Some("stop") => match self.supervisor.stop(session_id, kernel_id.as_deref()).await {
                Ok(body) => ToolOutcome::ok(body),
                Err(e) => ToolOutcome::error(e),
            },
            Some(other) => ToolOutcome::error(format!(
                "unknown action '{other}'; expected start|run_cell|run_all|inspect|restart|status|stop"
            )),
            None => ToolOutcome::error(
                "missing required argument: action (start|run_cell|run_all|inspect|restart|status|stop)",
            ),
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

/// Format the results of a `run_all` into a compact, human-readable summary.
fn format_run_all(results: &[CellRunResult], cells: &[parse::NotebookCell], path: &str) -> String {
    let total_code = cells.len();
    let ran = results.len();
    let mut out = format!("Ran {ran}/{total_code} code cells from {path}\n");

    for r in results {
        let status = if r.errored { "\u{2717}" } else { "\u{2713}" };
        // Show a brief snippet of output (first line) for context.
        let snippet = r
            .output
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect::<String>();
        if snippet.is_empty() {
            out.push_str(&format!("\n[cell {}] {status}", r.cell_index));
        } else {
            out.push_str(&format!("\n[cell {}] {status}  {snippet}", r.cell_index));
        }
        if r.truncated {
            out.push_str(" [truncated]");
        }
    }

    // If we stopped early, note it.
    if ran < total_code {
        if let Some(last) = results.last() {
            if last.errored {
                out.push_str(&format!(
                    "\n\n[stopped on error at cell {}]",
                    last.cell_index
                ));
            }
        }
    }
    out
}

/// Delimiters for the machine-readable Phase 3 trailer. The FE (#879) strips the
/// block between these lines out of the visible output and JSON-parses the body
/// to render images / variables. Emitted only when there is something to carry,
/// so plain cells stay byte-identical to Phase 1/2.
const META_OPEN: &str = "<<<FF_NB_META";
const META_CLOSE: &str = "FF_NB_META";

/// Build the `FF_NB_META` trailer carrying image paths and/or a variables dump,
/// or `None` when both are empty. `vars_json` is a pre-serialized JSON array (as
/// the kernel emitted it) spliced in verbatim.
fn meta_trailer(images: &[String], vars_json: Option<&str>) -> Option<String> {
    let has_images = !images.is_empty();
    let has_vars = vars_json.is_some_and(|v| v.trim() != "[]" && !v.trim().is_empty());
    if !has_images && !has_vars {
        return None;
    }
    let images_arr = serde_json::to_string(
        &images
            .iter()
            .map(|p| serde_json::json!({ "path": p, "mediaType": "image/png" }))
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_string());
    let vars_arr = if has_vars {
        vars_json.unwrap().trim().to_string()
    } else {
        "[]".to_string()
    };
    Some(format!(
        "\n{META_OPEN}\n{{\"images\":{images_arr},\"variables\":{vars_arr}}}\n{META_CLOSE}\n"
    ))
}

/// Render the `inspect` variables JSON into a compact human-readable table for
/// the text result (the FE gets the structured data from the meta trailer).
fn format_variables(vars_json: &str) -> String {
    let parsed: Result<Vec<serde_json::Value>, _> = serde_json::from_str(vars_json);
    let Ok(vars) = parsed else {
        return "variables: (unparseable)".to_string();
    };
    if vars.is_empty() {
        return "no user variables in scope".to_string();
    }
    let mut out = format!("{} variable(s) in scope:", vars.len());
    for v in &vars {
        let name = v.get("name").and_then(Value::as_str).unwrap_or("?");
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("?");
        let repr = v.get("repr").and_then(Value::as_str).unwrap_or("");
        out.push_str(&format!("\n  {name}: {ty} = {repr}"));
    }
    out
}
