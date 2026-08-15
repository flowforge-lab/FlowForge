//! Structured git tool (#855). Exposes the read queries `status`, `diff`, `log`,
//! and `show` as structured, token-efficient results, plus the local-write
//! actions `branch` and `commit` (#1254). Safety is per-action: the reads are
//! ReadOnly (available in Plan mode); the writes are Write. `min_safety` stays
//! ReadOnly so the tool is still advertised in Plan — the write actions are then
//! refused at invocation time by the Plan×Write=Deny gate, exactly as the github
//! tool handles its mutating actions.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;

use crate::jail::resolve_pathspec_in_root;
use crate::registry::{Safety, Tool, ToolOutcome};

/// Max lines of unified diff output before truncation.
const MAX_DIFF_LINES: usize = 500;
/// Default number of log entries.
const DEFAULT_LOG_LIMIT: u32 = 10;
/// Wall-clock budget for a mutating git command (#1258 review, finding 4).
/// `commit` runs user-controlled `pre-commit`/`commit-msg` hooks and can invoke
/// gpg-agent pinentry; a blocking hook or a tty-less pinentry would otherwise
/// hang the tool forever. `GIT_TERMINAL_PROMPT=0` stops git's own prompts but
/// neither of those. 30s is generous for a local commit yet bounds a hang.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Validate a user-supplied revision before it is handed to git. A revision is
/// never a legitimate option, so reject anything that could be parsed as one:
/// this closes the injection where `ref="--output=<path>"` (or any `-…` flag)
/// would turn a "read-only" query into an arbitrary-file **write** — which, since
/// this tool is ReadOnly and auto-approved in Plan, would run with no gate (#857
/// review). Call sites additionally pass the value after `--end-of-options` so
/// git treats it as a rev even in the unlikely event validation is bypassed.
fn validate_ref(r: &str) -> Result<(), ToolOutcome> {
    if r.is_empty() {
        return Err(ToolOutcome::error("`ref` must not be empty"));
    }
    if r.starts_with('-') {
        return Err(ToolOutcome::error(format!(
            "invalid `ref` {r:?}: a revision must not start with '-' (rejected so it \
             can't be interpreted as a git option such as --output=…)"
        )));
    }
    if r.chars().any(|c| c.is_control()) {
        return Err(ToolOutcome::error(
            "invalid `ref`: contains control characters".to_string(),
        ));
    }
    Ok(())
}

pub struct GitTool;

#[async_trait]
impl Tool for GitTool {
    fn reaches_network(&self) -> bool {
        false
    }
    fn name(&self) -> &str {
        "git"
    }

