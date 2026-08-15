//! Structured GitHub operations via the `gh` CLI (#734). Wraps `gh` with JSON
//! output parsing so the agent gets clean, structured PR/issue/CI data without
//! shell escaping or token management headaches.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;

use crate::registry::{Safety, Tool, ToolOutcome};

pub struct GithubTool;

#[async_trait]
impl Tool for GithubTool {
    fn name(&self) -> &str {
        "github"
    }

    fn description(&self) -> &str {
        "Interact with GitHub: create/list/merge PRs, check CI status, create/edit/list issues, \
         and push branches. Uses the `gh` CLI under the hood with structured JSON output."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "The operation to perform.",
                    "enum": ["pr_create", "pr_list", "pr_view", "pr_reviews", "pr_review_comments", "pr_merge", "pr_checks", "pr_review", "pr_comment", "pr_request_review", "pr_review_inline", "issue_create", "issue_edit", "issue_list", "issue_view", "issue_comment", "push"]
                },
                "title": { "type": "string", "description": "Title for PR or issue (pr_create, issue_create, issue_edit)." },
                "body": { "type": "string", "description": "Body text for a PR/issue or a review/comment (pr_create, issue_create, issue_edit, pr_review, pr_comment, issue_comment, pr_review_inline). Required for pr_review / pr_review_inline when event is COMMENT or REQUEST_CHANGES (GitHub 422s a bodiless one); optional for APPROVE. Markdown supported." },
                "base": { "type": "string", "description": "Base branch for PR (pr_create). Defaults to 'main'." },
                "head": { "type": "string", "description": "Head branch for PR (pr_create). Defaults to current branch." },
                "draft": { "type": "boolean", "description": "Open the PR as a draft (pr_create). Defaults to false." },
                "number": { "type": "integer", "description": "PR or issue number (pr_view, pr_reviews, pr_review_comments, pr_merge, pr_checks, pr_review, pr_comment, pr_request_review, pr_review_inline, issue_view, issue_edit, issue_comment)." },
                "event": { "type": "string", "enum": ["APPROVE", "REQUEST_CHANGES", "COMMENT"], "description": "Review verdict for pr_review / pr_review_inline. Note: APPROVE and REQUEST_CHANGES are rejected on your own PR (422) — use COMMENT for a self-review." },
                "comments": { "type": "array", "description": "Inline review comments for pr_review_inline. Each anchors to a diff line.", "items": { "type": "object", "properties": { "path": { "type": "string", "description": "File path (repo-relative)." }, "line": { "type": "integer", "description": "Line number in the file's diff." }, "side": { "type": "string", "enum": ["LEFT", "RIGHT"], "description": "Diff side. Defaults to RIGHT." }, "start_line": { "type": "integer", "description": "Start line for a multi-line comment (optional)." }, "body": { "type": "string", "description": "Comment text." } }, "required": ["path", "line", "body"] } },
                "squash": { "type": "boolean", "description": "Squash merge (pr_merge). Defaults to true." },
                "label": { "type": ["string", "array"], "items": { "type": "string" }, "description": "Label(s) to filter by or assign — a single string or an array. On issue_create/pr_create each is applied; on issue_edit each is added (--add-label); on issue_list/pr_list used as a filter." },
                "assignee": { "type": ["string", "array"], "items": { "type": "string" }, "description": "GitHub username(s) to assign — a single string or an array (issue_create, pr_create; added on issue_edit)." },
                "reviewer": { "type": ["string", "array"], "items": { "type": "string" }, "description": "Reviewer username(s) to request — a single string or an array (pr_create, pr_request_review)." },
                "author": { "type": "string", "description": "Filter by author (pr_list). Use '@me' for self." },
                "limit": { "type": "integer", "description": "Max results to return (pr_list, issue_list). Defaults to 10." },
                "force": { "type": "boolean", "description": "Force push (push). Defaults to false." },
                "diff": { "type": "boolean", "description": "For pr_view: return the raw unified diff (gh pr diff) instead of the PR metadata + body. Defaults to false." },
                "delete_branch": { "type": "boolean", "description": "Delete head branch after merge (pr_merge). Defaults to true." }
            },
            "required": ["action"]
        })
    }

    /// Per-action parameter attribution for Phase 2B pruning (#1162).
    ///
    /// Every entry was read off the dispatch code, following forwards
    /// (`pr_comment`/`issue_comment` → `comment_on`, `pr_create`/`issue_create`
    /// → `create_flag_args`, `pr_review_inline` → `build_inline_review_payload`)
    /// — **not** copied from the property descriptions above, which disagree with
    /// the code in four places (#1161).
    ///
    /// Only top-level argument keys belong here. `build_inline_review_payload`
    /// also reads `path`/`line`/`side`/`start_line`, but those are fields of the
    /// `comments` array elements, not properties of this schema.
    fn action_params(&self) -> Option<BTreeMap<&'static str, &'static [&'static str]>> {
        Some(BTreeMap::from([
            (
                "pr_create",
                &[
                    "title", "body", "base", "head", "label", "assignee", "reviewer", "draft",
                ][..],
            ),
            ("pr_list", &["author", "label", "limit"][..]),
            ("pr_view", &["number", "diff"][..]),
            ("pr_reviews", &["number"][..]),
            ("pr_review_comments", &["number"][..]),
            ("pr_merge", &["number", "squash", "delete_branch"][..]),
            ("pr_checks", &["number"][..]),
            ("pr_review", &["number", "event", "body"][..]),
            ("pr_comment", &["number", "body"][..]),
            ("pr_request_review", &["number", "reviewer"][..]),
            (
                "pr_review_inline",
                &["number", "event", "body", "comments"][..],
            ),
            ("issue_create", &["title", "body", "label", "assignee"][..]),
            (
                "issue_edit",
                &["number", "title", "body", "label", "assignee"][..],
            ),
            ("issue_list", &["label", "limit"][..]),
            ("issue_view", &["number"][..]),
            ("issue_comment", &["number", "body"][..]),
            ("push", &["force"][..]),
        ]))
    }

    fn safety(&self, args: &Value) -> Safety {
        match args.get("action").and_then(|a| a.as_str()) {
            // pr_reviews / pr_review_comments (#853) are pure reads — same
            // class as pr_view / issue_view — so they inherit Plan-mode
            // availability from #846 without a gating change.
            Some(
                "pr_list" | "pr_view" | "pr_reviews" | "pr_review_comments" | "pr_checks"
                | "issue_list" | "issue_view",
            ) => Safety::ReadOnly,
            // Remote-publishing mutations: creating/merging a PR and pushing a
            // branch write to the remote repo, so they carry the Publish tier
            // (Plan denies, Auto prompts, Act allows) rather than plain Write
            // (#1051). Chatty review/issue writes stay Write so Auto stays
            // usable for them.
            Some("pr_create" | "pr_merge" | "push") => Safety::Publish,
            _ => Safety::Write,
        }
    }

    fn max_safety(&self) -> Safety {
        Safety::Publish
    }

    // Read-only floor: the list/read actions (`pr_list`, `pr_view`, `pr_checks`,
    // `issue_list`, `issue_view`) are `ReadOnly`, so gh is advertised in Plan; the
    // per-call `safety` gate rejects the mutating actions there (Plan x Write = Deny).
    fn min_safety(&self) -> Safety {
        Safety::ReadOnly
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        let action = match args.get("action").and_then(|a| a.as_str()) {
            Some(a) => a,
            None => return ToolOutcome::error("missing required parameter: action"),
        };

        match action {
            "pr_create" => pr_create(&args, root).await,
            "pr_list" => pr_list(&args, root).await,
            "pr_view" => pr_view(&args, root).await,
            "pr_reviews" => pr_reviews(&args, root).await,
            "pr_review_comments" => pr_review_comments(&args, root).await,
            "pr_merge" => pr_merge(&args, root).await,
            "pr_checks" => pr_checks(&args, root).await,
            "pr_review" => pr_review(&args, root).await,
            "pr_comment" => pr_comment(&args, root).await,
            "pr_request_review" => pr_request_review(&args, root).await,
            "pr_review_inline" => pr_review_inline(&args, root).await,
            "issue_create" => issue_create(&args, root).await,
            "issue_edit" => issue_edit(&args, root).await,
            "issue_list" => issue_list(&args, root).await,
            "issue_view" => issue_view(&args, root).await,
            "issue_comment" => issue_comment(&args, root).await,
            "push" => push(&args, root).await,
            _ => ToolOutcome::error(format!("unknown action: {action}")),
        }
    }
}

