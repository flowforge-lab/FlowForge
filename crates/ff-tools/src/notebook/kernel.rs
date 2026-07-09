//! A persistent Python kernel for cell-at-a-time execution (#859, epic #856).
//!
//! Unlike the stateless [`crate::python::PythonTool`] (one snippet, fresh
//! interpreter), a kernel is a long-lived `python3` subprocess whose module
//! globals persist across cells — `x = 1` in one cell is visible to `print(x)`
//! in the next. That is the whole point: cell-at-a-time iteration.
//!
//! ## Sentinel framing
//! We drive a self-written REPL loop (NOT `python3 -i`, whose prompts/echo are
//! fragile) fed on stdin. Each cell is length-prefixed (`<N>\n` then N bytes of
//! source) so multi-line/blank source is unambiguous. After running a cell the
//! driver prints a sentinel line `__FF_CELL_END_<nonce>__<ok|error>` and flushes;
//! the Rust side reads stdout up to that exact line. The nonce is random per
//! kernel, so a cell that itself prints a sentinel-shaped string cannot be
//! mistaken for the real delimiter.
//!
//! ## Lifecycle & signals
//! Spawned in its own process group (unix) so a per-cell timeout can SIGINT the
//! whole group (raises `KeyboardInterrupt` in a pure-Python busy loop), then
//! SIGKILL after a grace window if the sentinel still does not arrive. Mirrors
//! [`crate::process::ProcessSupervisor`]'s cross-platform kill patterns.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::timeout;

/// Per-cell wall-clock default when the caller omits `timeout_secs`.
pub(crate) const DEFAULT_CELL_TIMEOUT_SECS: u64 = 60;
/// Hard ceiling on a caller-supplied `timeout_secs`.
pub(crate) const MAX_CELL_TIMEOUT_SECS: u64 = 600;
/// Grace period between SIGINT and SIGKILL when a cell overruns.
const INTERRUPT_GRACE: Duration = Duration::from_secs(2);
/// Max bytes of output kept per cell; the tail is dropped past this.
const MAX_CELL_OUTPUT: usize = 16 * 1024;

/// Fixed sentinel affixes. The random nonce sits between them, so cell output
/// containing the literal prefix (without this kernel's nonce) never collides.
const SENTINEL_PREFIX: &str = "__FF_CELL_END_";
const SENTINEL_SUFFIX: &str = "__";

/// Marker line prefix the driver prints (one per saved figure) when a cell
/// produced matplotlib output. The absolute PNG path follows. Stripped from the
/// cell's visible output and surfaced as [`CellResult::images`] (Phase 3, #856).
const IMAGE_MARKER: &str = "__FF_IMAGE__";
/// Marker line prefix the `inspect` snippet prints, followed by a JSON array of
/// `{name,type,repr}`. Stripped from visible output → [`CellResult::vars_json`].
const VARS_MARKER: &str = "__FF_VARS__";

/// Max chars kept of each variable's `repr` in the `inspect` dump, so a huge
/// object (a big DataFrame) can't blow the result size.
const MAX_REPR_LEN: usize = 200;

