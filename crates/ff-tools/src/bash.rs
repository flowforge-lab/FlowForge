//! Shell command execution.
//!
//! Honesty note: a shell command can reach any path the user can (`cat /etc/passwd`,
//! absolute paths, `cd ..`), so unlike [`crate::view`]/[`crate::edit`] this is **not**
//! path-jailed — `root` only sets the working directory. The real safety lever is
//! [`Tool::safety`] classification + the host's approval gate for write/dangerous
//! commands. OS-level sandboxing (sandbox-exec / Landlock) is a tracked follow-up.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;

use crate::registry::{Safety, Tool, ToolOutcome};

const TIMEOUT: Duration = Duration::from_secs(120);

/// First tokens that are unambiguously read-only — auto-runnable without approval.
const READ_ONLY_CMDS: &[&str] = &[
    "ls", "cat", "pwd", "echo", "head", "tail", "wc", "rg", "grep", "fd", "stat", "file", "tree",
    "which", "whoami", "date", "printenv", "du", "df",
];

/// Substrings that mark a command as destructive regardless of context.
const DANGEROUS_PATTERNS: &[&str] = &[
    "rm -rf",
    "rm -fr",
    "sudo",
    "mkfs",
    "dd ",
    ":(){",
    "shutdown",
    "reboot",
    "> /dev/",
    "chmod -r 777",
    "git push --force",
    "git push -f",
    "curl",
    "wget",
];

pub struct BashTool;

impl BashTool {
    fn command_arg(args: &Value) -> Option<&str> {
        args.get("command").and_then(Value::as_str)
    }

    /// Resolve the effective working directory. `working_dir` is optional: absent
    /// uses `root`; a relative path is joined onto `root`; an absolute path is
    /// honored as-is. Like the shell command itself, this is intentionally not
    /// path-jailed (see module docs) -- it only sets `cwd`.
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