/// Resolve the GitHub token once per process, then cache it.
///
/// Resolution order (first non-empty wins):
/// 1. `GH_TOKEN` environment variable (CI / scripted usage).
/// 2. `gh auth token` — the `gh` CLI's own credential store. Works for users
///    who ran `gh auth login`; respects whatever auth method they configured.
/// 3. Credential file at `~/.config/flowforge/gh_token` (the XDG-style path
///    where FlowForge credentials live on all platforms).
///
/// Cached in a `OnceLock` so the ~50ms subprocess spawn only happens once.
fn resolve_token() -> Option<String> {
    static TOKEN: OnceLock<Option<String>> = OnceLock::new();
    TOKEN
        .get_or_init(|| {
            // 1. Env var (always wins — lets CI / power users override).
            if let Ok(tok) = std::env::var("GH_TOKEN") {
                if !tok.is_empty() {
                    return Some(tok);
                }
            }
            // 2. gh's own credential store (general, zero-config if authed).
            if let Some(tok) = gh_auth_token() {
                return Some(tok);
            }
            // 3. Credential file (~/.config/flowforge/gh_token).
            if let Some(home) = dirs::home_dir() {
                let path = home.join(".config/flowforge/gh_token");
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    let tok = contents.trim().to_string();
                    if !tok.is_empty() {
                        return Some(tok);
                    }
                }
            }
            None
        })
        .clone()
}

/// How long we will wait for `gh auth token` before giving up. Token
/// resolution runs on the async tool path, so a wedged `gh` must never be able
/// to stall a turn; five seconds is far beyond the ~50ms a healthy `gh` takes.
const GH_AUTH_TIMEOUT: Duration = Duration::from_secs(5);

/// Ask `gh auth token` for the active credential. Returns `None` if `gh` is
/// not installed, not authenticated, returns an error, or does not answer
/// within [`GH_AUTH_TIMEOUT`].
///
/// This does not use `Command::output()`, which waits for the stdout pipe to
/// reach EOF rather than for the child to exit. `gh` forks a background update
/// notifier that inherits that pipe and can outlive the command, so on Windows
/// the read blocks indefinitely even though `gh` itself is long gone — that is
/// what timed out CI's `ff-tools` run at 120s. The notifier is disabled below
/// *and* the read is bounded, because "no unbounded wait on a child process"
/// should hold whatever `gh` decides to spawn next.
fn gh_auth_token() -> Option<String> {
    let mut child = std::process::Command::new("gh")
        .args(["auth", "token"])
        .env("PATH", ff_core::augmented_path())
        // The update notifier is the known pipe-holder; the prompt disable
        // keeps a misconfigured `gh` from blocking on a TTY that isn't there.
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    // Read on a worker thread. If something really is holding the pipe open,
    // that thread parks forever — but the caller walks away on the deadline
    // instead of parking with it.
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let read = std::io::Read::read_to_string(&mut stdout, &mut buf);
        let _ = tx.send(read.ok().map(|_| buf));
    });

    let stdout = match rx.recv_timeout(GH_AUTH_TIMEOUT) {
        Ok(Some(buf)) => buf,
        // Read error, or the sender vanished.
        Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => return None,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };

    // stdout hit EOF, so the child has closed it and this cannot block long.
    if !child.wait().ok()?.success() {
        return None;
    }
    let tok = stdout.trim();
    (!tok.is_empty()).then(|| tok.to_string())
}

