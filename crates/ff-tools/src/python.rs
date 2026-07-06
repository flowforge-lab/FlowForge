//! Python execution.
//!
//! A dedicated `python` tool so the agent can write and run a Python snippet to
//! solve an intermediate step, rather than shelling out through a `bash` heredoc.
//! Stateless: every call is a fresh interpreter process (a stateful, kernel-like
//! variant where variables persist across calls is a tracked follow-up).
//!
//! Honesty note: like [`crate::bash`], Python is **not** sandboxed -- a snippet can
//! reach any path the user can, open sockets, or spawn subprocesses. It is always
//! classified [`Safety::Dangerous`], so the permission matrix (RFC 0019 §3) gates it:
//! Act prompts for confirmation and Auto denies it -- it is never silently run.
//! OS-level sandboxing (sandbox-exec / Landlock) is the same tracked follow-up that
//! covers `bash`.
//!
//! The snippet is fed to the interpreter on **stdin** (`python3 -`): no temp file
//! is written into the workspace, there is no argument-length limit, and tracebacks
//! still carry real line numbers (reported against `<stdin>`).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::timeout;

use crate::registry::{Safety, Tool, ToolOutcome};
use crate::sink::{OutputSink, OutputStream};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 600;

/// Live-stream byte cap per stream (#680 V2). Same semantics as bash: bounds the
/// transient live output forwarded to the frontend, independent of the final capture.
const MAX_STREAM_BYTES: usize = 256 * 1024;

// Interpreter layout differs by platform. Unix venvs put the interpreter at
// `bin/python` and the PATH fallback is `python3`; Windows venvs put it at
// `Scripts\python.exe` and ships `python` (the `python3` alias is the Store shim
// or absent). `py -3` would need a launcher arg, which the single-program spawn
// here does not model, so the PATH fallback is `python`.
#[cfg(not(windows))]
const VENV_PYTHON_SUBPATH: &str = "bin/python";
#[cfg(windows)]
const VENV_PYTHON_SUBPATH: &str = "Scripts/python.exe";

#[cfg(not(windows))]
const PROJECT_VENV_PYTHON: [&str; 2] = [".venv/bin/python", "venv/bin/python"];
#[cfg(windows)]
const PROJECT_VENV_PYTHON: [&str; 2] = [".venv/Scripts/python.exe", "venv/Scripts/python.exe"];

#[cfg(not(windows))]
const PATH_INTERPRETER: &str = "python3";
#[cfg(windows)]
const PATH_INTERPRETER: &str = "python";

pub struct PythonTool;

impl PythonTool {
    fn code_arg(args: &Value) -> Option<&str> {
        args.get("code").and_then(Value::as_str)
    }

    /// Resolve the working directory. Same semantics as `bash`: absent uses
    /// `root`, a relative path joins onto `root`, an absolute path is honored.
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