    fn description(&self) -> &str {
        "Read/write git tool with structured output. Read actions: status \
         (branch + staged/modified/untracked), diff (stat or unified with line \
         cap), log (structured commits), show (single commit) — all ReadOnly, \
         available in Plan mode. Write actions: branch (create + switch to a new \
         branch), commit (stage all changes — or only an explicit `paths` set — \
         and commit) — Write, gated outside \
         Plan. For push/rebase use the github tool or bash."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "The git action to run.",
                    "enum": ["status", "diff", "log", "show", "branch", "commit"]
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
                },
                "name": {
                    "type": "string",
                    "description": "For branch: the new branch name to create and switch to. Fails if it already exists."
                },
                "message": {
                    "type": "string",
                    "description": "For commit: the commit message. Without 'paths', stages all changes (tracked and untracked) before committing."
                },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "For commit (optional): stage only these paths instead of all changes. Omit to stage the entire working tree."
                }
            },
            "required": ["action"]
        })
    }

    /// Per-action parameter attribution for Phase 2B pruning (#1162).
    ///
    /// Read off the dispatch code. Note `status` takes **no** arguments at all —
    /// `git_status(root)` does not even receive `args` — so `git status` prunes
    /// down to the bare `action` discriminant.
    ///
    /// `git_log` reads `n` through a multi-line `.get()` at `:313-317`; a
    /// single-line scan of the source misses it. That is why this map is
    /// hand-verified rather than generated.
    fn action_params(&self) -> Option<BTreeMap<&'static str, &'static [&'static str]>> {
        Some(BTreeMap::from([
            ("status", &[][..]),
            ("diff", &["ref", "staged", "stat", "path"][..]),
            ("log", &["n", "path"][..]),
            ("show", &["ref"][..]),
            ("branch", &["name"][..]),
            ("commit", &["message", "paths"][..]),
        ]))
    }

    /// Per-action safety (#1254). A **read whitelist**: only the four known read
    /// actions are ReadOnly; everything else — the writes, and any future or
    /// unrecognized action — is Write. This fails *closed*, matching the github
    /// and bash tools (a misclassified action is over-gated, never under-gated).
    /// `every_action_has_intentional_safety` pins the whitelist to the action set
    /// so a new read added without updating it is caught (over-gated, but loudly).
    fn safety(&self, args: &Value) -> Safety {
        match args.get("action").and_then(|a| a.as_str()) {
            Some("status" | "diff" | "log" | "show") => Safety::ReadOnly,
            _ => Safety::Write,
        }
    }

    fn max_safety(&self) -> Safety {
        Safety::Write
    }

    /// Stays ReadOnly even though writes exist: this is what keeps the tool
    /// advertised in Plan mode (advertising keys on `min_safety == ReadOnly`).
    /// The write actions are then refused at call time by Plan×Write=Deny.
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
            "branch" => git_branch(&args, root).await,
            "commit" => git_commit(&args, root).await,
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

    // Jail `path` per CONTRIBUTING §3: this is a read-only action, but `root`
    // may be a repo subdirectory, and an unjailed `../` would still disclose
    // history/diff content outside the workspace (#1260).
    let resolved_path = match path {
        Some(p) => match resolve_pathspec_in_root(root, p) {
            Ok(r) => Some(r.to_string_lossy().into_owned()),
            Err(e) => return ToolOutcome::error(e),
        },
        None => None,
    };

    let mut cmd_args: Vec<&str> = vec!["diff"];

    if staged {
        cmd_args.push("--cached");
    }

    if stat {
        cmd_args.push("--numstat");
    }

    if let Some(r) = git_ref {
        if let Err(e) = validate_ref(r) {
            return e;
        }
        // `--end-of-options` forces git to treat the next arg as a revision, not
        // an option, so a value like `--output=…` can't smuggle a write (#857).
        cmd_args.push("--end-of-options");
        cmd_args.push(r);
    }

    cmd_args.push("--");

    if let Some(p) = &resolved_path {
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
    let mut file_count: u32 = 0;

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
            file_count += 1;
        }
    }

    result.push_str(&format!(
        "\nTotal: +{total_added} -{total_removed} in {file_count} file(s)"
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

    // Jail `path` per CONTRIBUTING §3 (#1260). `resolve_pathspec_in_root`
    // permits a path whose parent no longer exists, so `git log -- <deleted
    // path>` — a legitimate history query — keeps working.
    let resolved_path = match path {
        Some(p) => match resolve_pathspec_in_root(root, p) {
            Ok(r) => Some(r.to_string_lossy().into_owned()),
            Err(e) => return ToolOutcome::error(e),
        },
        None => None,
    };

    let n_str = format!("-{n}");
    let fmt_arg = "--format=%H%x00%s%x00%aN%x00%aI";
    let mut cmd_args: Vec<&str> = vec!["log", &n_str, fmt_arg];

    if let Some(p) = &resolved_path {
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
    // A user-supplied commit-ish must not be parseable as a git option (#857).
    if let Err(e) = validate_ref(commit) {
        return e;
    }

    // Get commit metadata. `--end-of-options` guards each use so `commit` is
    // always treated as a revision, never a flag.
    let fmt_arg = "--format=%H%x00%s%x00%aN%x00%aI%x00%b";
    let meta_args = vec!["show", "--no-patch", fmt_arg, "--end-of-options", commit];
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
    let stat_args = vec!["show", "--stat", "--format=", "--end-of-options", commit];
    if let Ok(stat) = run_git(root, &stat_args).await {
        if !stat.trim().is_empty() {
            result.push_str(&format!("\n{}", stat.trim()));
        }
    }

    // Get unified diff (bounded)
    let diff_args = vec!["show", "--format=", "--end-of-options", commit];
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

// ─── branch ──────────────────────────────────────────────────────────────────

pub(crate) async fn git_branch(args: &Value, root: &Path) -> ToolOutcome {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.trim().is_empty() => n.trim(),
        _ => return ToolOutcome::error("branch requires a non-empty 'name'"),
    };
    // `switch -c` creates and switches in one step, failing if the branch already
    // exists (unlike `switch -C`, which would silently reset it).
    match run_git_write(root, &["switch", "-c", name]).await {
        Ok(_) => ToolOutcome::ok(format!("Created and switched to branch '{name}'.")),
        Err(e) => e,
    }
}

// ─── commit ──────────────────────────────────────────────────────────────────

pub(crate) async fn git_commit(args: &Value, root: &Path) -> ToolOutcome {
    let message = match args.get("message").and_then(|v| v.as_str()) {
        Some(m) if !m.trim().is_empty() => m.trim(),
        _ => return ToolOutcome::error("commit requires a non-empty 'message'"),
    };

    // Staging is explicit-or-all (#1255 B): `paths` stages exactly those entries;
    // omitting it falls back to `add -A` (stage the whole working tree). The
    // explicit form lets a caller avoid sweeping in unrelated WIP.
    //
    // Validate each entry rather than silently dropping bad ones (#1262 review):
    // a non-string element or a blank path is a caller mistake — dropping it would
    // stage a different set than asked (e.g. `["a", 123, "b"]` → `["a", "b"]`) with
    // no signal, so reject it explicitly.
    let paths: Vec<String> = match args.get("paths") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for p in arr {
                match p.as_str() {
                    Some(s) if !s.trim().is_empty() => out.push(s.trim().to_string()),
                    Some(_) => {
                        return ToolOutcome::error("git commit 'paths' entries must be non-empty");
                    }
                    None => {
                        return ToolOutcome::error("git commit 'paths' entries must be strings");
                    }
                }
            }
            out
        }
        Some(_) => return ToolOutcome::error("git commit 'paths' must be an array of strings"),
    };

    let stage_result = if paths.is_empty() {
        run_git_write(root, &["add", "-A"]).await
    } else {
        // Jail every entry to `root` (CONTRIBUTING §3): commit is a Write that
        // Auto allows without a prompt, so a `../` traversal must not stage/commit
        // outside the workspace when `root` is a repo subdirectory.
        // resolve_pathspec_in_root returns an absolute, canonical in-root path,
        // safe to pass to `git add`, and — unlike a plain resolve_in_root —
        // still resolves a path whose parent directory was already deleted
        // (staging that deletion by its explicit path; #1260).
        let mut resolved = Vec::with_capacity(paths.len());
        for p in &paths {
            match resolve_pathspec_in_root(root, p) {
                Ok(abs) => resolved.push(abs.to_string_lossy().into_owned()),
                Err(e) => return ToolOutcome::error(e),
            }
        }
        let mut add_args = vec!["add", "--"];
        add_args.extend(resolved.iter().map(String::as_str));
        run_git_write(root, &add_args).await
    };
    if let Err(e) = stage_result {
        return e;
    }

    // Report exactly what was staged so the user/model can see the commit's scope
    // (addresses the "add -A silently sweeps unrelated changes" concern).
    let staged = match run_git_write(root, &["diff", "--cached", "--name-only"]).await {
        Ok(out) => out.lines().map(str::to_string).collect::<Vec<_>>(),
        Err(e) => return e,
    };
    if staged.is_empty() {
        return ToolOutcome::error(
            "nothing to commit: no changes staged (working tree clean or paths matched nothing)",
        );
    }

    match run_git_write(root, &["commit", "-m", message]).await {
        Ok(out) => {
            let summary = out.lines().next().unwrap_or("").trim();
            let files = staged.join("\n  ");
            let header = if summary.is_empty() {
                format!("Committed {} file(s):", staged.len())
            } else {
                format!("Committed {} file(s) — {summary}", staged.len())
            };
            ToolOutcome::ok(format!("{header}\n  {files}"))
        }
        Err(e) => e,
    }
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

/// Strict variant of `run_git` for mutating commands (#1254): any non-zero exit
/// is a real failure. `run_git` deliberately tolerates non-zero exits with output
/// (e.g. `git diff` signalling changes) — correct for reads, but for a write a
/// non-zero exit means the mutation did not happen and must surface as an error.
async fn run_git_write(root: &Path, args: &[&str]) -> Result<String, ToolOutcome> {
    let child = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        // If the timeout fires, the child future is dropped; kill_on_drop then
        // reaps the hung process (blocking hook / pinentry) instead of leaking it.
        .kill_on_drop(true)
        .output();

    let output = match timeout(WRITE_TIMEOUT, child).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(ToolOutcome::error(format!("failed to run git: {e}"))),
        Err(_) => {
            return Err(ToolOutcome::error(format!(
                "git {} timed out after {}s — a pre-commit/commit-msg hook or gpg \
                 pinentry may be blocking. Resolve the hook or disable signing and retry.",
                args.first().unwrap_or(&""),
                WRITE_TIMEOUT.as_secs()
            )));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        return Err(ToolOutcome::error(format!(
            "git {} failed: {detail}",
            args.first().unwrap_or(&"")
        )));
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
    fn safety_is_per_action() {
        let tool = GitTool;
        for read in ["status", "diff", "log", "show"] {
            assert_eq!(
                tool.safety(&serde_json::json!({ "action": read })),
                Safety::ReadOnly,
                "{read} should be ReadOnly"
            );
        }
        for write in ["branch", "commit"] {
            assert_eq!(
                tool.safety(&serde_json::json!({ "action": write })),
                Safety::Write,
                "{write} should be Write"
            );
        }
        // max rises to Write (the tool can mutate), but min stays ReadOnly so the
        // tool is still advertised in Plan — writes are refused at call time.
        assert_eq!(tool.max_safety(), Safety::Write);
        assert_eq!(tool.min_safety(), Safety::ReadOnly);
        // Fail closed: an unrecognized/absent action is Write, not ReadOnly, so a
        // future action can never accidentally slip through as auto-approved.
        assert_eq!(
            tool.safety(&serde_json::json!({ "action": "push" })),
            Safety::Write,
            "unknown action must fail closed to Write"
        );
        assert_eq!(
            tool.safety(&serde_json::json!({})),
            Safety::Write,
            "missing action must fail closed to Write"
        );
    }

    #[test]
    fn every_action_has_intentional_safety() {
        // Binds the safety() read-whitelist to the advertised action *enum* itself
        // (not a hardcoded copy): every action the schema offers must be classified,
        // and the writes must be Write. Add an action to the enum without teaching
        // safety() about it and this fails — a read would be over-gated as Write
        // (loud), which is the drift the test exists to catch.
        let tool = GitTool;
        let writes = ["branch", "commit"];
        let schema = tool.parameters();
        let actions = schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum present in schema");
        assert!(
            !actions.is_empty(),
            "schema must advertise at least one action"
        );
        for action in actions {
            let action = action.as_str().expect("action enum entries are strings");
            let got = tool.safety(&serde_json::json!({ "action": action }));
            let want = if writes.contains(&action) {
                Safety::Write
            } else {
                Safety::ReadOnly
            };
            assert_eq!(
                got, want,
                "{action} classified as {got:?}, expected {want:?}"
            );
        }
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

    /// Bootstraps a hermetic throwaway git repo in a tempdir for the
    /// integration tests below. Returns the tempdir (kept alive for the test's
    /// duration so `.git` isn't deleted mid-test). Replaces the old reliance on
    /// `std::env::current_dir()` being a real repo: that broke under CI's
    /// default shallow checkout (`fetch-depth: 1`), where `log -n 3` had only
    /// one commit and the workspace's own status differed per branch (#1072).
    fn init_temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("LC_ALL", "C")
                .output()
                .expect("git bootstrap command runs");
            assert!(
                out.status.success(),
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr),
            );
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.name", "FF Test"]);
        run(&["config", "user.email", "test@flowforge.local"]);
        std::fs::write(dir.path().join("README.md"), "hello\n").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "--quiet", "-m", "initial commit"]);
        dir
    }

    #[tokio::test]
    async fn integration_status_in_repo() {
        let repo = init_temp_repo();
        let result = git_status(repo.path()).await;
        assert!(result.success, "git status failed: {}", result.content);
        assert!(result.content.contains("branch:"));
    }

    #[tokio::test]
    async fn integration_log_in_repo() {
        let repo = init_temp_repo();
        let args = serde_json::json!({"action": "log", "n": 3});
        let result = git_log(&args, repo.path()).await;
        assert!(result.success, "git log failed: {}", result.content);
        assert!(!result.content.is_empty());
    }

    #[tokio::test]
    async fn integration_diff_stat() {
        let repo = init_temp_repo();
        // Clean-tree `git diff --numstat` exits 0 with empty output; git_diff
        // normalizes that to "No differences." -- success either way.
        let args = serde_json::json!({"action": "diff", "stat": true});
        let result = git_diff(&args, repo.path()).await;
        assert!(result.success, "git diff --stat failed: {}", result.content);
    }

    #[tokio::test]
    async fn integration_branch_creates_and_switches() {
        let repo = init_temp_repo();
        let args = serde_json::json!({"action": "branch", "name": "feature/x"});
        let result = git_branch(&args, repo.path()).await;
        assert!(result.success, "git branch failed: {}", result.content);
        // HEAD is now on the new branch.
        let head = git_status(repo.path()).await;
        assert!(
            head.content.contains("feature/x"),
            "expected HEAD on feature/x, got: {}",
            head.content
        );
    }

    #[tokio::test]
    async fn integration_branch_rejects_existing() {
        let repo = init_temp_repo();
        let args = serde_json::json!({"action": "branch", "name": "dup"});
        assert!(git_branch(&args, repo.path()).await.success);
        // `switch -c dup` fails when `dup` already exists, regardless of the
        // branch currently checked out — so a repeat create is a clean rejection.
        let second = git_branch(&args, repo.path()).await;
        assert!(!second.success, "creating an existing branch should fail");
    }

    #[tokio::test]
    async fn integration_branch_requires_name() {
        let repo = init_temp_repo();
        let result = git_branch(&serde_json::json!({"action": "branch"}), repo.path()).await;
        assert!(!result.success);
        assert!(result.content.contains("name"));
    }

    #[tokio::test]
    async fn integration_commit_stages_all_changes() {
        let repo = init_temp_repo();
        // A tracked edit + a brand-new untracked file: `add -A` must stage both.
        std::fs::write(repo.path().join("README.md"), "hello world\n").unwrap();
        std::fs::write(repo.path().join("new.txt"), "fresh\n").unwrap();
        let args = serde_json::json!({"action": "commit", "message": "capture work"});
        let result = git_commit(&args, repo.path()).await;
        assert!(result.success, "git commit failed: {}", result.content);
        // The outcome names exactly what was staged (both files).
        assert!(
            result.content.contains("README.md") && result.content.contains("new.txt"),
            "commit outcome should list both staged files, got: {}",
            result.content
        );
        // The new commit exists at HEAD with our message (direct, not a negation).
        let log = git_log(&serde_json::json!({"action": "log", "n": 1}), repo.path()).await;
        assert!(log.success);
        assert!(
            log.content.contains("capture work"),
            "HEAD commit should carry the message, got: {}",
            log.content
        );
    }

    #[tokio::test]
    async fn integration_commit_paths_stages_only_listed() {
        let repo = init_temp_repo();
        std::fs::write(repo.path().join("wanted.txt"), "in\n").unwrap();
        std::fs::write(repo.path().join("unrelated.txt"), "out\n").unwrap();
        let args = serde_json::json!({
            "action": "commit",
            "message": "scoped",
            "paths": ["wanted.txt"],
        });
        let result = git_commit(&args, repo.path()).await;
        assert!(result.success, "scoped commit failed: {}", result.content);
        assert!(
            result.content.contains("wanted.txt") && !result.content.contains("unrelated.txt"),
            "only wanted.txt should be staged/reported, got: {}",
            result.content
        );
        // unrelated.txt is still untracked (was never staged).
        let status = git_status(repo.path()).await;
        assert!(
            status.content.contains("unrelated.txt"),
            "unrelated.txt should remain uncommitted, got: {}",
            status.content
        );
    }

    #[tokio::test]
    async fn integration_commit_rejects_empty_stage() {
        let repo = init_temp_repo();
        // Clean tree: nothing to stage, so commit must error rather than run git.
        let result = git_commit(
            &serde_json::json!({"action": "commit", "message": "noop"}),
            repo.path(),
        )
        .await;
        assert!(!result.success);
        assert!(result.content.contains("nothing to commit"));
    }

    #[tokio::test]
    async fn integration_commit_paths_rejects_jail_escape() {
        // CONTRIBUTING §3 jail-escape test. `root` is a subdirectory of the repo;
        // a `../` path points at a real file inside the repo but *outside* root.
        // The jail must reject it before anything is staged or committed.
        let repo = init_temp_repo();
        std::fs::write(repo.path().join("secret.txt"), "outside\n").unwrap();
        let sub = repo.path().join("workspace");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("mine.txt"), "inside\n").unwrap();

        let result = git_commit(
            &serde_json::json!({
                "action": "commit",
                "message": "escape",
                "paths": ["../secret.txt"],
            }),
            &sub,
        )
        .await;
        assert!(
            !result.success,
            "a ../ path must be rejected, got: {}",
            result.content
        );
        assert!(
            result.content.contains("outside the workspace root"),
            "error should name the jail violation, got: {}",
            result.content
        );
        // And nothing was committed: HEAD is still the fixture's initial commit.
        let log = git_log(&serde_json::json!({"action": "log", "n": 5}), &sub).await;
        assert!(
            !log.content.contains("escape"),
            "the rejected commit must not exist, got: {}",
            log.content
        );
    }

    #[tokio::test]
    async fn integration_commit_paths_allows_deleted_directory_path() {
        // #1260: staging a deletion by its explicit nested path must work even
        // after the containing directory is gone — not just by passing the
        // directory itself (the workaround the issue was filed to remove).
        let repo = init_temp_repo();
        std::fs::create_dir(repo.path().join("olddir")).unwrap();
        std::fs::write(repo.path().join("olddir/a.txt"), "x\n").unwrap();
        let add = git_commit(
            &serde_json::json!({"message": "add", "paths": ["olddir/a.txt"]}),
            repo.path(),
        )
        .await;
        assert!(add.success, "{}", add.content);

        std::fs::remove_dir_all(repo.path().join("olddir")).unwrap();
        let remove = git_commit(
            &serde_json::json!({"message": "remove", "paths": ["olddir/a.txt"]}),
            repo.path(),
        )
        .await;
        assert!(
            remove.success,
            "committing a deletion by its explicit (now-nonexistent) path should \
             succeed, got: {}",
            remove.content
        );
    }

    #[tokio::test]
    async fn integration_diff_path_rejects_jail_escape() {
        // CONTRIBUTING §3 jail-escape test for the read side (#1260): `diff`'s
        // `path` must be jailed to `root`, not passed straight to git.
        let repo = init_temp_repo();
        std::fs::write(repo.path().join("secret.txt"), "outside\n").unwrap();
        let sub = repo.path().join("workspace");
        std::fs::create_dir(&sub).unwrap();

        let result = git_diff(
            &serde_json::json!({"action": "diff", "path": "../secret.txt"}),
            &sub,
        )
        .await;
        assert!(
            !result.success,
            "a ../ path must be rejected, got: {}",
            result.content
        );
        assert!(
            result.content.contains("outside the workspace root"),
            "error should name the jail violation, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn integration_log_path_rejects_jail_escape() {
        let repo = init_temp_repo();
        let sub = repo.path().join("workspace");
        std::fs::create_dir(&sub).unwrap();

        let result = git_log(
            &serde_json::json!({"action": "log", "path": "../README.md"}),
            &sub,
        )
        .await;
        assert!(
            !result.success,
            "a ../ path must be rejected, got: {}",
            result.content
        );
        assert!(
            result.content.contains("outside the workspace root"),
            "error should name the jail violation, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn integration_log_path_finds_deleted_file_history() {
        // Regression guard (#1260): `git log -- <path>` for a file that has
        // since been deleted is a legitimate query and must keep working
        // through the jail, not just for still-existing files.
        let repo = init_temp_repo();
        std::fs::create_dir(repo.path().join("gone")).unwrap();
        std::fs::write(repo.path().join("gone/file.txt"), "x\n").unwrap();
        git_commit(
            &serde_json::json!({"message": "add gone/file.txt", "paths": ["gone/file.txt"]}),
            repo.path(),
        )
        .await;
        std::fs::remove_file(repo.path().join("gone/file.txt")).unwrap();
        git_commit(
            &serde_json::json!({"message": "remove gone/file.txt", "paths": ["gone/file.txt"]}),
            repo.path(),
        )
        .await;

        let result = git_log(
            &serde_json::json!({"action": "log", "path": "gone/file.txt", "n": 5}),
            repo.path(),
        )
        .await;
        assert!(result.success, "{}", result.content);
        assert!(
            result.content.contains("add gone/file.txt")
                && result.content.contains("remove gone/file.txt"),
            "history for a deleted path should still be found, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn integration_diff_path_rejects_colon_magic() {
        // Documents the #1260 design decision: colon-magic pathspecs are
        // rejected outright rather than parsed.
        let repo = init_temp_repo();
        let result = git_diff(
            &serde_json::json!({"action": "diff", "path": ":(exclude)README.md"}),
            repo.path(),
        )
        .await;
        assert!(!result.success);
        assert!(result.content.contains("magic"), "{}", result.content);
    }

    #[tokio::test]
    async fn integration_commit_requires_message() {
        let repo = init_temp_repo();
        let result = git_commit(&serde_json::json!({"action": "commit"}), repo.path()).await;
        assert!(!result.success);
        assert!(result.content.contains("message"));
    }

    #[tokio::test]
    async fn integration_commit_paths_rejects_non_string_entry() {
        // #1262 review: a non-string paths entry must be rejected, not silently
        // dropped — dropping it would stage a different set than the caller asked.
        let repo = init_temp_repo();
        std::fs::write(repo.path().join("a.txt"), "x\n").unwrap();
        let result = git_commit(
            &serde_json::json!({"message": "m", "paths": ["a.txt", 123]}),
            repo.path(),
        )
        .await;
        assert!(!result.success);
        assert!(
            result.content.contains("must be strings"),
            "got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn integration_commit_paths_rejects_blank_entry() {
        let repo = init_temp_repo();
        std::fs::write(repo.path().join("a.txt"), "x\n").unwrap();
        let result = git_commit(
            &serde_json::json!({"message": "m", "paths": ["a.txt", "   "]}),
            repo.path(),
        )
        .await;
        assert!(!result.success);
        assert!(
            result.content.contains("must be non-empty"),
            "got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn not_a_repo_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        // When TMPDIR is inside a git repo (e.g. a workspace sub-directory),
        // `git` walks up and finds the parent `.git`, causing this test to
        // falsely pass. A `.git` file pointing to a non-existent repo blocks
        // the walk-up without making the tempdir a valid repository.
        std::fs::write(tmp.path().join(".git"), "gitdir: /nonexistent/.git\n").unwrap();
        let result = git_status(tmp.path()).await;
        assert!(!result.success);
        assert!(
            result.content.contains("not a git repository")
                || result.content.contains("Not a git repository"),
            "unexpected error: {}",
            result.content
        );
    }

    // ─── #857 review: argument-injection guard ─────────────────────────────────

    #[test]
    fn validate_ref_rejects_option_like_and_control() {
        // The exploit: `ref` starting with '-' could be parsed as a git option
        // (e.g. `--output=<path>`), turning a read-only query into a file write.
        assert!(validate_ref("--output=/tmp/pwned").is_err());
        assert!(validate_ref("-x").is_err());
        assert!(validate_ref("--upload-pack=touch /tmp/x").is_err());
        assert!(validate_ref("bad\nref").is_err()); // control char
        assert!(validate_ref("").is_err());
        // Legitimate revisions pass.
        assert!(validate_ref("HEAD").is_ok());
        assert!(validate_ref("HEAD~3").is_ok());
        assert!(validate_ref("main").is_ok());
        assert!(validate_ref("a1b2c3d").is_ok());
        assert!(validate_ref("origin/main").is_ok());
        assert!(validate_ref("v1.2.3").is_ok());
    }

    #[tokio::test]
    async fn diff_rejects_option_injection_in_ref() {
        // `validate_ref` rejects the option-like ref before git ever runs, so
        // the repo need not be real -- a tempdir is enough. Drop the old
        // `current_dir()` dependence so the test is hermetic (#1072).
        let dir = tempfile::tempdir().unwrap();
        let args = serde_json::json!({"action": "diff", "ref": "--output=/tmp/ff_git_pwn"});
        let result = git_diff(&args, dir.path()).await;
        assert!(!result.success, "option-like ref must be rejected");
        assert!(result.content.contains("must not start with '-'"));
    }

    #[tokio::test]
    async fn show_rejects_option_injection_and_writes_nothing() {
        // End-to-end proof the exploit is dead: try to make `git show` write a
        // file via --output; assert it's rejected AND no file appears.
        // `validate_ref` short-circuits before git runs, so a tempdir suffices.
        // The marker lives under the tempdir (per-test) instead of the old
        // machine-global `std::env::temp_dir()` path, so parallel test
        // processes can't race on it under nextest (#1072).
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ff_git_show_pwn_857");
        let _ = std::fs::remove_file(&marker);
        let args = serde_json::json!({
            "action": "show",
            "ref": format!("--output={}", marker.display()),
        });
        let result = git_show(&args, dir.path()).await;
        assert!(!result.success, "option-like ref must be rejected");
        assert!(
            !marker.exists(),
            "the rejected injection must not have written a file"
        );
    }

    #[test]
    fn action_params_coherent_with_schema() {
        // RFC 0024 Phase 2B (#1162): adding an action to the enum without
        // declaring its parameters fails here.
        crate::registry::assert_action_params_coherent(&GitTool);
    }

    #[test]
    fn action_params_cover_known_dispatch_reads() {
        // Closes what the orphan check cannot see: `ref` and `path` are each
        // claimed by two actions, so dropping one still leaves the property
        // claimed and the coherence check silent.
        let declared = GitTool.action_params().expect("git declares action_params");
        let required: &[(&str, &str)] = &[
            // git_show reads `ref` at :356.
            ("show", "ref"),
            // git_diff reads all four; `staged` at :224.
            ("diff", "ref"),
            ("diff", "staged"),
            ("diff", "stat"),
            ("diff", "path"),
            // git_log reads `n` via a multi-line .get() at :313-317.
            ("log", "n"),
            ("log", "path"),
            // git_branch reads `name`; git_commit reads `message` (#1254).
            ("branch", "name"),
            ("commit", "message"),
        ];
        for (action, param) in required {
            let params = declared
                .get(action)
                .unwrap_or_else(|| panic!("action {action:?} missing from action_params"));
            assert!(
                params.contains(param),
                "action {action:?} reads {param:?} in its dispatch path but does not declare it"
            );
        }
        // git_status(root) never receives `args`, so it must claim nothing.
        assert!(
            declared.get("status").expect("status declared").is_empty(),
            "git status takes no arguments; declaring any would keep dead properties in its schema"
        );
    }
}