/// Build a `gh` command with token and working directory set.
fn gh_cmd(root: &Path) -> Command {
    let mut cmd = Command::new("gh");
    if let Some(token) = resolve_token() {
        cmd.env("GH_TOKEN", token);
    }
    cmd.current_dir(root)
        .env("PATH", ff_core::augmented_path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Run a command and return (stdout, stderr, success).
async fn run_gh(mut cmd: Command) -> Result<String, String> {
    let output = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn gh: {e} (is gh installed?)"))?
        .wait_with_output()
        .await
        .map_err(|e| format!("gh command failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        Ok(stdout)
    } else {
        Err(stderr.trim().to_string())
    }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

async fn pr_create(args: &Value, root: &Path) -> ToolOutcome {
    let title = match args.get("title").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return ToolOutcome::error("pr_create requires 'title'"),
    };
    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let base = args.get("base").and_then(|v| v.as_str()).unwrap_or("main");

    let mut cmd = gh_cmd(root);
    cmd.args([
        "pr", "create", "--title", title, "--body", body, "--base", base,
    ]);

    if let Some(head) = args.get("head").and_then(|v| v.as_str()) {
        cmd.args(["--head", head]);
    }
    // `--draft` is PR-only: `gh issue create` has no draft flag, so it is applied
    // here rather than in the shared create_flag_args (which issue_create reuses).
    if args.get("draft").and_then(|v| v.as_bool()).unwrap_or(false) {
        cmd.arg("--draft");
    }
    cmd.args(create_flag_args(args, true));

    match run_gh(cmd).await {
        Ok(url) => ToolOutcome::ok(format!("PR created: {}", url.trim())),
        Err(e) => ToolOutcome::error(format!("pr_create failed: {e}")),
    }
}

async fn pr_list(args: &Value, root: &Path) -> ToolOutcome {
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10);
    let mut cmd = gh_cmd(root);
    cmd.args([
        "pr",
        "list",
        "--json",
        "number,title,headRefName,state,createdAt,author",
        "--limit",
        &limit.to_string(),
    ]);

    cmd.args(pr_list_flags(args));

    match run_gh(cmd).await {
        Ok(json) => format_json_table(&json, &["number", "title", "headRefName", "state"]),
        Err(e) => ToolOutcome::error(format!("pr_list failed: {e}")),
    }
}

async fn pr_merge(args: &Value, root: &Path) -> ToolOutcome {
    let number = match args.get("number").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => return ToolOutcome::error("pr_merge requires 'number'"),
    };
    let squash = args.get("squash").and_then(|v| v.as_bool()).unwrap_or(true);

    let delete_branch = args
        .get("delete_branch")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let mut cmd = gh_cmd(root);
    cmd.args(["pr", "merge", &number.to_string()]);
    if delete_branch {
        cmd.arg("--delete-branch");
    }
    if squash {
        cmd.arg("--squash");
    }

    match run_gh(cmd).await {
        Ok(out) => ToolOutcome::ok(format!("PR #{number} merged. {}", out.trim())),
        Err(e) => ToolOutcome::error(format!("pr_merge failed: {e}")),
    }
}

async fn pr_checks(args: &Value, root: &Path) -> ToolOutcome {
    let number = match args.get("number").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => return ToolOutcome::error("pr_checks requires 'number'"),
    };

    let mut cmd = gh_cmd(root);
    cmd.args(["pr", "checks", &number.to_string(), "--json", "name,state"]);

    match run_gh(cmd).await {
        Ok(json) => {
            let checks: Vec<Value> = serde_json::from_str(&json).unwrap_or_default();
            if checks.is_empty() {
                return ToolOutcome::ok(format!("PR #{number}: no checks found."));
            }
            let mut lines = vec![format!("PR #{number} checks:")];
            for check in &checks {
                let name = check.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let state = check.get("state").and_then(|v| v.as_str()).unwrap_or("?");
                let icon = match state {
                    "SUCCESS" | "success" => "✓",
                    "FAILURE" | "failure" => "✗",
                    "PENDING" | "pending" | "IN_PROGRESS" => "◯",
                    _ => "?",
                };
                lines.push(format!("  {icon} {name}: {state}"));
            }
            ToolOutcome::ok(lines.join("\n"))
        }
        Err(e) => ToolOutcome::error(format!("pr_checks failed: {e}")),
    }
}

/// Submit a review verdict on a PR: `gh pr review <n> --approve|--request-changes|--comment [--body]`.
async fn pr_review(args: &Value, root: &Path) -> ToolOutcome {
    let number = match args.get("number").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => return ToolOutcome::error("pr_review requires 'number'"),
    };
    let event = args
        .get("event")
        .and_then(|v| v.as_str())
        .unwrap_or("COMMENT")
        .to_uppercase();
    let flag = match event.as_str() {
        "APPROVE" => "--approve",
        "REQUEST_CHANGES" => "--request-changes",
        "COMMENT" => "--comment",
        other => {
            return ToolOutcome::error(format!(
                "pr_review 'event' must be APPROVE, REQUEST_CHANGES, or COMMENT (got {other})"
            ))
        }
    };
    let body = args.get("body").and_then(|v| v.as_str());
    // COMMENT and REQUEST_CHANGES both require a body: GitHub's API 422s a bodiless
    // COMMENT/REQUEST_CHANGES review, and non-interactive `gh pr review
    // --request-changes` errors without `--body`. APPROVE may omit it.
    if matches!(flag, "--comment" | "--request-changes")
        && body.map(str::trim).unwrap_or("").is_empty()
    {
        return ToolOutcome::error(format!(
            "pr_review with event={event} requires a non-empty 'body'"
        ));
    }

    let mut cmd = gh_cmd(root);
    cmd.args(["pr", "review", &number.to_string(), flag]);
    if let Some(b) = body {
        cmd.args(["--body", b]);
    }
    match run_gh(cmd).await {
        Ok(out) => ToolOutcome::ok(format!(
            "Review ({event}) submitted on PR #{number}. {}",
            out.trim()
        )),
        Err(e) => ToolOutcome::error(format!("pr_review failed: {e}")),
    }
}

/// Post a top-level comment on a PR: `gh pr comment <n> --body`.
async fn pr_comment(args: &Value, root: &Path) -> ToolOutcome {
    comment_on("pr", args, root).await
}

/// Post a top-level comment on an issue: `gh issue comment <n> --body`.
async fn issue_comment(args: &Value, root: &Path) -> ToolOutcome {
    comment_on("issue", args, root).await
}

/// Shared body for `pr_comment` / `issue_comment` (only the `gh` subcommand differs).
async fn comment_on(kind: &str, args: &Value, root: &Path) -> ToolOutcome {
    let number = match args.get("number").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => return ToolOutcome::error(format!("{kind}_comment requires 'number'")),
    };
    let body = match args.get("body").and_then(|v| v.as_str()) {
        Some(b) if !b.trim().is_empty() => b,
        _ => return ToolOutcome::error(format!("{kind}_comment requires a non-empty 'body'")),
    };
    let mut cmd = gh_cmd(root);
    cmd.args([kind, "comment", &number.to_string(), "--body", body]);
    match run_gh(cmd).await {
        Ok(out) => ToolOutcome::ok(format!(
            "Comment posted on {kind} #{number}. {}",
            out.trim()
        )),
        Err(e) => ToolOutcome::error(format!("{kind}_comment failed: {e}")),
    }
}

