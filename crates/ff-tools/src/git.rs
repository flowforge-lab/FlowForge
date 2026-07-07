//! Structured read-only git queries (#855). Exposes `status`, `diff`, `log`,
//! and `show` as structured, token-efficient results. All actions are ReadOnly,
//! so this tool is available in Plan mode.

use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;

use crate::registry::{Safety, Tool, ToolOutcome};

/// Max lines of unified diff output before truncation.
const MAX_DIFF_LINES: usize = 500;
/// Default number of log entries.
const DEFAULT_LOG_LIMIT: u32 = 10;

pub struct GitTool;

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &str {
        "git"
    }

    fn description(&self) -> &str {
        "Read-only git queries with structured output. Actions: status (branch + \
         staged/modified/untracked), diff (stat or unified with line cap), log \
         (structured commits), show (single commit). All read-only — available in \
         Plan mode. For mutations (commit, push, rebase) use bash or github tool."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "The query to run.",
                    "enum": ["status", "diff", "log", "show"]
                },
                "stat": {
                    "type": "boolean",
                    "description": "For diff: return per-file summary (added/removed lines) instead of unified diff. Default false."
                },
                "staged": {
                    "type": "boolean",
                    "description": "For diff: show staged changes only (--cached). Default false."
                },
                "path": {
                    "type": "string",
                    "description": "For diff/log: limit to a specific file or directory path."
                },
                "ref": {
                    "type": "string",
                    "description": "For diff: compare against a ref (branch/commit). For show: the commit to show. Default HEAD."
                },
                "n": {
                    "type": "integer",
                    "description": "For log: max number of entries (default 10, max 50)."
                }
            },
            "required": ["action"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::ReadOnly
    }

    fn max_safety(&self) -> Safety {
        Safety::ReadOnly
    }

    fn min_safety(&self) -> Safety {
        Safety::ReadOnly
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        let action = match args.get("action").and_then(|a| a.as_str()) {
            Some(a) => a,
            None => return ToolOutcome::error("missing required parameter: action"),
        };

        match action {
            "status" => git_status(root).await,
            "diff" => git_diff(&args, root).await,
            "log" => git_log(&args, root).await,
            "show" => git_show(&args, root).await,
            _ => ToolOutcome::error(format!("unknown action: {action}")),
        }
    }
}

// ─── status ──────────────────────────────────────────────────────────────────

async fn git_status(root: &Path) -> ToolOutcome {
    let output = match run_git(
        root,
        &["status", "--porcelain=v2", "-b", "--untracked-files=all"],
    )
    .await
    {
        Ok(o) => o,
        Err(e) => return e,
    };

    let mut branch = String::new();
    let mut upstream = String::new();
    let mut ahead: u32 = 0;
    let mut behind: u32 = 0;
    let mut staged: Vec<String> = Vec::new();
    let mut modified: Vec<String> = Vec::new();
    let mut untracked: Vec<String> = Vec::new();

    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            branch = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            upstream = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            // Format: "+N -M"
            for part in rest.split_whitespace() {
                if let Some(n) = part.strip_prefix('+') {
                    ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    behind = n.parse().unwrap_or(0);
                }
            }
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
            // Changed entry: "1 XY sub mH mI mW hH hI path"
            // or rename:     "2 XY sub mH mI mW hH hI X score path\tpath"
            let parts: Vec<&str> = line.splitn(9, ' ').collect();
            if parts.len() >= 9 {
                let xy = parts[1];
                let path = if line.starts_with("2 ") {
                    // Rename: path contains \t
                    parts[8].rsplit('\t').next().unwrap_or(parts[8])
                } else {
                    parts[8]
                };
                let x = xy.as_bytes().first().copied().unwrap_or(b'.');
                let y = xy.as_bytes().get(1).copied().unwrap_or(b'.');

                if x != b'.' && x != b'?' {
                    staged.push(format!("{} {path}", char::from(x)));
                }
                if y != b'.' && y != b'?' {
                    modified.push(path.to_string());
                }
            }
        } else if let Some(rest) = line.strip_prefix("? ") {
            untracked.push(rest.to_string());
        }
    }

    let mut result = format!("branch: {branch}");
    if !upstream.is_empty() {
        result.push_str(&format!("\nupstream: {upstream}"));
        if ahead > 0 || behind > 0 {
            result.push_str(&format!(" (ahead {ahead}, behind {behind})"));
        }
    }

    if staged.is_empty() && modified.is_empty() && untracked.is_empty() {
        result.push_str("\n\nClean working tree.");
    } else {
        if !staged.is_empty() {
            result.push_str("\n\nStaged:");
            for f in &staged {
                result.push_str(&format!("\n  {f}"));
            }
        }
        if !modified.is_empty() {
            result.push_str("\n\nModified:");
            for f in &modified {
                result.push_str(&format!("\n  {f}"));
            }
        }
        if !untracked.is_empty() {
            result.push_str("\n\nUntracked:");
            for f in &untracked {
                result.push_str(&format!("\n  {f}"));
            }
        }
    }

    ToolOutcome::ok(result)
}