/// The Python driver loop, formatted once per kernel with its nonce. Reads
/// length-prefixed cells from stdin, `exec`s each into a persistent namespace,
/// and emits the nonce sentinel (`ok`/`error`) after each. After a successful
/// cell, if matplotlib is loaded, open figures are saved under `img_dir_literal`
/// (a JSON/Python string literal, already quoted) and announced with
/// [`IMAGE_MARKER`] lines. Runs unbuffered.
fn driver_source(nonce: &str, img_dir_literal: &str) -> String {
    // {nonce}/{img_dir}/{image_marker} are substituted; everything else is
    // literal Python. Keep it small and dependency-free (stdlib only; matplotlib
    // is used only if the cell itself imported it).
    format!(
        r#"
import sys, traceback, os
_ns = {{"__name__": "__cell__"}}
# Funnel stderr into stdout so warnings / prints-to-stderr surface in the drained
# stream (and can't deadlock on an undrained stderr pipe).
sys.stderr = sys.stdout
_end = "{prefix}{nonce}{suffix}"
_img_dir = {img_dir}
_img_seq = 0
def _emit(status):
    sys.stdout.write("\n" + _end + status + "\n")
    sys.stdout.flush()
def _save_figures():
    # Only if the cell imported matplotlib; never import it ourselves.
    global _img_seq
    _mpl = sys.modules.get("matplotlib")
    if _mpl is None:
        return
    try:
        import matplotlib.pyplot as _plt
        _nums = _plt.get_fignums()
        if not _nums:
            return
        os.makedirs(_img_dir, exist_ok=True)
        for _num in _nums:
            _fig = _plt.figure(_num)
            _p = os.path.join(_img_dir, "fig-" + str(_img_seq) + ".png")
            _img_seq += 1
            _fig.savefig(_p)
            sys.stdout.write("\n{image_marker}" + _p + "\n")
        _plt.close("all")
    except Exception:
        # Image saving is best-effort; a failure never fails the cell.
        pass
while True:
    # Read the whole frame from the binary stream so the length is a byte
    # count that matches what Rust wrote (#880). `sys.stdin.read(n)` reads
    # characters, not bytes, so any non-ASCII cell source (em-dash, non-Latin
    # identifier, etc.) would under-read and desync the stream.
    _hdr = sys.stdin.buffer.readline()
    if not _hdr:
        break
    _hdr = _hdr.strip()
    if not _hdr:
        continue
    try:
        _n = int(_hdr)
    except ValueError:
        _emit("error")
        continue
    _src = sys.stdin.buffer.read(_n).decode("utf-8")
    try:
        exec(compile(_src, "<cell>", "exec"), _ns)
        _save_figures()
        sys.stdout.flush()
        sys.stderr.flush()
        _emit("ok")
    except BaseException:
        traceback.print_exc(file=sys.stdout)
        sys.stdout.flush()
        _emit("error")
"#,
        prefix = SENTINEL_PREFIX,
        nonce = nonce,
        suffix = SENTINEL_SUFFIX,
        img_dir = img_dir_literal,
        image_marker = IMAGE_MARKER,
    )
}
/// Python snippet the `inspect` action feeds to the kernel. Walks the persistent
/// namespace, skips underscore-private names / modules / callables / classes,
/// and prints a single [`VARS_MARKER`] line with a JSON array of
/// `{name,type,repr}` (each repr truncated). Runs in `_ns` like any cell, but is
/// side-effect-free w.r.t. user data (it only reads), so `inspect` is ReadOnly.
fn inspect_snippet() -> String {
    format!(
        r#"
import json as _json, sys as _sys
# This snippet is exec'd with the persistent namespace as its globals, so
# globals() IS that namespace; introspect it directly.
_g = dict(globals())
_vars = []
for _k in sorted(_g.keys()):
    if _k.startswith("_"):
        continue
    _v = _g[_k]
    _t = type(_v).__name__
    if _t in ("module", "function", "builtin_function_or_method", "type", "method"):
        continue
    try:
        _r = repr(_v)
    except Exception:
        _r = "<unrepresentable>"
    if len(_r) > {max_repr}:
        _r = _r[:{max_repr}] + "..."
    _vars.append({{"name": _k, "type": _t, "repr": _r}})
_sys.stdout.write("\n{vars_marker}" + _json.dumps(_vars) + "\n")
"#,
        max_repr = MAX_REPR_LEN,
        vars_marker = VARS_MARKER,
    )
}

/// The exact sentinel line this kernel's driver emits for a given status.
pub(super) fn sentinel_line(nonce: &str, status: &str) -> String {
    format!("{SENTINEL_PREFIX}{nonce}{SENTINEL_SUFFIX}{status}")
}

/// Outcome of running one cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellResult {
    pub output: String,
    /// `true` when the cell raised (the driver reported `error`).
    pub errored: bool,
    pub truncated: bool,
    /// Absolute paths of any figures the cell saved (matplotlib). Empty for a
    /// cell that produced no images. The paths live under the kernel's temp dir
    /// and are cleaned when the kernel stops (Phase 3, #856).
    pub images: Vec<String>,
    /// JSON array of `{name,type,repr}` when this cell was an `inspect` run;
    /// `None` for an ordinary cell.
    pub vars_json: Option<String>,
}

/// Strip [`IMAGE_MARKER`] / [`VARS_MARKER`] lines out of a drained cell body,
/// returning `(clean_output, image_paths, vars_json)`. A marker is only honoured
/// as a whole line (prefix at the start of a line), so ordinary text that merely
/// contains the marker substring mid-line is left untouched.
pub(super) fn extract_markers(buf: &str) -> (String, Vec<String>, Option<String>) {
    let mut clean = String::with_capacity(buf.len());
    let mut images = Vec::new();
    let mut vars_json = None;
    for line in buf.split_inclusive('\n') {
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        if let Some(path) = trimmed.strip_prefix(IMAGE_MARKER) {
            images.push(path.to_string());
        } else if let Some(json) = trimmed.strip_prefix(VARS_MARKER) {
            // Last one wins (there is only ever one per inspect run).
            vars_json = Some(json.to_string());
        } else {
            clean.push_str(line);
        }
    }
    (clean.trim_end_matches('\n').to_string(), images, vars_json)
}