/// Request review on an existing PR: `gh pr edit <n> --add-reviewer a,b`.
/// Distinct from `pr_create --reviewer`, which requests at creation time.
async fn pr_request_review(args: &Value, root: &Path) -> ToolOutcome {
    let number = match args.get("number").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => return ToolOutcome::error("pr_request_review requires 'number'"),
    };
    let flags = match pr_request_review_flags(args) {
        Ok(f) => f,
        Err(e) => return ToolOutcome::error(e),
    };
    let mut cmd = gh_cmd(root);
    cmd.args(["pr", "edit", &number.to_string()]);
    cmd.args(flags);
    match run_gh(cmd).await {
        Ok(out) => ToolOutcome::ok(format!(
            "Requested review from {} on PR #{number}. {}",
            str_or_list(args, "reviewer").join(", "),
            out.trim()
        )),
        Err(e) => ToolOutcome::error(format!("pr_request_review failed: {e}")),
    }
}

/// Submit a review carrying inline comments. `gh` has no porcelain for inline
/// review comments, so wrap `gh api POST repos/{owner}/{repo}/pulls/<n>/reviews`
/// with a JSON payload passed via `--input` (a temp file — arrays of objects
/// don't compose safely with `-f`).
async fn pr_review_inline(args: &Value, root: &Path) -> ToolOutcome {
    let number = match args.get("number").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => return ToolOutcome::error("pr_review_inline requires 'number'"),
    };
    let payload = match build_inline_review_payload(args) {
        Ok(p) => p,
        Err(e) => return ToolOutcome::error(e),
    };

    // A NamedTempFile creates the payload with a randomized name via O_EXCL, so it
    // can't follow a pre-planted symlink at a predictable path, and it is removed on
    // drop (including the early-return error paths below).
    use std::io::Write;
    let mut tmp = match tempfile::Builder::new()
        .prefix(&format!("ff-gh-review-{number}-"))
        .suffix(".json")
        .tempfile()
    {
        Ok(f) => f,
        Err(e) => {
            return ToolOutcome::error(format!("pr_review_inline: could not stage payload: {e}"))
        }
    };
    if let Err(e) = tmp.write_all(payload.to_string().as_bytes()) {
        return ToolOutcome::error(format!("pr_review_inline: could not stage payload: {e}"));
    }

    let mut cmd = gh_cmd(root);
    cmd.args([
        "api",
        "-X",
        "POST",
        &format!("repos/{{owner}}/{{repo}}/pulls/{number}/reviews"),
        "--input",
    ]);
    cmd.arg(tmp.path());

    let result = run_gh(cmd).await;
    drop(tmp);
    match result {
        Ok(out) => ToolOutcome::ok(format!(
            "Inline review submitted on PR #{number}. {}",
            out.trim()
        )),
        Err(e) => ToolOutcome::error(format!("pr_review_inline failed: {e}")),
    }
}

/// Build the `POST /pulls/{n}/reviews` JSON body from the tool args: an `event`,
/// an optional review `body`, and a `comments` array of `{path, line, side, body}`
/// (with optional `start_line`). Pure so it can be unit-tested.
fn build_inline_review_payload(args: &Value) -> Result<Value, String> {
    let event = args
        .get("event")
        .and_then(|v| v.as_str())
        .unwrap_or("COMMENT")
        .to_uppercase();
    if !matches!(event.as_str(), "APPROVE" | "REQUEST_CHANGES" | "COMMENT") {
        return Err(format!(
            "pr_review_inline 'event' must be APPROVE, REQUEST_CHANGES, or COMMENT (got {event})"
        ));
    }
    let raw = args
        .get("comments")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or("pr_review_inline requires a non-empty 'comments' array")?;

    let mut comments = Vec::with_capacity(raw.len());
    for (i, c) in raw.iter().enumerate() {
        let path = c
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("comments[{i}] missing 'path'"))?;
        let line = c
            .get("line")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| format!("comments[{i}] missing integer 'line'"))?;
        let body = c
            .get("body")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| format!("comments[{i}] missing 'body'"))?;
        let side = c.get("side").and_then(|v| v.as_str()).unwrap_or("RIGHT");
        let mut obj = serde_json::json!({
            "path": path, "line": line, "side": side, "body": body,
        });
        if let Some(start) = c.get("start_line").and_then(|v| v.as_u64()) {
            obj["start_line"] = serde_json::json!(start);
        }
        comments.push(obj);
    }

    let body = args
        .get("body")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    // A top-level review body is mandatory for COMMENT / REQUEST_CHANGES (GitHub
    // 422s otherwise) and optional for APPROVE. Mirrors the `pr_review` guard so
    // the two review paths behave the same.
    if body.is_none() && matches!(event.as_str(), "COMMENT" | "REQUEST_CHANGES") {
        return Err(format!(
            "pr_review_inline with event={event} requires a non-empty 'body'"
        ));
    }

    let mut payload = serde_json::json!({ "event": event, "comments": comments });
    if let Some(b) = body {
        payload["body"] = serde_json::json!(b);
    }
    Ok(payload)
}

/// Read all reviews submitted on a PR (`gh api .../pulls/<n>/reviews`).
/// ReadOnly, usable in Plan mode (#853) — closes the gap where reading
/// existing review feedback required a `bash gh api` fallback.
async fn pr_reviews(args: &Value, root: &Path) -> ToolOutcome {
    let number = match args.get("number").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => return ToolOutcome::error("pr_reviews requires 'number'"),
    };

    let mut cmd = gh_cmd(root);
    cmd.args([
        "api",
        &format!("repos/{{owner}}/{{repo}}/pulls/{number}/reviews"),
    ]);

    match run_gh(cmd).await {
        Ok(json) => {
            let reviews: Vec<Value> = serde_json::from_str(&json).unwrap_or_default();
            ToolOutcome::ok(render_reviews(number, &reviews))
        }
        Err(e) => ToolOutcome::error(format!("pr_reviews failed: {e}")),
    }
}

