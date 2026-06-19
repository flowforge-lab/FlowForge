//! Python execution.
//!
//! A dedicated `python` tool so the agent can write and run a Python snippet to
//! solve an intermediate step, rather than shelling out through a `bash` heredoc.
//! Stateless: every call is a fresh interpreter process (a stateful, kernel-like
//! variant where variables persist across calls is a tracked follow-up).
//!
//! Honesty note: like [`crate::bash`], Python is **not** sandboxed -- a snippet can
//! reach any path the user can, open sockets, or spawn subprocesses. It is always
//! classified [`Safety::Write`] so the host's approval gate covers it. OS-level
//! sandboxing (sandbox-exec / Landlock) is the same tracked follow-up that covers
//! `bash`.
//!
//! The snippet is fed to the interpreter on **stdin** (`python3 -`): no temp file
//! is written into the workspace, there is no argument-length limit, and tracebacks
//! still carry real line numbers (reported against `<stdin>`).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::registry::{Safety, Tool, ToolOutcome};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 600;

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

    /// Pick the interpreter: prefer the nearest project virtualenv
    /// (`.venv/bin/python`, then `venv/bin/python`) found by walking up from the
    /// working dir, so the agent runs with the project's dependencies even when it
    /// is invoked from a subdir of a monorepo whose `.venv` lives at the root.
    /// Falls back to `python3` on PATH when no virtualenv is found.
    fn interpreter(dir: &Path) -> PathBuf {
        for ancestor in dir.ancestors() {
            for venv in [".venv/bin/python", "venv/bin/python"] {
                let candidate = ancestor.join(venv);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
        PathBuf::from("python3")
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
        // Python can touch the filesystem, network, and subprocesses; proving a
        // snippet is read-only is infeasible, so always defer to the approval gate.
        Safety::Write
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skip_if_no_python() -> bool {
        std::process::Command::new("python3")
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

    #[test]
    fn always_write_safety() {
        assert_eq!(
            PythonTool.safety(&serde_json::json!({"code": "print(1)"})),
            Safety::Write
        );
    }

    #[test]
    fn interpreter_finds_venv_in_an_ancestor_of_the_working_dir() {
        // .venv at the project root must be found when running in a nested subdir.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".venv/bin")).unwrap();
        std::fs::write(root.join(".venv/bin/python"), "").unwrap();
        let sub = root.join("packages/app/src");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(PythonTool::interpreter(&sub), root.join(".venv/bin/python"));
    }

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
        assert_eq!(PythonTool::interpreter(&sub), sub.join(".venv/bin/python"));
    }

    #[test]
    fn interpreter_falls_back_to_path_python3_without_any_venv() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            PythonTool::interpreter(dir.path()),
            PathBuf::from("python3")
        );
    }
}