    /// Clamp a caller-supplied timeout to `[1, MAX_TIMEOUT_SECS]`, defaulting when absent.
    fn resolve_timeout(args: &Value) -> Duration {
        let secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS);
        Duration::from_secs(secs)
    }

    /// Pick the interpreter, in order of signal strength (the paths below are the
    /// unix layout; on Windows the interpreter is at `Scripts\python.exe` and the
    /// PATH fallback is `python` -- see [`VENV_PYTHON_SUBPATH`] / [`PATH_INTERPRETER`]):
    /// 1. an **activated** virtualenv (`$VIRTUAL_ENV/bin/python`) -- the explicit
    ///    intent of the launching shell (typically the CLI), which a working-dir
    ///    walk would never find;
    /// 2. the **nearest project** virtualenv (`.venv/bin/python`, then
    ///    `venv/bin/python`) walking up from the working dir, so the agent runs
    ///    with the project's deps even when invoked from a subdir of a monorepo
    ///    whose `.venv` lives at the root (the GUI case, where no env is inherited);
    /// 3. `python3` on PATH.
    ///
    /// Central-cache layouts (poetry/pipenv defaults, conda named envs) are not
    /// probed -- discovering those means invoking the tool, which is out of scope
    /// for a stateless snippet runner; they resolve via `python3`/PATH instead.
    fn interpreter(dir: &Path) -> PathBuf {
        Self::interpreter_with(std::env::var("VIRTUAL_ENV").ok(), dir)
    }

    /// Interpreter selection with `$VIRTUAL_ENV` injected, so the precedence is
    /// testable without mutating process-global environment state.
    fn interpreter_with(virtual_env: Option<String>, dir: &Path) -> PathBuf {
        if let Some(ve) = virtual_env.filter(|v| !v.trim().is_empty()) {
            let candidate = Path::new(&ve).join(VENV_PYTHON_SUBPATH);
            if candidate.is_file() {
                return candidate;
            }
        }
        for ancestor in dir.ancestors() {
            for venv in PROJECT_VENV_PYTHON {
                let candidate = ancestor.join(venv);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
        PathBuf::from(PATH_INTERPRETER)
    }
}

#[async_trait]
impl Tool for PythonTool {
    fn name(&self) -> &str {
        "python"
    }

    fn description(&self) -> &str {
        "Execute a Python 3 snippet in the workspace directory and return its \
         stdout, stderr, and exit status. Use for data processing, calculations, \
         or any step easier in Python than shell. Each call runs in a fresh \
         interpreter (no state persists between calls). Prefers a project \
         virtualenv (.venv/venv) when present. Pass `working_dir` to run in a \
         subdirectory of the workspace."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "The Python 3 source to execute."
                },
                "working_dir": {
                    "type": "string",
                    "description": "Directory to run in, relative to the workspace \
                                    root or absolute. Defaults to the workspace root."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Max seconds to run before the process is killed \
                                    (default 120, max 600)."
                }
            },
            "required": ["code"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        // Python runs arbitrary code (os.system, sockets, subprocesses) with no
        // detectable safe subset, so it is always Dangerous: the permission matrix
        // (RFC 0019 §3) then gates it -- Act prompts, Auto denies (never silent).
        Safety::Dangerous
    }

    fn max_safety(&self) -> Safety {
        Safety::Dangerous
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        let Some(code) = Self::code_arg(&args) else {
            return ToolOutcome::error("missing required argument: code");
        };

        let dir = Self::resolve_dir(&args, root);
        if !dir.is_dir() {
            return ToolOutcome::error(format!(
                "working_dir does not exist or is not a directory: {}",
                dir.display()
            ));
        }

        let interpreter = Self::interpreter(&dir);
        let limit = Self::resolve_timeout(&args);

        let spawned = Command::new(&interpreter)
            .arg("-")
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();

        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                return ToolOutcome::error(format!(
                    "failed to spawn python interpreter ({}): {e}",
                    interpreter.display()
                ));
            }
        };

        // Feed the snippet on stdin, then close it so the interpreter runs.
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(code.as_bytes()).await {
                return ToolOutcome::error(format!("failed to write code to python stdin: {e}"));
            }
            // Drop closes the pipe -> EOF -> interpreter executes.
            drop(stdin);
        }

        match timeout(limit, child.wait_with_output()).await {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let code = output.status.code().unwrap_or(-1);
                let body = format!(
                    "exit_code: {code}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                    stdout.trim_end(),
                    stderr.trim_end()
                );
                if output.status.success() {
                    ToolOutcome::ok(body)
                } else {
                    ToolOutcome::error(body)
                }
            }
            Ok(Err(e)) => ToolOutcome::error(format!("failed to run python: {e}")),
            Err(_) => ToolOutcome::error(format!("python timed out after {}s", limit.as_secs())),
        }
    }

    /// Streaming variant (#680 V2): when a sink is present, spawn the interpreter and
    /// drain its pipes as they produce, emitting each chunk to the sink while still
    /// building the full capture for the final outcome. Byte-for-byte identical result
    /// to `run`; the live stream is purely additive.
    async fn run_streaming(
        &self,
        args: Value,
        root: &Path,
        _session_id: &str,
        sink: Option<OutputSink>,
    ) -> ToolOutcome {
        let Some(sink) = sink else {
            return self.run(args, root).await;
        };
        let Some(code) = Self::code_arg(&args) else {
            return ToolOutcome::error("missing required argument: code");
        };
        let dir = Self::resolve_dir(&args, root);
        if !dir.is_dir() {
            return ToolOutcome::error(format!(
                "working_dir does not exist or is not a directory: {}",
                dir.display()
            ));
        }
        let interpreter = Self::interpreter(&dir);
        let limit = Self::resolve_timeout(&args);

        let mut child = match Command::new(&interpreter)
            .arg("-")
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                return ToolOutcome::error(format!(
                    "failed to spawn python interpreter ({}): {e}",
                    interpreter.display()
                ));
            }
        };

        // Feed the snippet on stdin, then close it so the interpreter runs.
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(code.as_bytes()).await {
                return ToolOutcome::error(format!("failed to write code to python stdin: {e}"));
            }
            drop(stdin);
        }

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        match timeout(limit, async {
            let (out_buf, err_buf) = tokio::join!(
                drain_stream(stdout, OutputStream::Stdout, sink.clone()),
                drain_stream(stderr, OutputStream::Stderr, sink),
            );
            child.wait().await.map(|status| (out_buf, err_buf, status))
        })
        .await
        {
            Ok(Ok((stdout_buf, stderr_buf, status))) => {
                format_output(&stdout_buf, &stderr_buf, status)
            }
            Ok(Err(e)) => ToolOutcome::error(format!("failed to run python: {e}")),
            Err(_) => ToolOutcome::error(format!("python timed out after {}s", limit.as_secs())),
        }
    }
}