/// Read inline review comments on a PR (`gh api .../pulls/<n>/comments`),
/// grouped by file then by reply thread. ReadOnly, usable in Plan mode
/// (#853) — closes the gap where reading the comment thread an agent needs
/// to respond to required a `bash gh api` fallback.
async fn pr_review_comments(args: &Value, root: &Path) -> ToolOutcome {
    let number = match args.get("number").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => return ToolOutcome::error("pr_review_comments requires 'number'"),
    };

    let mut cmd = gh_cmd(root);
    cmd.args([
        "api",
        &format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments"),
        "--paginate",
    ]);

    match run_gh(cmd).await {
        Ok(json) => {
            // `--paginate` returns one JSON array per page separated by
            // newlines; flatten to a single Vec before grouping.
            let mut comments: Vec<Value> = Vec::new();
            for page in json.split('\n') {
                let trimmed = page.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(arr) = serde_json::from_str::<Vec<Value>>(trimmed) {
                    comments.extend(arr);
                } else if let Ok(one) = serde_json::from_str::<Value>(trimmed) {
                    comments.push(one);
                }
            }
            ToolOutcome::ok(render_review_comments(number, &comments))
        }
        Err(e) => ToolOutcome::error(format!("pr_review_comments failed: {e}")),
    }
}

async fn issue_create(args: &Value, root: &Path) -> ToolOutcome {
    let title = match args.get("title").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return ToolOutcome::error("issue_create requires 'title'"),
    };
    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");

    let mut cmd = gh_cmd(root);
    cmd.args(["issue", "create", "--title", title, "--body", body]);
    cmd.args(create_flag_args(args, false));

    match run_gh(cmd).await {
        Ok(url) => ToolOutcome::ok(format!("Issue created: {}", url.trim())),
        Err(e) => ToolOutcome::error(format!("issue_create failed: {e}")),
    }
}

async fn issue_edit(args: &Value, root: &Path) -> ToolOutcome {
    let number = match args.get("number").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => return ToolOutcome::error("issue_edit requires 'number'"),
    };

    let mut cmd = gh_cmd(root);
    cmd.args(["issue", "edit", &number.to_string()]);
    cmd.args(issue_edit_flags(args));

    match run_gh(cmd).await {
        Ok(out) => ToolOutcome::ok(format!("Issue #{number} updated. {}", out.trim())),
        Err(e) => ToolOutcome::error(format!("issue_edit failed: {e}")),
    }
}

async fn issue_list(args: &Value, root: &Path) -> ToolOutcome {
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10);
    let mut cmd = gh_cmd(root);
    cmd.args([
        "issue",
        "list",
        "--json",
        "number,title,state,labels,createdAt",
        "--limit",
        &limit.to_string(),
    ]);

    push_repeated_flag_cmd(&mut cmd, "--label", &str_or_list(args, "label"));

    match run_gh(cmd).await {
        Ok(json) => format_json_table(&json, &["number", "title", "state"]),
        Err(e) => ToolOutcome::error(format!("issue_list failed: {e}")),
    }
}

/// Read one issue's full body + metadata (`gh issue view <n> --json …`). ReadOnly,
/// so it is usable in Plan mode (#825) — closes the "can list but not read a single
/// issue's body" gap that forced a `bash gh` fallback.
async fn issue_view(args: &Value, root: &Path) -> ToolOutcome {
    let number = match args.get("number").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => return ToolOutcome::error("issue_view requires 'number'"),
    };
    let mut cmd = gh_cmd(root);
    cmd.args([
        "issue",
        "view",
        &number.to_string(),
        "--json",
        "number,title,state,author,labels,assignees,body",
    ]);
    match run_gh(cmd).await {
        Ok(json) => {
            let v: Value = serde_json::from_str(&json).unwrap_or_default();
            ToolOutcome::ok(render_record(&v, "issue"))
        }
        Err(e) => ToolOutcome::error(format!("issue_view failed: {e}")),
    }
}

/// Read one PR: metadata + body by default, or the raw unified diff when
/// `diff: true` (`gh pr diff <n>` — the compact form the review flow prefers over
/// the file-listing JSON). ReadOnly, usable in Plan mode (#825).
async fn pr_view(args: &Value, root: &Path) -> ToolOutcome {
    let number = match args.get("number").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => return ToolOutcome::error("pr_view requires 'number'"),
    };
    let diff = args.get("diff").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut cmd = gh_cmd(root);
    if diff {
        cmd.args(["pr", "diff", &number.to_string()]);
        return match run_gh(cmd).await {
            Ok(out) if out.trim().is_empty() => {
                ToolOutcome::ok(format!("PR #{number}: empty diff."))
            }
            Ok(out) => ToolOutcome::ok(out),
            Err(e) => ToolOutcome::error(format!("pr_view (diff) failed: {e}")),
        };
    }
    cmd.args([
        "pr",
        "view",
        &number.to_string(),
        "--json",
        "number,title,state,author,baseRefName,headRefName,additions,deletions,changedFiles,body",
    ]);
    match run_gh(cmd).await {
        Ok(json) => {
            let v: Value = serde_json::from_str(&json).unwrap_or_default();
            ToolOutcome::ok(render_record(&v, "pr"))
        }
        Err(e) => ToolOutcome::error(format!("pr_view failed: {e}")),
    }
}

