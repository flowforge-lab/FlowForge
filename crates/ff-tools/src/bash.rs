//! Shell command execution.
//!
//! Honesty note: a shell command can reach any path the user can (`cat /etc/passwd`,
//! absolute paths, `cd ..`), so unlike [`crate::view`]/[`crate::edit`] this is **not**
//! path-jailed — `root` only sets the working directory. The real safety lever is
//! [`Tool::safety`] classification + the host's approval gate for write/dangerous
//! commands. OS-level sandboxing (sandbox-exec / Landlock) is a tracked follow-up.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;

use crate::registry::{Safety, Tool, ToolOutcome};

/// Default per-call wall-clock budget when `timeout_secs` is not supplied.
const DEFAULT_TIMEOUT_SECS: u64 = 120;
/// Hard ceiling for a caller-supplied `timeout_secs`. Long enough for a cold
/// `cargo build` + test (the case that used to force a background-poll loop),
/// short enough to stay a foreground call rather than an unbounded job.
const MAX_TIMEOUT_SECS: u64 = 600;

/// Workspace-relative scratch directory (#458 RC4c). A sanctioned, always-writable
/// temp location under the workspace root so the agent never reaches for `/tmp`
/// (sandbox-denied in the field). Created lazily and exported as `TMPDIR`/`TMP` for
/// the child so even tools that default to `/tmp` redirect here.
const SCRATCH_DIR: &str = ".ff-scratch";

/// Scratch entries older than this are pruned on the next `bash` run (#483). Intermediate
/// scripts and middle-result files are ephemeral; a week is long enough that anything this
/// old belongs to a finished session. `.ff-scratch/` is workspace-scoped, so nothing else
/// ever clears it.
const MAX_SCRATCH_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

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