/// Format captured stdout/stderr + exit status into the tool result body.
fn format_output(stdout: &[u8], stderr: &[u8], status: std::process::ExitStatus) -> ToolOutcome {
    let out = String::from_utf8_lossy(stdout);
    let err = String::from_utf8_lossy(stderr);
    let code = status.code().unwrap_or(-1);
    let body = format!(
        "exit_code: {code}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.trim_end(),
        err.trim_end()
    );
    if status.success() {
        ToolOutcome::ok(body)
    } else {
        ToolOutcome::error(body)
    }
}

/// Read `reader` to EOF, accumulating the full bytes for the final capture while
/// emitting each chunk to `sink` up to [`MAX_STREAM_BYTES`]. The returned buffer is
/// complete regardless of the live cap.
async fn drain_stream<R: AsyncRead + Unpin>(
    mut reader: R,
    stream: OutputStream,
    sink: OutputSink,
) -> Vec<u8> {
    let mut full = Vec::new();
    let mut emitted = 0usize;
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                full.extend_from_slice(&buf[..n]);
                if emitted < MAX_STREAM_BYTES {
                    let take = n.min(MAX_STREAM_BYTES - emitted);
                    sink.emit(stream, String::from_utf8_lossy(&buf[..take]).into_owned());
                    emitted += take;
                }
            }
        }
    }
    full
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skip_if_no_python() -> bool {
        std::process::Command::new(PATH_INTERPRETER)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn captures_stdout_and_exit_zero() {
        if !skip_if_no_python() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let out = PythonTool
            .run(
                serde_json::json!({"code": "print('hello from py')"}),
                dir.path(),
            )
            .await;
        assert!(out.success, "{}", out.content);
        assert!(out.content.contains("hello from py"));
        assert!(out.content.contains("exit_code: 0"));
    }

    #[tokio::test]
    async fn syntax_error_is_error_with_traceback() {
        if !skip_if_no_python() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let out = PythonTool
            .run(serde_json::json!({"code": "def (:"}), dir.path())
            .await;
        assert!(!out.success);
        assert!(
            out.content.contains("SyntaxError"),
            "expected SyntaxError in: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn runtime_error_is_error_with_line_number() {
        if !skip_if_no_python() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // Error on line 2 -> traceback must report line 2 (stdin keeps line numbers).
        let out = PythonTool
            .run(
                serde_json::json!({"code": "x = 1\nraise ValueError('boom')"}),
                dir.path(),
            )
            .await;
        assert!(!out.success);
        assert!(out.content.contains("ValueError"));
        assert!(
            out.content.contains("line 2"),
            "expected line 2 in: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn nonzero_exit_is_error() {
        if !skip_if_no_python() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let out = PythonTool
            .run(
                serde_json::json!({"code": "import sys; sys.exit(3)"}),
                dir.path(),
            )
            .await;
        assert!(!out.success);
        assert!(out.content.contains("exit_code: 3"));
    }

    #[tokio::test]
    async fn missing_code_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let out = PythonTool.run(serde_json::json!({}), dir.path()).await;
        assert!(!out.success);
        assert!(out.content.contains("missing required argument"));
    }

    #[tokio::test]
    async fn runs_in_relative_working_dir() {
        if !skip_if_no_python() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let out = PythonTool
            .run(
                serde_json::json!({
                    "code": "import os; print(os.path.basename(os.getcwd()))",
                    "working_dir": "sub"
                }),
                dir.path(),
            )
            .await;
        assert!(out.success, "{}", out.content);
        assert!(out.content.contains("sub"));
    }

    #[tokio::test]
    async fn nonexistent_working_dir_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let out = PythonTool
            .run(
                serde_json::json!({"code": "print(1)", "working_dir": "nope"}),
                dir.path(),
            )
            .await;
        assert!(!out.success);
        assert!(out.content.contains("working_dir does not exist"));
    }

    #[test]
    fn timeout_is_clamped() {
        assert_eq!(
            PythonTool::resolve_timeout(&serde_json::json!({})).as_secs(),
            DEFAULT_TIMEOUT_SECS
        );
        assert_eq!(
            PythonTool::resolve_timeout(&serde_json::json!({"timeout_secs": 99999})).as_secs(),
            MAX_TIMEOUT_SECS
        );
        assert_eq!(
            PythonTool::resolve_timeout(&serde_json::json!({"timeout_secs": 0})).as_secs(),
            1
        );
    }

    #[tokio::test]
    async fn run_streaming_emits_progressive_chunks() {
        if !skip_if_no_python() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = OutputSink::new(tx);
        let tool = PythonTool;
        let out = tool
            .run_streaming(
                serde_json::json!({
                    "code": "import sys\nfor i in range(3):\n    print(f'line{i}')\n    sys.stdout.flush()"
                }),
                dir.path(),
                crate::registry::NO_SESSION,
                Some(sink),
            )
            .await;

        assert!(out.success);
        assert!(out.content.contains("exit_code: 0"));
        assert!(out.content.contains("line0"));
        assert!(out.content.contains("line2"));

        let mut streamed = String::new();
        while let Ok((stream, delta)) = rx.try_recv() {
            assert_eq!(stream, OutputStream::Stdout);
            streamed.push_str(&delta);
        }
        assert!(streamed.contains("line0"), "stdout streamed: {streamed}");
        assert!(streamed.contains("line2"), "stdout streamed: {streamed}");
    }

    #[tokio::test]
    async fn run_streaming_without_sink_matches_run() {
        if !skip_if_no_python() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let tool = PythonTool;
        let cmd = serde_json::json!({"code": "print('hello')"});
        let buffered = tool.run(cmd.clone(), dir.path()).await;
        let streamed = tool
            .run_streaming(cmd, dir.path(), crate::registry::NO_SESSION, None)
            .await;
        assert_eq!(buffered, streamed);
    }

    #[tokio::test]
    async fn run_streaming_captures_stderr() {
        if !skip_if_no_python() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = OutputSink::new(tx);
        let tool = PythonTool;
        let out = tool
            .run_streaming(
                serde_json::json!({"code": "import sys; print('oops', file=sys.stderr)"}),
                dir.path(),
                crate::registry::NO_SESSION,
                Some(sink),
            )
            .await;
        assert!(out.content.contains("oops"));
        let mut saw_stderr = false;
        while let Ok((stream, delta)) = rx.try_recv() {
            if stream == OutputStream::Stderr && delta.contains("oops") {
                saw_stderr = true;
            }
        }
        assert!(saw_stderr, "stderr chunks are tagged Stderr and streamed");
    }

    #[test]
    fn always_dangerous_safety() {
        assert_eq!(
            PythonTool.safety(&serde_json::json!({"code": "print(1)"})),
            Safety::Dangerous
        );
        assert_eq!(PythonTool.max_safety(), Safety::Dangerous);
    }

    #[cfg(not(windows))]
    #[test]
    fn interpreter_finds_venv_in_an_ancestor_of_the_working_dir() {
        // .venv at the project root must be found when running in a nested subdir.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".venv/bin")).unwrap();
        std::fs::write(root.join(".venv/bin/python"), "").unwrap();
        let sub = root.join("packages/app/src");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(
            PythonTool::interpreter_with(None, &sub),
            root.join(".venv/bin/python")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn interpreter_prefers_the_nearest_venv_when_several_ancestors_have_one() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".venv/bin")).unwrap();
        std::fs::write(root.join(".venv/bin/python"), "").unwrap();
        let sub = root.join("pkg");
        std::fs::create_dir_all(sub.join(".venv/bin")).unwrap();
        std::fs::write(sub.join(".venv/bin/python"), "").unwrap();
        // The subdir's own venv wins over the root's.
        assert_eq!(
            PythonTool::interpreter_with(None, &sub),
            sub.join(".venv/bin/python")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn interpreter_falls_back_to_path_python3_without_any_venv() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            PythonTool::interpreter_with(None, dir.path()),
            PathBuf::from("python3")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn interpreter_prefers_an_activated_virtual_env_over_a_project_venv() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A project .venv in the working dir...
        std::fs::create_dir_all(root.join(".venv/bin")).unwrap();
        std::fs::write(root.join(".venv/bin/python"), "").unwrap();
        // ...is still beaten by an activated $VIRTUAL_ENV elsewhere.
        let active = root.join("active-env");
        std::fs::create_dir_all(active.join("bin")).unwrap();
        std::fs::write(active.join("bin/python"), "").unwrap();
        assert_eq!(
            PythonTool::interpreter_with(Some(active.to_string_lossy().into_owned()), root),
            active.join("bin/python")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn interpreter_ignores_a_stale_or_empty_virtual_env_and_walks_up() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".venv/bin")).unwrap();
        std::fs::write(root.join(".venv/bin/python"), "").unwrap();
        // $VIRTUAL_ENV set but empty or pointing nowhere real -> ignored, walk-up wins.
        for stale in [
            String::new(),
            root.join("gone").to_string_lossy().into_owned(),
        ] {
            assert_eq!(
                PythonTool::interpreter_with(Some(stale), root),
                root.join(".venv/bin/python")
            );
        }
    }

    // Windows mirrors of the layout-specific cases above: venvs live at
    // `Scripts\python.exe` (not `bin/python`) and the PATH fallback is `python`
    // (not `python3`). The precedence logic itself is platform-independent and is
    // covered by the unix tests above.
    #[cfg(windows)]
    #[test]
    fn interpreter_finds_windows_scripts_venv_in_an_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".venv/Scripts")).unwrap();
        std::fs::write(root.join(".venv/Scripts/python.exe"), "").unwrap();
        let sub = root.join("packages/app/src");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(
            PythonTool::interpreter_with(None, &sub),
            root.join(".venv/Scripts/python.exe")
        );
    }

    #[cfg(windows)]
    #[test]
    fn interpreter_falls_back_to_path_python_without_any_venv() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            PythonTool::interpreter_with(None, dir.path()),
            PathBuf::from("python")
        );
    }
}