// ─── diff ────────────────────────────────────────────────────────────────────

async fn git_diff(args: &Value, root: &Path) -> ToolOutcome {
    let stat = args.get("stat").and_then(|v| v.as_bool()).unwrap_or(false);
    let staged = args
        .get("staged")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let path = args.get("path").and_then(|v| v.as_str());
    let git_ref = args.get("ref").and_then(|v| v.as_str());

    let mut cmd_args: Vec<&str> = vec!["diff"];

    if staged {
        cmd_args.push("--cached");
    }

    if stat {
        cmd_args.push("--numstat");
    }

    if let Some(r) = git_ref {
        cmd_args.push(r);
    }

    cmd_args.push("--");

    if let Some(p) = path {
        cmd_args.push(p);
    }

    let output = match run_git(root, &cmd_args).await {
        Ok(o) => o,
        Err(e) => return e,
    };

    if output.trim().is_empty() {
        return ToolOutcome::ok("No differences.".to_string());
    }

    if stat {
        // --numstat: "added\tremoved\tpath" per line
        let result = parse_numstat(&output);
        ToolOutcome::ok(result)
    } else {
        // Unified diff with line cap
        let lines: Vec<&str> = output.lines().collect();
        if lines.len() <= MAX_DIFF_LINES {
            ToolOutcome::ok(output)
        } else {
            let truncated: String = lines[..MAX_DIFF_LINES].join("\n");
            ToolOutcome::ok(format!(
                "{truncated}\n\n... truncated ({} lines total, showing first {MAX_DIFF_LINES})",
                lines.len()
            ))
        }
    }
}

fn parse_numstat(output: &str) -> String {
    let mut result = String::from("File changes:\n");
    let mut total_added: u32 = 0;
    let mut total_removed: u32 = 0;

    for line in output.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 {
            let added = parts[0];
            let removed = parts[1];
            let path = parts[2];
            result.push_str(&format!("  +{added} -{removed}\t{path}\n"));
            // binary files show "-" for counts
            total_added += added.parse::<u32>().unwrap_or(0);
            total_removed += removed.parse::<u32>().unwrap_or(0);
        }
    }

    result.push_str(&format!(
        "\nTotal: +{total_added} -{total_removed} in {} file(s)",
        output.lines().count()
    ));
    result
}

// ─── log ─────────────────────────────────────────────────────────────────────

async fn git_log(args: &Value, root: &Path) -> ToolOutcome {
    let n = args
        .get("n")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_LOG_LIMIT as u64)
        .min(50) as u32;
    let path = args.get("path").and_then(|v| v.as_str());

    let n_str = format!("-{n}");
    let fmt_arg = "--format=%H%x00%s%x00%aN%x00%aI";
    let mut cmd_args: Vec<&str> = vec!["log", &n_str, fmt_arg];

    if let Some(p) = path {
        cmd_args.push("--");
        cmd_args.push(p);
    }

    let output = match run_git(root, &cmd_args).await {
        Ok(o) => o,
        Err(e) => return e,
    };

    if output.trim().is_empty() {
        return ToolOutcome::ok("No commits.".to_string());
    }

    let mut result = String::new();
    for line in output.lines() {
        let parts: Vec<&str> = line.splitn(4, '\x00').collect();
        if parts.len() >= 4 {
            let hash = &parts[0][..7.min(parts[0].len())];
            let subject = parts[1];
            let author = parts[2];
            let date = parts[3];
            result.push_str(&format!("{hash} {subject} ({author}, {date})\n"));
        }
    }

    ToolOutcome::ok(result.trim_end().to_string())
}

// ─── show ────────────────────────────────────────────────────────────────────