/// Best-effort age prune of the scratch dir (#483): remove entries whose mtime is older
/// than `max_age`, preserving the dir itself and its `.gitignore`. All errors are
/// swallowed -- a prune failure must never fail the tool call (same discipline as the
/// scratch creation above).
fn prune_scratch(scratch: &Path, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(scratch) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        if entry.file_name() == ".gitignore" {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        // A future mtime (clock skew) reads as not-old, so the entry is kept.
        if now.duration_since(modified).unwrap_or(Duration::ZERO) <= max_age {
            continue;
        }
        let path = entry.path();
        if meta.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}

pub struct BashTool;

impl BashTool {
    fn command_arg(args: &Value) -> Option<&str> {
        args.get("command")
            .and_then(Value::as_str)
            .map(Self::strip_command_prefix)
    }

    /// Resolve the per-call timeout: an optional `timeout_secs`, clamped to
    /// `[1, MAX_TIMEOUT_SECS]`, else the default. Lets a multi-minute build run in
    /// one foreground call instead of a background job polled with `sleep`-loops
    /// that re-hit a fixed ceiling (#479).
    fn timeout_for(args: &Value) -> Duration {
        match args.get("timeout_secs").and_then(Value::as_u64) {
            Some(secs) => Duration::from_secs(secs.clamp(1, MAX_TIMEOUT_SECS)),
            None => Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    /// Strip a single leaked `command:` key prefix from the value (#458 RC4a).
    /// Some models (SiliconFlow GLM / DeepSeek) echo the schema key into the value,
    /// so the tool argument arrives as `command: ls` and the shell then runs the
    /// literal `command:` token and fails. Normalizing here -- the single chokepoint
    /// feeding both `run` and `safety` -- fixes dispatch and classification at once.
    ///
    /// Narrow on purpose: only a *single leading* `command` (case-insensitive),
    /// optional whitespace, then `:` is removed. A command that merely contains
    /// `command:` later (`git commit -m "command: x"`) or uses the shell builtin
    /// (`command -v ls`, no colon) is left untouched.
    fn strip_command_prefix(s: &str) -> &str {
        let t = s.trim_start();
        if t.get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case("command"))
        {
            if let Some(rest) = t[7..].trim_start().strip_prefix(':') {
                return rest.trim_start();
            }
        }
        s
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

    /// Strip a redundant leading `cd <workspace> && …` from the command (#458 RC4b).
    /// The bash tool already runs from the workspace root, so an agent-prepended
    /// `cd /abs/workspace && …` either no-ops or fails (`no such file or directory`)
    /// and burns a call. Conservative: only strips when the `cd` target resolves to
    /// `root` itself -- a real `cd subdir && …` is left intact. A bare `cd <root>`
    /// with nothing after becomes a no-op (`true`). Returns the command to execute.
    fn strip_redundant_cd<'a>(command: &'a str, root: &Path) -> &'a str {
        let t = command.trim_start();
        let Some(after_cd) = t.strip_prefix("cd ").map(str::trim_start) else {
            return command;
        };
        // The cd target runs to the first command separator (&&, ;, or newline).
        let sep = ["&&", ";", "\n"]
            .iter()
            .filter_map(|s| after_cd.find(s).map(|i| (i, s.len())))
            .min_by_key(|&(i, _)| i);
        let (target_raw, rest) = match sep {
            Some((i, len)) => (after_cd[..i].trim(), after_cd[i + len..].trim_start()),
            None => (after_cd.trim(), ""),
        };
        // Unquote a simply-quoted target ("..." or '...').
        let target = target_raw
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| {
                target_raw
                    .strip_prefix('\'')
                    .and_then(|s| s.strip_suffix('\''))
            })
            .unwrap_or(target_raw);
        let is_root = std::fs::canonicalize(target)
            .ok()
            .zip(std::fs::canonicalize(root).ok())
            .map(|(a, b)| a == b)
            .unwrap_or_else(|| Path::new(target) == root);
        if is_root {
            if rest.is_empty() {
                "true"
            } else {
                rest
            }
        } else {
            command
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
         Commands already run from the workspace root -- issue bare commands; do NOT \
         prefix `cd <workspace>` (use `working_dir` for a subdirectory instead). For \
         temporary files use the workspace scratch dir `.ff-scratch/` (created for \
         you), never `/tmp`. A command runs for at most 120s by default; for a slow \
         build or test, pass `timeout_secs` (max 600) and run it in the foreground \
         rather than backgrounding and polling. On Windows commands run under PowerShell (`pwsh`) or `cmd.exe`, not bash -- prefer cross-platform invocations."
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
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Wall-clock budget for this command, in seconds \
                                    (default 120, max 600). Raise it to run a slow build \
                                    or test suite in the foreground in a single call \
                                    instead of backgrounding and polling."
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

    fn max_safety(&self) -> Safety {
        Safety::Dangerous
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        let Some(command) = Self::command_arg(&args) else {
            return ToolOutcome::error("missing required argument: command");
        };
        // Drop a redundant `cd <workspace> && …` the model sometimes prepends (#458 RC4b).
        let command = Self::strip_redundant_cd(command, root);

        let dir = Self::resolve_dir(&args, root);
        if !dir.is_dir() {
            return ToolOutcome::error(format!(
                "working_dir does not exist or is not a directory: {}",
                dir.display()
            ));
        }

        // Sanctioned workspace scratch dir (#458 RC4c): create it and point the
        // child's TMPDIR/TMP at it so `/tmp`-defaulting tools redirect. Best-effort:
        // a creation failure just leaves the system default temp dir in place.
        let scratch = root.join(SCRATCH_DIR);
        if std::fs::create_dir_all(&scratch).is_ok() {
            // Make the dir self-ignoring in ANY host project (#458 review follow-up):
            // a `.gitignore` of `*` keeps `.ff-scratch/` out of the user's `git status`
            // even when their repo's own `.gitignore` knows nothing about it. Written
            // only if absent so we never clobber a user edit.
            let ignore = scratch.join(".gitignore");
            if !ignore.exists() {
                let _ = std::fs::write(&ignore, "*\n");
            }
            // Age-prune stale scratch (#483) so ephemeral scripts/results from finished
            // sessions don't accumulate unbounded -- the dir is workspace-scoped, so no
            // session-delete or quit hook ever clears it.
            prune_scratch(&scratch, MAX_SCRATCH_AGE);
        }

        let timeout_budget = Self::timeout_for(&args);
        let (program, flag) = crate::shell::shell_invocation();
        let child = Command::new(&program)
            .arg(flag)
            .arg(command)
            .current_dir(&dir)
            .env("TMPDIR", &scratch)
            .env("TMP", &scratch)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output();

        match timeout(timeout_budget, child).await {
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
            Err(_) => ToolOutcome::error(format!(
                "command timed out after {}s. For a longer job, pass `timeout_secs` (max {}); for one longer than that, run it in the background and poll with quick, non-blocking checks rather than sleeping inside a call.",
                timeout_budget.as_secs(),
                MAX_TIMEOUT_SECS
            )),
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

    // ---- #458 RC4a: leaked `command:` prefix ----

    #[test]
    fn strips_leaked_command_prefix() {
        assert_eq!(
            BashTool::strip_command_prefix("command: echo hi"),
            "echo hi"
        );
        assert_eq!(BashTool::strip_command_prefix("command:echo hi"), "echo hi");
        assert_eq!(BashTool::strip_command_prefix("  COMMAND : ls"), "ls");
        // Only a single leading prefix is removed.
        assert_eq!(
            BashTool::strip_command_prefix("command: command: ls"),
            "command: ls"
        );
    }

    #[test]
    fn command_prefix_strip_is_narrow() {
        // `command` builtin with no colon is untouched.
        assert_eq!(
            BashTool::strip_command_prefix("command -v ls"),
            "command -v ls"
        );
        // A `command:` that merely appears later is preserved.
        let c = r#"git commit -m "command: x""#;
        assert_eq!(BashTool::strip_command_prefix(c), c);
    }

    #[tokio::test]
    async fn run_normalizes_command_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let out = BashTool
            .run(
                serde_json::json!({"command": "command: echo hi"}),
                dir.path(),
            )
            .await;
        assert!(out.success, "{}", out.content);
        assert!(out.content.contains("hi"));
        // The literal `command:` token never reached the shell (no "command not found").
        assert!(!out.content.to_lowercase().contains("command not found"));
    }

    // ---- #458 RC4b: redundant `cd <workspace>` ----

    #[test]
    fn strips_redundant_cd_into_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let cmd = format!("cd {} && echo hi", root.display());
        assert_eq!(BashTool::strip_redundant_cd(&cmd, root), "echo hi");
        // Bare `cd <root>` with nothing after becomes a no-op.
        let bare = format!("cd {}", root.display());
        assert_eq!(BashTool::strip_redundant_cd(&bare, root), "true");
    }

    #[test]
    fn keeps_real_cd_into_subdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let cmd = "cd sub && ls";
        // A genuine subdir cd is left intact.
        assert_eq!(BashTool::strip_redundant_cd(cmd, dir.path()), cmd);
        // A command with no leading cd is untouched.
        assert_eq!(BashTool::strip_redundant_cd("ls -la", dir.path()), "ls -la");
    }

    #[tokio::test]
    async fn run_strips_redundant_cd() {
        let dir = tempfile::tempdir().unwrap();
        let out = BashTool
            .run(
                serde_json::json!({"command": format!("cd {} && echo hi", dir.path().display())}),
                dir.path(),
            )
            .await;
        assert!(out.success, "{}", out.content);
        assert!(out.content.contains("hi"));
        assert!(!out.content.contains("No such file or directory"));
    }

    // ---- #458 RC4c: workspace scratch dir + TMPDIR ----

    #[tokio::test]
    async fn creates_scratch_dir_and_points_tmpdir_at_it() {
        let dir = tempfile::tempdir().unwrap();
        let out = BashTool
            .run(serde_json::json!({"command": "echo $TMPDIR"}), dir.path())
            .await;
        assert!(out.success, "{}", out.content);
        assert!(
            out.content.contains(SCRATCH_DIR),
            "TMPDIR should point at the scratch dir: {}",
            out.content
        );
        assert!(
            dir.path().join(SCRATCH_DIR).is_dir(),
            "scratch dir must be created"
        );
        // Self-ignoring in any host project (#458 review follow-up).
        let ignore = dir.path().join(SCRATCH_DIR).join(".gitignore");
        assert_eq!(
            std::fs::read_to_string(&ignore).unwrap_or_default(),
            "*\n",
            "scratch dir must carry a `*` .gitignore"
        );
    }

    #[tokio::test]
    async fn scratch_gitignore_is_not_clobbered_if_present() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join(SCRATCH_DIR);
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join(".gitignore"), "custom\n").unwrap();
        let out = BashTool
            .run(serde_json::json!({"command": "true"}), dir.path())
            .await;
        assert!(out.success, "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(scratch.join(".gitignore")).unwrap(),
            "custom\n",
            "an existing .gitignore must be preserved"
        );
    }

    // ---- #479: configurable per-call timeout ----

    #[test]
    fn timeout_defaults_when_absent() {
        let d = BashTool::timeout_for(&serde_json::json!({"command": "true"}));
        assert_eq!(d, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
    }

    #[test]
    fn timeout_honors_and_clamps_caller_value() {
        // An in-range value is honored.
        assert_eq!(
            BashTool::timeout_for(&serde_json::json!({"timeout_secs": 300})),
            Duration::from_secs(300)
        );
        // Above the ceiling clamps down.
        assert_eq!(
            BashTool::timeout_for(&serde_json::json!({"timeout_secs": 99999})),
            Duration::from_secs(MAX_TIMEOUT_SECS)
        );
        // Zero clamps up to the 1s floor (never an instant timeout).
        assert_eq!(
            BashTool::timeout_for(&serde_json::json!({"timeout_secs": 0})),
            Duration::from_secs(1)
        );
        // A non-integer value falls back to the default.
        assert_eq!(
            BashTool::timeout_for(&serde_json::json!({"timeout_secs": "lots"})),
            Duration::from_secs(DEFAULT_TIMEOUT_SECS)
        );
    }

    #[tokio::test]
    async fn run_enforces_a_short_caller_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let out = BashTool
            .run(
                serde_json::json!({"command": "sleep 5", "timeout_secs": 1}),
                dir.path(),
            )
            .await;
        assert!(!out.success, "a 5s sleep under a 1s budget must time out");
        assert!(
            out.content.contains("timed out after 1s") && out.content.contains("timeout_secs"),
            "the timeout error should name the budget and teach `timeout_secs`: {}",
            out.content
        );
    }

    // ---- #483: age-based scratch prune ----

    /// Backdate an entry's mtime so the age prune treats it as stale.
    fn age_entry(path: &Path, age: Duration) {
        let when = SystemTime::now() - age;
        std::fs::File::open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    #[test]
    fn prune_removes_stale_entries_and_keeps_recent_ones() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path();

        let stale_file = scratch.join("old.txt");
        std::fs::write(&stale_file, "x").unwrap();
        age_entry(&stale_file, Duration::from_secs(10 * 24 * 60 * 60));

        let stale_dir = scratch.join("old-run");
        std::fs::create_dir(&stale_dir).unwrap();
        std::fs::write(stale_dir.join("r.json"), "{}").unwrap();
        age_entry(&stale_dir, Duration::from_secs(8 * 24 * 60 * 60));

        let fresh = scratch.join("recent.txt");
        std::fs::write(&fresh, "y").unwrap();

        prune_scratch(scratch, MAX_SCRATCH_AGE);

        assert!(!stale_file.exists(), "stale file should be pruned");
        assert!(!stale_dir.exists(), "stale dir should be pruned");
        assert!(fresh.exists(), "recent entry must be kept");
    }

    #[test]
    fn prune_preserves_gitignore_even_when_stale() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path();
        let ignore = scratch.join(".gitignore");
        std::fs::write(&ignore, "*\n").unwrap();
        age_entry(&ignore, Duration::from_secs(30 * 24 * 60 * 60));

        prune_scratch(scratch, MAX_SCRATCH_AGE);

        assert!(ignore.exists(), ".gitignore must never be pruned");
        assert_eq!(std::fs::read_to_string(&ignore).unwrap(), "*\n");
    }

    #[test]
    fn prune_on_missing_dir_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        // Does not panic and leaves nothing behind.
        prune_scratch(&dir.path().join("does-not-exist"), MAX_SCRATCH_AGE);
    }
}