    fn classify(command: &str) -> Safety {
        let lower = command.to_lowercase();
        if DANGEROUS_PATTERNS.iter().any(|p| lower.contains(p)) {
            return Safety::Dangerous;
        }
        // Command substitution and redirects bypass the segment split below (they can
        // hide writes or run arbitrary nested commands), so never auto-run them.
        if ["$(", "`", ">", ">>"].iter().any(|t| command.contains(t)) {
            return Safety::Write;
        }
        // Read-only only when *every* segment (split on pipes/&&/;) starts with a
        // known read command. A single write segment downgrades the whole line.
        let segments: Vec<&str> = lower
            .split(['|', ';', '&'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let all_read_only = !segments.is_empty()
            && segments.iter().all(|seg| {
                seg.split_whitespace()
                    .next()
                    .map(|first| READ_ONLY_CMDS.contains(&first))
                    .unwrap_or(false)
            });
        if all_read_only {
            Safety::ReadOnly
        } else {
            Safety::Write
        }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command in the workspace directory and return its stdout, \
         stderr, and exit status. Use for builds, tests, git, and file inspection. \
         Pass `working_dir` to run in a subdirectory of the workspace."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to run."
                },
                "working_dir": {
                    "type": "string",
                    "description": "Directory to run the command in, relative to the \
                                    workspace root or absolute. Defaults to the workspace root."
                }
            },
            "required": ["command"]
        })
    }

    fn safety(&self, args: &Value) -> Safety {
        match Self::command_arg(args) {
            Some(cmd) => Self::classify(cmd),
            None => Safety::Dangerous,
        }
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        let Some(command) = Self::command_arg(&args) else {
            return ToolOutcome::error("missing required argument: command");
        };

        let dir = Self::resolve_dir(&args, root);
        if !dir.is_dir() {
            return ToolOutcome::error(format!(
                "working_dir does not exist or is not a directory: {}",
                dir.display()
            ));
        }

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let child = Command::new(&shell)
            .arg("-c")
            .arg(command)
            .current_dir(&dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output();

        match timeout(TIMEOUT, child).await {
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
            Ok(Err(e)) => ToolOutcome::error(format!("failed to spawn command: {e}")),
            Err(_) => ToolOutcome::error(format!("command timed out after {}s", TIMEOUT.as_secs())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_read_only() {
        assert_eq!(BashTool::classify("ls -la"), Safety::ReadOnly);
        assert_eq!(
            BashTool::classify("cat foo.txt | grep bar"),
            Safety::ReadOnly
        );
    }

    #[test]
    fn classifies_write_and_dangerous() {
        assert_eq!(BashTool::classify("touch new.txt"), Safety::Write);
        assert_eq!(BashTool::classify("cat x | rm -rf /"), Safety::Dangerous);
        assert_eq!(BashTool::classify("sudo reboot"), Safety::Dangerous);
        // A write segment downgrades an otherwise read-only pipeline.
        assert_eq!(BashTool::classify("ls && touch f"), Safety::Write);
    }

    #[test]
    fn metacharacters_are_never_read_only() {
        // Command substitution and redirects can hide writes / arbitrary exec even
        // when every visible token looks read-only.
        assert_eq!(BashTool::classify("echo $(rm -rf x)"), Safety::Dangerous);
        assert_eq!(BashTool::classify("echo $(date)"), Safety::Write);
        assert_eq!(BashTool::classify("cat `whoami`"), Safety::Write);
        assert_eq!(BashTool::classify("cat foo > bar"), Safety::Write);
        assert_eq!(BashTool::classify("echo hi >> log"), Safety::Write);
    }

    #[test]
    fn find_and_env_are_not_read_only() {
        // `find -delete` writes; `env CMD` runs arbitrary programs.
        assert_eq!(BashTool::classify("find . -name x"), Safety::Write);
        assert_eq!(BashTool::classify("env FOO=1 ls"), Safety::Write);
    }

    #[tokio::test]
    async fn runs_in_root_and_captures_output() {
        let dir = tempfile::tempdir().unwrap();
        let out = BashTool
            .run(serde_json::json!({"command": "echo hello"}), dir.path())
            .await;
        assert!(out.success);
        assert!(out.content.contains("hello"));
        assert!(out.content.contains("exit_code: 0"));
    }

    #[tokio::test]
    async fn nonzero_exit_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let out = BashTool
            .run(serde_json::json!({"command": "exit 3"}), dir.path())
            .await;
        assert!(!out.success);
        assert!(out.content.contains("exit_code: 3"));
    }

    #[tokio::test]
    async fn missing_command_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let out = BashTool.run(serde_json::json!({}), dir.path()).await;
        assert!(!out.success);
        assert!(out.content.contains("missing required argument"));
    }

    #[tokio::test]
    async fn working_dir_relative_runs_in_subdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let out = BashTool
            .run(
                serde_json::json!({"command": "pwd", "working_dir": "sub"}),
                dir.path(),
            )
            .await;
        assert!(out.success, "{}", out.content);
        // The canonical path ends with the subdir we asked for.
        assert!(out.content.contains("sub"));
    }

    #[tokio::test]
    async fn working_dir_absolute_is_honored() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("marker.txt"), "x").unwrap();
        let out = BashTool
            .run(
                serde_json::json!({
                    "command": "ls",
                    "working_dir": other.path().to_str().unwrap(),
                }),
                root.path(),
            )
            .await;
        assert!(out.success, "{}", out.content);
        assert!(out.content.contains("marker.txt"));
    }

    #[tokio::test]
    async fn missing_working_dir_runs_in_root() {
        let dir = tempfile::tempdir().unwrap();
        let out = BashTool
            .run(serde_json::json!({"command": "echo hi"}), dir.path())
            .await;
        assert!(out.success, "{}", out.content);
        assert!(out.content.contains("hi"));
    }

    #[tokio::test]
    async fn nonexistent_working_dir_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let out = BashTool
            .run(
                serde_json::json!({"command": "pwd", "working_dir": "does-not-exist"}),
                dir.path(),
            )
            .await;
        assert!(!out.success);
        assert!(out.content.contains("working_dir does not exist"));
    }
}