/// Render a single issue/PR JSON record as readable markdown (title, state, and
/// the full body) rather than a one-row table. `login`-bearing sub-objects
/// (author) and `name`-bearing arrays (labels) / `login` arrays (assignees) are
/// flattened to comma lists.
fn render_record(v: &Value, kind: &str) -> String {
    let num = v.get("number").and_then(|x| x.as_u64());
    let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
    let state = v.get("state").and_then(|x| x.as_str()).unwrap_or("?");
    let sigil = if kind == "pr" { "PR #" } else { "#" };
    let head = match num {
        Some(n) => format!("{sigil}{n} [{state}] {title}"),
        None => format!("[{state}] {title}"),
    };
    let mut out = vec![head];

    if let Some(author) = v
        .get("author")
        .and_then(|a| a.get("login"))
        .and_then(|l| l.as_str())
    {
        out.push(format!("author: {author}"));
    }
    let labels = join_field(v.get("labels"), "name");
    if !labels.is_empty() {
        out.push(format!("labels: {labels}"));
    }
    let assignees = join_field(v.get("assignees"), "login");
    if !assignees.is_empty() {
        out.push(format!("assignees: {assignees}"));
    }
    if kind == "pr" {
        if let (Some(base), Some(head_ref)) = (
            v.get("baseRefName").and_then(|x| x.as_str()),
            v.get("headRefName").and_then(|x| x.as_str()),
        ) {
            let adds = v.get("additions").and_then(|x| x.as_u64()).unwrap_or(0);
            let dels = v.get("deletions").and_then(|x| x.as_u64()).unwrap_or(0);
            let files = v.get("changedFiles").and_then(|x| x.as_u64()).unwrap_or(0);
            out.push(format!(
                "{head_ref} → {base}  +{adds}/-{dels} across {files} files"
            ));
        }
    }

    let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("").trim();
    out.push(String::new());
    out.push(if body.is_empty() {
        "(no description)".to_string()
    } else {
        body.to_string()
    });
    out.join("\n")
}