/// Split accumulated stdout at the first sentinel line for `nonce`. Returns the
/// cell output (everything before the sentinel) and whether the status was
/// `error`, or `None` if no sentinel is present yet. A sentinel-shaped line with
/// a different nonce stays in the output (not a delimiter) — the collision guard.
pub(super) fn parse_sentinel(buf: &str, nonce: &str) -> Option<(String, bool)> {
    let ok = sentinel_line(nonce, "ok");
    let err = sentinel_line(nonce, "error");
    let mut offset = 0usize;
    for line in buf.split_inclusive('\n') {
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        let errored = if trimmed == ok {
            Some(false)
        } else if trimmed == err {
            Some(true)
        } else {
            None
        };
        if let Some(errored) = errored {
            // Output is everything before this sentinel line.
            let output = buf[..offset].trim_end_matches('\n').to_string();
            return Some((output, errored));
        }
        offset += line.len();
    }
    None
}

/// A running Python kernel: the child process, its stdin, a random nonce, and a
/// monotonic execution counter. Owned by [`super::KernelSupervisor`] behind its
/// mutex; one per session in Phase 1.
pub struct KernelState {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    nonce: String,
    pid: Option<u32>,
    /// Cells run so far (incremented on each `run_cell` attempt).
    pub execution_count: u64,
    /// Set once the kernel is known unusable (died / killed after timeout).
    pub dead: bool,
    pub kernel_id: String,
    /// Temp directory where this kernel's saved figures land; removed on stop.
    img_dir: PathBuf,
}