async fn git_show(args: &Value, root: &Path) -> ToolOutcome {
    let commit = args.get("ref").and_then(|v| v.as_str()).unwrap_or("HEAD");

    // Get commit metadata
    let fmt_arg = "--format=%H%x00%s%x00%aN%x00%aI%x00%b";
    let meta_args = vec!["show", "--no-patch", fmt_arg, commit];
    let meta = match run_git(root, &meta_args).await {
        Ok(o) => o,
        Err(e) => return e,
    };

    let mut result = String::new();
    if let Some(first_line) = meta.lines().next() {
        let parts: Vec<&str> = first_line.splitn(5, '\x00').collect();
        if parts.len() >= 5 {
            result.push_str(&format!("commit: {}\n", parts[0]));
            result.push_str(&format!("author: {} ({})\n", parts[2], parts[3]));
            result.push_str(&format!("subject: {}\n", parts[1]));
            let body = parts[4].trim();
            if !body.is_empty() {
                result.push_str(&format!("\n{body}\n"));
            }
        }
    }

    // Get the diff stat
    let stat_args = vec!["show", "--stat", "--format=", commit];
    if let Ok(stat) = run_git(root, &stat_args).await {
        if !stat.trim().is_empty() {
            result.push_str(&format!("\n{}", stat.trim()));
        }
    }

    // Get unified diff (bounded)
    let diff_args = vec!["show", "--format=", commit];
    if let Ok(diff) = run_git(root, &diff_args).await {
        if !diff.trim().is_empty() {
            let lines: Vec<&str> = diff.lines().collect();
            if lines.len() <= MAX_DIFF_LINES {
                result.push_str(&format!("\n\n{diff}"));
            } else {
                let truncated: String = lines[..MAX_DIFF_LINES].join("\n");
                result.push_str(&format!(
                    "\n\n{truncated}\n\n... truncated ({} lines total, showing first {MAX_DIFF_LINES})",
                    lines.len()
                ));
            }
        }
    }

    ToolOutcome::ok(result)
}

// ─── helpers ─────────────────────────────────────────────────────────────────