/// Flatten a JSON array of objects to a comma-joined list of one string field
/// (e.g. `labels[].name`, `assignees[].login`). Empty when absent/not an array.
fn join_field(arr: Option<&Value>, key: &str) -> String {
    arr.and_then(|a| a.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|it| it.get(key).and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// Cap a `diff_hunk` string to a few lines / chars so one noisy file doesn't
/// blow the model context window. Returns the input unchanged when it's
/// already under the limit; otherwise truncates and appends a marker.
fn trim_diff_hunk(hunk: &str) -> String {
    const MAX_LINES: usize = 6;
    const MAX_CHARS: usize = 400;
    let trimmed = hunk.trim_end_matches('\n');
    let lines: Vec<&str> = trimmed.split('\n').collect();
    if lines.len() <= MAX_LINES && trimmed.chars().count() <= MAX_CHARS {
        return trimmed.to_string();
    }
    let mut out: Vec<&str> = lines.into_iter().take(MAX_LINES).collect();
    // Drop a trailing hunk-context line (one that starts with ' ' or is just
    // '...') so we don't cut mid-context.
    while out
        .last()
        .is_some_and(|l| l.trim().is_empty() || l.trim() == "...")
    {
        out.pop();
    }
    let joined = out.join("\n");
    let truncated = joined.chars().take(MAX_CHARS).collect::<String>();
    format!("{truncated}\n  …(diff hunk trimmed)")
}

/// Render a list of PR reviews as readable markdown. Newest first by
/// `submittedAt` (then by `id` for stability). Tolerant of missing
/// fields — an unknown state still renders, an absent body becomes
/// `(no description)`. Empty input yields a friendly "no reviews" line.
fn render_reviews(number: u64, reviews: &[Value]) -> String {
    if reviews.is_empty() {
        return format!("PR #{number} has no reviews yet.");
    }

    let mut sorted: Vec<&Value> = reviews.iter().collect();
    sorted.sort_by(|a, b| {
        let ta = a.get("submittedAt").and_then(|v| v.as_str()).unwrap_or("");
        let tb = b.get("submittedAt").and_then(|v| v.as_str()).unwrap_or("");
        tb.cmp(ta).then_with(|| {
            let ia = a.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
            let ib = b.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
            ib.cmp(&ia)
        })
    });

    let count = sorted.len();
    let mut out = vec![format!("Reviews on PR #{number} ({count}):"), String::new()];

    for r in &sorted {
        let state = r.get("state").and_then(|v| v.as_str()).unwrap_or("?");
        let author = r
            .get("user")
            .or_else(|| r.get("author"))
            .and_then(|u| u.get("login"))
            .and_then(|l| l.as_str())
            .unwrap_or("?");
        let ts = r.get("submittedAt").and_then(|v| v.as_str()).unwrap_or("");
        let body = r.get("body").and_then(|v| v.as_str()).unwrap_or("").trim();
        out.push(format!("[{state}] {author} — {ts}"));
        if body.is_empty() {
            out.push("(no description)".to_string());
        } else {
            out.push(body.to_string());
        }
        out.push(String::new());
    }

    // Drop the trailing blank line so callers can `format!` cleanly.
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out.join("\n")
}

/// One thread inside one file: the first element is the root comment, the
/// rest are replies in chronological order.
type ReviewThread = Vec<Value>;
type FileThreads = (String, Vec<ReviewThread>);

/// Group inline review comments by file path, then thread them via
/// `in_reply_to_id`. Within each file, root comments come first (sorted
/// chronologically), and each root is followed by its replies in order.
/// Orphan replies (whose parent isn't a root in the set) are bucketed under
/// a synthetic root keyed on their `in_reply_to_id` so they aren't lost.
///
/// Note: GitHub's inline-review API flattens reply chains so every reply's
/// `in_reply_to_id` points at the thread root, not the previous comment — so
/// real chains are depth-2 relative to the root and attach in one pass. If a
/// depth-3 chain ever shows up (each reply pointing at the previous one),
/// only the root + first reply attach; any grandchild surfaces as its own
/// synthetic thread. See `group_review_comments_depth_three_chain_stays_visible`.
fn group_review_comments(comments: &[Value]) -> Vec<FileThreads> {
    use std::collections::HashMap;

    if comments.is_empty() {
        return Vec::new();
    }

    // Bucket by file path first; preserve insertion order so files appear
    // in the order their first comment was created.
    let mut file_order: Vec<String> = Vec::new();
    let mut by_path: HashMap<String, Vec<&Value>> = HashMap::new();
    for c in comments {
        let path = c
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        if !by_path.contains_key(&path) {
            file_order.push(path.clone());
        }
        by_path.entry(path).or_default().push(c);
    }

    let mut out: Vec<FileThreads> = Vec::with_capacity(file_order.len());

    for path in file_order {
        let items = by_path.remove(&path).unwrap_or_default();

        // Sort all comments in this file by createdAt asc, id asc as a tiebreaker.
        let mut sorted = items;
        sorted.sort_by(|a, b| {
            let ta = a.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");
            let tb = b.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");
            ta.cmp(tb).then_with(|| {
                let ia = a.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                let ib = b.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                ia.cmp(&ib)
            })
        });

        // Index by id and by in_reply_to_id for parent lookup.
        let by_id: HashMap<u64, &Value> = sorted
            .iter()
            .filter_map(|c| c.get("id").and_then(|v| v.as_u64()).map(|i| (i, *c)))
            .collect();

        // Roots are comments with no in_reply_to_id (or whose parent we
        // can't find — we still want to surface them).
        let mut roots: Vec<&Value> = sorted
            .iter()
            .copied()
            .filter(|c| {
                c.get("in_reply_to_id")
                    .and_then(|v| v.as_u64())
                    .map(|p| !by_id.contains_key(&p))
                    .unwrap_or(true)
            })
            .collect();

        // Each root is the start of a thread; build it by walking replies.
        // Replies whose root is `roots[i]` follow that thread in order.
        let mut threads: Vec<ReviewThread> = Vec::new();
        let mut thread_for: HashMap<u64, usize> = HashMap::new();
        for (i, root) in roots.iter().enumerate() {
            if let Some(id) = root.get("id").and_then(|v| v.as_u64()) {
                thread_for.insert(id, i);
            }
            threads.push(vec![(*root).clone()]);
        }

        for c in &sorted {
            if let Some(parent) = c.get("in_reply_to_id").and_then(|v| v.as_u64()) {
                if let Some(idx) = thread_for.get(&parent).copied() {
                    threads[idx].push((*c).clone());
                }
                // Else: this is a reply whose parent was filtered out by
                // the roots predicate because it IS a reply (chain > 1) —
                // we'll catch it as a root below via the orphan pass.
            }
        }

        // Orphan sweep: anything still not attached to a thread. Happens
        // when a comment's parent itself is a reply (so the parent was
        // excluded from `roots` and never entered `thread_for`). Group
        // orphans by their `in_reply_to_id` so siblings of a missing parent
        // share one synthetic thread; descendants of a missing parent end up
        // as separate threads, but every comment still surfaces.
        // Covered by `group_review_comments_depth_three_chain_stays_visible`.
        let mut attached: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for t in &threads {
            for c in t {
                if let Some(id) = c.get("id").and_then(|v| v.as_u64()) {
                    attached.insert(id);
                }
            }
        }
        let orphans: Vec<&Value> = sorted
            .iter()
            .copied()
            .filter(|c| {
                c.get("id")
                    .and_then(|v| v.as_u64())
                    .map(|id| !attached.contains(&id))
                    .unwrap_or(false)
            })
            .collect();
        if !orphans.is_empty() {
            // Group orphans by their in_reply_to_id so a chain collapses
            // into one thread, then sort threads by earliest member.
            let mut synth: HashMap<u64, ReviewThread> = HashMap::new();
            for o in &orphans {
                let key = o
                    .get("in_reply_to_id")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                synth.entry(key).or_default().push((*o).clone());
            }
            let mut synth_threads: Vec<ReviewThread> = synth.into_values().collect();
            for t in &mut synth_threads {
                t.sort_by_key(|c| c.get("id").and_then(|v| v.as_u64()).unwrap_or(u64::MAX));
            }
            synth_threads.sort_by_key(|t| {
                t.first()
                    .and_then(|c| c.get("id").and_then(|v| v.as_u64()))
                    .unwrap_or(u64::MAX)
            });
            threads.extend(synth_threads);
            roots.extend(orphans);
        }

        // Stable, file-local root ordering by createdAt asc.
        roots.sort_by(|a, b| {
            let ta = a.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");
            let tb = b.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");
            ta.cmp(tb).then_with(|| {
                let ia = a.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                let ib = b.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                ia.cmp(&ib)
            })
        });

        // Re-key threads to the new root order.
        let mut new_threads: Vec<ReviewThread> = Vec::with_capacity(roots.len());
        for r in &roots {
            let rid = r.get("id").and_then(|v| v.as_u64());
            // Find the thread that currently starts with r; if none, find
            // the orphan thread that contains r and re-root it.
            let thread_idx = threads
                .iter()
                .position(|t| t.first().and_then(|c| c.get("id").and_then(|v| v.as_u64())) == rid);
            if let Some(idx) = thread_idx {
                let t = threads.remove(idx);
                new_threads.push(t);
            } else {
                // Shouldn't happen with the orphan sweep above, but fall
                // back to a single-element thread so the comment still
                // surfaces.
                new_threads.push(vec![(*r).clone()]);
            }
        }

        out.push((path, new_threads));
    }

    out
}

/// Render grouped review comment threads as readable markdown. Each file is
/// introduced by a `── path ──` banner; within a file, root comments
/// introduce a new thread and replies are indented under their parent.
fn render_review_comments(number: u64, comments: &[Value]) -> String {
    if comments.is_empty() {
        return format!("PR #{number} has no review comments.");
    }

    let grouped = group_review_comments(comments);
    let count = comments.len();
    let thread_count: usize = grouped.iter().map(|(_, t)| t.len()).sum();
    let mut out = vec![format!(
        "Review comments on PR #{number} ({count} comments in {thread_count} threads):"
    )];
    out.push(String::new());

    for (path, threads) in &grouped {
        out.push(format!("── {path} ──"));
        out.push(String::new());
        for (i, thread) in threads.iter().enumerate() {
            if i > 0 {
                out.push(String::new());
            }
            render_thread(&mut out, thread);
        }
        out.push(String::new());
    }

    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out.join("\n")
}

/// Render one thread (root + replies) into `out`. The root is the first
/// element; subsequent elements are replies, each indented under the root.
fn render_thread(out: &mut Vec<String>, thread: &[Value]) {
    if thread.is_empty() {
        return;
    }
    let root = &thread[0];
    push_comment(out, root, false);
    for reply in &thread[1..] {
        out.push(String::new());
        let author = author_login(reply).unwrap_or("?");
        out.push(format!("    ↳ {author} (reply)"));
        let body = reply
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if body.is_empty() {
            out.push("      (no description)".to_string());
        } else {
            for line in body.lines() {
                out.push(format!("      {line}"));
            }
        }
    }
}

/// Render a single (root) inline comment: bullet, author @ line, trimmed
/// diff hunk, then the body. `indented` is reserved for future use; today
/// only the root call is `false`.
fn push_comment(out: &mut Vec<String>, c: &Value, indented: bool) {
    let _ = indented; // currently only used by the reply path above
    let author = author_login(c).unwrap_or("?");
    let line = c.get("line").and_then(|v| v.as_u64());
    let original_line = c.get("original_line").and_then(|v| v.as_u64());
    let side = c.get("side").and_then(|v| v.as_str()).unwrap_or("RIGHT");
    let pos = match (line, original_line) {
        (Some(l), _) => format!("line {l} ({side})"),
        (None, Some(o)) => format!("original line {o} ({side})"),
        (None, None) => "?".to_string(),
    };
    out.push(format!("  • {author} @ {pos}"));
    if let Some(hunk) = c.get("diff_hunk").and_then(|v| v.as_str()) {
        if !hunk.trim().is_empty() {
            out.push("    ```diff".to_string());
            for line in trim_diff_hunk(hunk).lines() {
                out.push(format!("    {line}"));
            }
            out.push("    ```".to_string());
        }
    }
    let body = c.get("body").and_then(|v| v.as_str()).unwrap_or("").trim();
    if body.is_empty() {
        out.push("    (no description)".to_string());
    } else {
        for line in body.lines() {
            out.push(format!("    {line}"));
        }
    }
}

fn author_login(c: &Value) -> Option<&str> {
    c.get("user")
        .or_else(|| c.get("author"))
        .and_then(|u| u.get("login"))
        .and_then(|l| l.as_str())
}

async fn push(args: &Value, root: &Path) -> ToolOutcome {
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);

    // Use git directly for push (gh doesn't have a push command).
    let mut cmd = Command::new("git");
    cmd.arg("push").arg("origin").arg("HEAD");
    if force {
        cmd.arg("--force-with-lease");
    }
    // Inherit GH_TOKEN for HTTPS auth if needed.
    if let Some(token) = resolve_token() {
        cmd.env("GH_TOKEN", token);
    }
    cmd.current_dir(root)
        .env("PATH", ff_core::augmented_path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = match cmd.spawn() {
        Ok(child) => match child.wait_with_output().await {
            Ok(o) => o,
            Err(e) => return ToolOutcome::error(format!("push failed: {e}")),
        },
        Err(e) => return ToolOutcome::error(format!("failed to spawn git: {e}")),
    };

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        ToolOutcome::ok(format!("Pushed successfully.\n{}", stderr.trim()))
    } else {
        ToolOutcome::error(format!("push failed:\n{}", stderr.trim()))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse an argument that accepts either a single string or an array of strings
/// into a flat `Vec<String>`, dropping empties. Back-compatible: a bare
/// `"bug"` and `["bug"]` both yield `["bug"]`; a missing key yields `[]`.
fn str_or_list(args: &Value, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_string()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Append a repeated `gh` flag, once per value (e.g. `--label a --label b`).
fn push_repeated_flag(out: &mut Vec<String>, flag: &str, values: &[String]) {
    for v in values {
        out.push(flag.to_string());
        out.push(v.clone());
    }
}

/// Same as [`push_repeated_flag`], but appends directly onto a `Command`
/// (used by `issue_edit`, whose flag names differ: `--add-label` / `--add-assignee`).
fn push_repeated_flag_cmd(cmd: &mut Command, flag: &str, values: &[String]) {
    for v in values {
        cmd.args([flag, v]);
    }
}

/// Build the argument flags for `issue_edit` from the tool `args`.
/// Pure so it can be unit-tested without spawning `gh`.
fn issue_edit_flags(args: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(body) = args.get("body").and_then(|v| v.as_str()) {
        push_repeated_flag(&mut out, "--body", &[body.to_string()]);
    }
    if let Some(title) = args.get("title").and_then(|v| v.as_str()) {
        push_repeated_flag(&mut out, "--title", &[title.to_string()]);
    }
    push_repeated_flag(&mut out, "--add-label", &str_or_list(args, "label"));
    push_repeated_flag(&mut out, "--add-assignee", &str_or_list(args, "assignee"));
    out
}

/// Build the argument flags for `pr_request_review` from the tool `args`.
/// Returns an error if `reviewer` is missing or empty.
/// Pure so it can be unit-tested without spawning `gh`.
fn pr_request_review_flags(args: &Value) -> Result<Vec<String>, String> {
    let reviewers = str_or_list(args, "reviewer");
    if reviewers.is_empty() {
        return Err("pr_request_review requires 'reviewer' (a string or array)".to_string());
    }
    let mut out = Vec::new();
    push_repeated_flag(&mut out, "--add-reviewer", &reviewers);
    Ok(out)
}

/// Build the argument flags for `pr_list` from the tool `args`.
/// Pure so it can be unit-tested without spawning `gh`.
fn pr_list_flags(args: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(author) = args.get("author").and_then(|v| v.as_str()) {
        push_repeated_flag(&mut out, "--author", &[author.to_string()]);
    }
    push_repeated_flag(&mut out, "--label", &str_or_list(args, "label"));
    out
}

/// Build the trailing `--label` / `--assignee` (and optional `--reviewer`) args
/// for a create/edit command from the tool `args`. Kept as a pure function so it
/// can be unit-tested without spawning `gh`.
fn create_flag_args(args: &Value, include_reviewer: bool) -> Vec<String> {
    let mut out = Vec::new();
    push_repeated_flag(&mut out, "--label", &str_or_list(args, "label"));
    push_repeated_flag(&mut out, "--assignee", &str_or_list(args, "assignee"));
    if include_reviewer {
        push_repeated_flag(&mut out, "--reviewer", &str_or_list(args, "reviewer"));
    }
    out
}

/// Format a JSON array into a readable table for the model.
fn format_json_table(json: &str, columns: &[&str]) -> ToolOutcome {
    let rows: Vec<Value> = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => return ToolOutcome::error(format!("failed to parse gh output: {e}")),
    };

    if rows.is_empty() {
        return ToolOutcome::ok("No results.".to_string());
    }

    let mut lines = Vec::with_capacity(rows.len());
    for row in &rows {
        let parts: Vec<String> = columns
            .iter()
            .map(|col| match row.get(*col) {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::Object(o)) => {
                    // For nested objects like author, extract login
                    o.get("login")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string()
                }
                _ => "".to_string(),
            })
            .collect();
        lines.push(parts.join("\t"));
    }

    ToolOutcome::ok(format!("{}\n{}", columns.join("\t"), lines.join("\n")))
}

#[cfg(test)]
mod tests;