impl KernelState {
    /// Spawn a kernel rooted at `dir`, reusing the same interpreter discovery as
    /// the stateless python tool (activated venv > project `.venv` > PATH).
    pub async fn spawn(dir: &Path) -> Result<Self, String> {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let kernel_id = format!("kernel-{}", &nonce[..8]);
        let python = crate::python::PythonTool::interpreter(dir);

        // Per-kernel scratch dir for saved figures (matplotlib). Created lazily by
        // the driver only if a cell actually saves an image; cleaned on stop.
        let img_dir = std::env::temp_dir()
            .join("flowforge-notebook")
            .join(&kernel_id);
        // JSON-encode the path so it's a safe Python string literal (handles
        // quotes/backslashes/unicode identically in JSON and Python).
        let img_dir_literal = serde_json::to_string(&img_dir.to_string_lossy())
            .unwrap_or_else(|_| "\"\"".to_string());

        let mut cmd = Command::new(&python);
        cmd.arg("-u") // unbuffered stdio so sentinels arrive promptly
            .arg("-c")
            .arg(driver_source(&nonce, &img_dir_literal))
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Own process group so a timeout can signal the whole group (unix).
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to start python kernel ({}): {e}", python.display()))?;
        let pid = child.id();
        let stdin = child.stdin.take().ok_or("kernel stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("kernel stdout unavailable")?;

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            nonce,
            pid,
            execution_count: 0,
            dead: false,
            kernel_id,
            img_dir,
        })
    }

    /// Run one cell: frame it to the driver, then read stdout up to this kernel's
    /// sentinel. On timeout, SIGINT the group, grace, then SIGKILL + mark dead.
    pub async fn run_cell(&mut self, code: &str, timeout_secs: u64) -> Result<CellResult, String> {
        if self.dead {
            return Err("kernel is not running (dead); start a new one".into());
        }
        self.execution_count += 1;

        // Length-prefixed frame: "<byte-len>\n<source bytes>".
        let bytes = code.as_bytes();
        let frame = format!("{}\n", bytes.len());
        if self.stdin.write_all(frame.as_bytes()).await.is_err()
            || self.stdin.write_all(bytes).await.is_err()
            || self.stdin.flush().await.is_err()
        {
            self.dead = true;
            return Err("kernel stdin closed (kernel died)".into());
        }

        match timeout(Duration::from_secs(timeout_secs), self.drain_to_sentinel()).await {
            Ok(Ok((output, errored))) => Ok(build_cell_result(output, errored)),
            Ok(Err(e)) => {
                self.dead = true;
                Err(e)
            }
            Err(_) => {
                // Overran: interrupt, grace, then kill.
                self.interrupt();
                tokio::time::sleep(INTERRUPT_GRACE).await;
                if let Ok(Ok((output, _))) =
                    timeout(Duration::from_millis(200), self.drain_to_sentinel()).await
                {
                    // KeyboardInterrupt landed and the driver recovered.
                    return Ok(build_cell_result(output, true));
                }
                self.kill();
                self.dead = true;
                Err(format!(
                    "cell exceeded timeout_secs={timeout_secs}; kernel interrupted and killed"
                ))
            }
        }
    }

    /// Run the introspection snippet (`inspect` action) and return the JSON array
    /// of `{name,type,repr}` the kernel emitted, or an error if it didn't. The
    /// snippet only reads the namespace, so `inspect` stays ReadOnly.
    pub async fn inspect(&mut self, timeout_secs: u64) -> Result<String, String> {
        // Introspection is not a user cell; undo run_cell's counter bump so
        // `execution_count` keeps meaning "cells the user ran".
        let res = self.run_cell(&inspect_snippet(), timeout_secs).await?;
        self.execution_count = self.execution_count.saturating_sub(1);
        if res.errored {
            return Err(format!("variable inspection failed:\n{}", res.output));
        }
        // The snippet prints a VARS_MARKER line, which `build_cell_result` peels
        // off into `vars_json`. Absent (shouldn't happen on success) → empty set.
        Ok(res.vars_json.unwrap_or_else(|| "[]".to_string()))
    }

    /// Read stdout lines, accumulating, until this kernel's sentinel appears.
    async fn drain_to_sentinel(&mut self) -> Result<(String, bool), String> {
        let mut buf = String::new();
        loop {
            let mut line = String::new();
            let n = self
                .stdout
                .read_line(&mut line)
                .await
                .map_err(|e| format!("kernel read error: {e}"))?;
            if n == 0 {
                return Err("kernel closed stdout (kernel died)".into());
            }
            buf.push_str(&line);
            if let Some(res) = parse_sentinel(&buf, &self.nonce) {
                return Ok(res);
            }
        }
    }

    /// Send SIGINT to the kernel's process group so a pure-Python busy loop gets a
    /// `KeyboardInterrupt`. Best-effort and no-op on unsupported platforms.
    fn interrupt(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // Negative pid targets the whole process group (we spawned with
            // `process_group(0)`), matching ProcessSupervisor's group signalling.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGINT);
            }
        }
        #[cfg(not(unix))]
        {
            // Windows: no cheap per-group SIGINT; the timeout path falls through
            // to `kill`, which uses the kill-on-drop child handle.
            let _ = self.pid;
        }
    }

    /// Force-kill the kernel. On unix, SIGKILL the group; elsewhere rely on the
    /// child handle (kill_on_drop) via `start_kill`.
    fn kill(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
    }

    /// Stop the kernel explicitly (the `stop` action). Idempotent.
    pub async fn stop(&mut self) {
        self.kill();
        self.dead = true;
        // Reap so we don't leave a zombie; ignore the result.
        let _ = self.child.wait().await;
        // Best-effort cleanup of this kernel's saved figures.
        let _ = std::fs::remove_dir_all(&self.img_dir);
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }
}

/// Build a [`CellResult`] from a drained cell body: strip image/vars markers,
/// then cap the remaining visible output. Shared by the normal and
/// interrupt-recovery paths of [`KernelState::run_cell`].
fn build_cell_result(raw: String, errored: bool) -> CellResult {
    let (clean, images, vars_json) = extract_markers(&raw);
    let (output, truncated) = cap_output(clean);
    CellResult {
        output,
        errored,
        truncated,
        images,
        vars_json,
    }
}

/// Cap a cell's output to [`MAX_CELL_OUTPUT`] bytes, keeping the tail (most
/// recent output) and prepending a truncation notice. Returns `(text, truncated)`.
fn cap_output(mut output: String) -> (String, bool) {
    if output.len() <= MAX_CELL_OUTPUT {
        return (output, false);
    }
    // Keep the tail; find a char boundary at/after the cut point.
    let cut = output.len() - MAX_CELL_OUTPUT;
    let mut boundary = cut;
    while boundary < output.len() && !output.is_char_boundary(boundary) {
        boundary += 1;
    }
    let tail = output.split_off(boundary);
    (
        format!("[output truncated to last {MAX_CELL_OUTPUT} bytes]\n{tail}"),
        true,
    )
}