async fn run_git(root: &Path, args: &[&str]) -> Result<String, ToolOutcome> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .output()
        .await
        .map_err(|e| ToolOutcome::error(format!("failed to run git: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Some failures are informational (e.g. empty log = not an error)
        if stderr.contains("not a git repository") {
            return Err(ToolOutcome::error(
                "Not a git repository (or any parent up to mount point).".to_string(),
            ));
        }
        if stderr.contains("unknown revision") || stderr.contains("bad revision") {
            return Err(ToolOutcome::error(format!(
                "Unknown revision or path: {}",
                stderr.trim()
            )));
        }
        // For other non-zero exits, return what we got (some git commands return
        // non-zero with useful output, e.g. diff with changes)
        let combined = format!("{}{}", stdout, stderr);
        if combined.trim().is_empty() {
            return Err(ToolOutcome::error(format!(
                "git {} failed: {}",
                args.first().unwrap_or(&""),
                stderr.trim()
            )));
        }
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_numstat_basic() {
        let output = "10\t2\tsrc/main.rs\n5\t0\tsrc/new.rs\n-\t-\timage.png\n";
        let result = parse_numstat(output);
        assert!(result.contains("+10 -2\tsrc/main.rs"));
        assert!(result.contains("+5 -0\tsrc/new.rs"));
        assert!(result.contains("image.png"));
        assert!(result.contains("Total: +15 -2 in 3 file(s)"));
    }

    #[test]
    fn parse_numstat_empty() {
        let result = parse_numstat("");
        assert!(result.contains("Total: +0 -0 in 0 file(s)"));
    }

    #[test]
    fn diff_truncation_logic() {
        let lines: Vec<String> = (0..600).map(|i| format!("line {i}")).collect();
        let output = lines.join("\n");
        let line_vec: Vec<&str> = output.lines().collect();

        assert!(line_vec.len() > MAX_DIFF_LINES);
        let truncated: String = line_vec[..MAX_DIFF_LINES].join("\n");
        let result = format!(
            "{truncated}\n\n... truncated ({} lines total, showing first {MAX_DIFF_LINES})",
            line_vec.len()
        );
        assert!(result.contains("line 0"));
        assert!(result.contains("line 499"));
        assert!(!result.contains("line 500\n"));
        assert!(result.contains("600 lines total"));
    }

    #[test]
    fn log_output_formatting() {
        let output = "abc1234def5678901234567890123456789012345\x00feat: add git tool\x00Tony\x002026-07-07T10:00:00-05:00\nbcd2345ef67890123456789012345678901234567\x00fix: typo\x00Alice\x002026-07-06T09:00:00-05:00\n";

        let mut result = String::new();
        for line in output.lines() {
            let parts: Vec<&str> = line.splitn(4, '\x00').collect();
            if parts.len() >= 4 {
                let hash = &parts[0][..7.min(parts[0].len())];
                let subject = parts[1];
                let author = parts[2];
                let date = parts[3];
                result.push_str(&format!("{hash} {subject} ({author}, {date})\n"));
            }
        }

        assert!(result.contains("abc1234 feat: add git tool (Tony, 2026-07-07T10:00:00-05:00)"));
        assert!(result.contains("bcd2345 fix: typo (Alice, 2026-07-06T09:00:00-05:00)"));
    }

    #[test]
    fn safety_always_readonly() {
        let tool = GitTool;
        assert_eq!(
            tool.safety(&serde_json::json!({"action": "status"})),
            Safety::ReadOnly
        );
        assert_eq!(
            tool.safety(&serde_json::json!({"action": "diff"})),
            Safety::ReadOnly
        );
        assert_eq!(
            tool.safety(&serde_json::json!({"action": "log"})),
            Safety::ReadOnly
        );
        assert_eq!(
            tool.safety(&serde_json::json!({"action": "show"})),
            Safety::ReadOnly
        );
        assert_eq!(tool.max_safety(), Safety::ReadOnly);
        assert_eq!(tool.min_safety(), Safety::ReadOnly);
    }

    #[test]
    fn status_parsing_with_changes() {
        // Simulate porcelain v2 output parsing
        let output = "# branch.head feat/test\n# branch.upstream origin/feat/test\n# branch.ab +3 -1\n1 M. N... 100644 100644 100644 abc123 def456 src/main.rs\n1 .M N... 100644 100644 100644 abc123 def456 src/lib.rs\n? new_file.txt\n";

        let mut staged: Vec<String> = Vec::new();
        let mut modified: Vec<String> = Vec::new();
        let mut untracked: Vec<String> = Vec::new();

        for line in output.lines() {
            if line.starts_with("1 ") || line.starts_with("2 ") {
                let parts: Vec<&str> = line.splitn(9, ' ').collect();
                if parts.len() >= 9 {
                    let xy = parts[1];
                    let path = parts[8];
                    let x = xy.as_bytes().first().copied().unwrap_or(b'.');
                    let y = xy.as_bytes().get(1).copied().unwrap_or(b'.');

                    if x != b'.' && x != b'?' {
                        staged.push(format!("{} {path}", char::from(x)));
                    }
                    if y != b'.' && y != b'?' {
                        modified.push(path.to_string());
                    }
                }
            } else if let Some(rest) = line.strip_prefix("? ") {
                untracked.push(rest.to_string());
            }
        }

        assert_eq!(staged, vec!["M src/main.rs"]);
        assert_eq!(modified, vec!["src/lib.rs"]);
        assert_eq!(untracked, vec!["new_file.txt"]);
    }

    #[tokio::test]
    async fn integration_status_in_repo() {
        let root = std::env::current_dir().unwrap();
        let result = git_status(&root).await;
        assert!(result.success, "git status failed: {}", result.content);
        assert!(result.content.contains("branch:"));
    }

    #[tokio::test]
    async fn integration_log_in_repo() {
        let root = std::env::current_dir().unwrap();
        let args = serde_json::json!({"action": "log", "n": 3});
        let result = git_log(&args, &root).await;
        assert!(result.success, "git log failed: {}", result.content);
        assert!(!result.content.is_empty());
    }

    #[tokio::test]
    async fn integration_diff_stat() {
        let root = std::env::current_dir().unwrap();
        let args = serde_json::json!({"action": "diff", "stat": true, "ref": "HEAD~1"});
        let result = git_diff(&args, &root).await;
        assert!(result.success, "git diff --stat failed: {}", result.content);
    }

    #[tokio::test]
    async fn not_a_repo_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let result = git_status(tmp.path()).await;
        assert!(!result.success);
        assert!(
            result.content.contains("not a git repository")
                || result.content.contains("Not a git repository"),
            "unexpected error: {}",
            result.content
        );
    }
}
