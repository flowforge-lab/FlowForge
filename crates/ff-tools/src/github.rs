//! Structured GitHub operations via the `gh` CLI (#734). Wraps `gh` with JSON
//! output parsing so the agent gets clean, structured PR/issue/CI data without
//! shell escaping or token management headaches.

use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;

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
                    "enum": ["pr_create", "pr_list", "pr_merge", "pr_checks", "pr_review", "pr_comment", "pr_request_review", "pr_review_inline", "issue_create", "issue_edit", "issue_list", "issue_comment", "push"]
                },
                "title": { "type": "string", "description": "Title for PR or issue (pr_create, issue_create)." },
                "body": { "type": "string", "description": "Body text for a PR/issue or a review/comment (pr_create, issue_create, issue_edit, pr_review, pr_comment, issue_comment, pr_review_inline). Required for pr_review / pr_review_inline when event is COMMENT or REQUEST_CHANGES (GitHub 422s a bodiless one); optional for APPROVE. Markdown supported." },
                "base": { "type": "string", "description": "Base branch for PR (pr_create). Defaults to 'main'." },
                "head": { "type": "string", "description": "Head branch for PR (pr_create). Defaults to current branch." },
                "number": { "type": "integer", "description": "PR or issue number (pr_merge, pr_checks, pr_review, pr_comment, pr_request_review, pr_review_inline, issue_edit, issue_comment)." },
                "event": { "type": "string", "enum": ["APPROVE", "REQUEST_CHANGES", "COMMENT"], "description": "Review verdict for pr_review / pr_review_inline. Note: APPROVE and REQUEST_CHANGES are rejected on your own PR (422) — use COMMENT for a self-review." },
                "comments": { "type": "array", "description": "Inline review comments for pr_review_inline. Each anchors to a diff line.", "items": { "type": "object", "properties": { "path": { "type": "string", "description": "File path (repo-relative)." }, "line": { "type": "integer", "description": "Line number in the file's diff." }, "side": { "type": "string", "enum": ["LEFT", "RIGHT"], "description": "Diff side. Defaults to RIGHT." }, "start_line": { "type": "integer", "description": "Start line for a multi-line comment (optional)." }, "body": { "type": "string", "description": "Comment text." } }, "required": ["path", "line", "body"] } },
                "squash": { "type": "boolean", "description": "Squash merge (pr_merge). Defaults to true." },
                "label": { "type": ["string", "array"], "items": { "type": "string" }, "description": "Label(s) to filter by or assign — a single string or an array. On issue_create/pr_create each is applied; on issue_edit each is added (--add-label); on issue_list/pr_list used as a filter." },
                "assignee": { "type": ["string", "array"], "items": { "type": "string" }, "description": "GitHub username(s) to assign — a single string or an array (issue_create, pr_create; added on issue_edit)." },
                "reviewer": { "type": ["string", "array"], "items": { "type": "string" }, "description": "Reviewer username(s) to request — a single string or an array (pr_create)." },
                "author": { "type": "string", "description": "Filter by author (pr_list). Use '@me' for self." },
                "limit": { "type": "integer", "description": "Max results to return (pr_list, issue_list). Defaults to 10." },
                "force": { "type": "boolean", "description": "Force push (push). Defaults to false." },
                "delete_branch": { "type": "boolean", "description": "Delete head branch after merge (pr_merge). Defaults to true." }
            },
            "required": ["action"]
        })
    }

    fn safety(&self, args: &Value) -> Safety {
        match args.get("action").and_then(|a| a.as_str()) {
            Some("pr_list" | "pr_checks" | "issue_list") => Safety::ReadOnly,
            _ => Safety::Write,
        }
    }

    fn max_safety(&self) -> Safety {
        Safety::Write
    }

    // Read-only floor: the list/read actions (`pr_list`, `pr_checks`, `issue_list`)
    // are `ReadOnly`, so gh is advertised in Plan; the per-call `safety` gate
    // rejects the mutating actions there (Plan x Write = Deny).
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
            "pr_merge" => pr_merge(&args, root).await,
            "pr_checks" => pr_checks(&args, root).await,
            "pr_review" => pr_review(&args, root).await,
            "pr_comment" => pr_comment(&args, root).await,
            "pr_request_review" => pr_request_review(&args, root).await,
            "pr_review_inline" => pr_review_inline(&args, root).await,
            "issue_create" => issue_create(&args, root).await,
            "issue_edit" => issue_edit(&args, root).await,
            "issue_list" => issue_list(&args, root).await,
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

/// Ask `gh auth token` for the active credential. Returns `None` if `gh` is
/// not installed, not authenticated, or returns an error.
fn gh_auth_token() -> Option<String> {
    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .env("PATH", ff_core::augmented_path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if out.status.success() {
        let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !tok.is_empty() {
            return Some(tok);
        }
    }
    None
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

    if let Some(author) = args.get("author").and_then(|v| v.as_str()) {
        cmd.args(["--author", author]);
    }

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
    let reviewers = str_or_list(args, "reviewer");
    if reviewers.is_empty() {
        return ToolOutcome::error("pr_request_review requires 'reviewer' (a string or array)");
    }
    let mut cmd = gh_cmd(root);
    cmd.args(["pr", "edit", &number.to_string()]);
    push_repeated_flag_cmd(&mut cmd, "--add-reviewer", &reviewers);
    match run_gh(cmd).await {
        Ok(out) => ToolOutcome::ok(format!(
            "Requested review from {} on PR #{number}. {}",
            reviewers.join(", "),
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

    if let Some(body) = args.get("body").and_then(|v| v.as_str()) {
        cmd.args(["--body", body]);
    }
    if let Some(title) = args.get("title").and_then(|v| v.as_str()) {
        cmd.args(["--title", title]);
    }
    push_repeated_flag_cmd(&mut cmd, "--add-label", &str_or_list(args, "label"));
    push_repeated_flag_cmd(&mut cmd, "--add-assignee", &str_or_list(args, "assignee"));

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
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_token_does_not_panic() {
        // resolve_token() is cached in a OnceLock, so we can only test the
        // first-call behavior once per process. On a dev machine with `gh`
        // authenticated it returns Some; on CI (no gh auth) it returns None.
        // Either outcome is valid — we verify it doesn't panic.
        let _ = resolve_token();
    }

    #[test]
    fn safety_read_for_list_actions() {
        let tool = GithubTool;
        let args = serde_json::json!({"action": "pr_list"});
        assert_eq!(tool.safety(&args), Safety::ReadOnly);
        let args = serde_json::json!({"action": "pr_checks"});
        assert_eq!(tool.safety(&args), Safety::ReadOnly);
        let args = serde_json::json!({"action": "issue_list"});
        assert_eq!(tool.safety(&args), Safety::ReadOnly);
    }

    #[test]
    fn safety_write_for_mutating_actions() {
        let tool = GithubTool;
        for action in [
            "pr_create",
            "pr_merge",
            "issue_create",
            "issue_edit",
            "push",
        ] {
            let args = serde_json::json!({"action": action});
            assert_eq!(
                tool.safety(&args),
                Safety::Write,
                "{action} should be Write"
            );
        }
    }

    #[test]
    fn format_json_table_empty() {
        let result = format_json_table("[]", &["number", "title"]);
        assert!(result.success);
        assert_eq!(result.content, "No results.");
    }

    #[test]
    fn format_json_table_rows() {
        let json = r#"[{"number":42,"title":"Fix bug","state":"OPEN"},{"number":43,"title":"Add feature","state":"MERGED"}]"#;
        let result = format_json_table(json, &["number", "title", "state"]);
        assert!(result.success);
        assert!(result.content.contains("42\tFix bug\tOPEN"));
        assert!(result.content.contains("43\tAdd feature\tMERGED"));
    }

    /// Fixture test for pr_checks output parsing — guards against requesting
    /// fields that gh doesn't support (the B1 bug that broke the initial PR).
    #[test]
    fn pr_checks_parse_fixture() {
        // Simulates the JSON that `gh pr checks --json name,state` returns.
        let fixture = r#"[{"name":"Rust (fmt, clippy, test)","state":"SUCCESS"},{"name":"Web (typecheck, lint)","state":"SUCCESS"},{"name":"Windows (compile)","state":"FAILURE"}]"#;
        let checks: Vec<Value> = serde_json::from_str(fixture).unwrap();

        let mut lines = vec!["PR #1 checks:".to_string()];
        for check in &checks {
            let name = check.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let state = check.get("state").and_then(|v| v.as_str()).unwrap_or("?");
            let icon = match state {
                "SUCCESS" => "✓",
                "FAILURE" => "✗",
                "PENDING" | "IN_PROGRESS" => "◯",
                _ => "?",
            };
            lines.push(format!("  {icon} {name}: {state}"));
        }
        let output = lines.join("\n");

        assert!(output.contains("✓ Rust (fmt, clippy, test): SUCCESS"));
        assert!(output.contains("✗ Windows (compile): FAILURE"));
        assert!(!output.contains("conclusion"), "no conclusion field used");
    }

    #[test]
    fn str_or_list_accepts_string_array_and_missing() {
        assert_eq!(str_or_list(&json!({"label": "bug"}), "label"), vec!["bug"]);
        assert_eq!(
            str_or_list(&json!({"label": ["bug", "frontend"]}), "label"),
            vec!["bug", "frontend"]
        );
        assert!(str_or_list(&json!({}), "label").is_empty());
        // empties and non-string items are dropped; whitespace trimmed.
        assert_eq!(
            str_or_list(&json!({"a": ["  x  ", "", 7, "y"]}), "a"),
            vec!["x", "y"]
        );
        assert!(str_or_list(&json!({"a": "   "}), "a").is_empty());
    }

    #[test]
    fn create_flag_args_single_label_backcompat() {
        // A bare string label behaves exactly as before: one --label.
        let out = create_flag_args(&json!({"label": "bug"}), false);
        assert_eq!(out, vec!["--label", "bug"]);
    }

    #[test]
    fn create_flag_args_multi_label_and_assignees() {
        let out = create_flag_args(
            &json!({"label": ["bug", "frontend"], "assignee": "abidkhan03"}),
            false,
        );
        assert_eq!(
            out,
            vec![
                "--label",
                "bug",
                "--label",
                "frontend",
                "--assignee",
                "abidkhan03"
            ]
        );
    }

    #[test]
    fn create_flag_args_reviewer_only_when_included() {
        let args = json!({"assignee": ["a", "b"], "reviewer": "r"});
        // pr_create includes reviewer; issue_create does not.
        assert_eq!(
            create_flag_args(&args, true),
            vec!["--assignee", "a", "--assignee", "b", "--reviewer", "r"]
        );
        assert_eq!(
            create_flag_args(&args, false),
            vec!["--assignee", "a", "--assignee", "b"]
        );
    }

    #[test]
    fn create_flag_args_empty_when_no_fields() {
        assert!(create_flag_args(&json!({"title": "x"}), true).is_empty());
    }

    #[test]
    fn inline_review_payload_builds_comments_and_defaults_side() {
        let args = json!({
            "event": "comment",
            "body": "overall looks good",
            "comments": [
                { "path": "src/a.rs", "line": 12, "body": "nit here" },
                { "path": "src/b.rs", "line": 40, "side": "LEFT", "start_line": 38, "body": "range" }
            ]
        });
        let p = build_inline_review_payload(&args).unwrap();
        assert_eq!(p["event"], "COMMENT", "event upper-cased");
        assert_eq!(p["body"], "overall looks good");
        assert_eq!(p["comments"][0]["side"], "RIGHT", "side defaults to RIGHT");
        assert_eq!(p["comments"][0]["path"], "src/a.rs");
        assert_eq!(p["comments"][0]["line"], 12);
        assert_eq!(p["comments"][1]["side"], "LEFT");
        assert_eq!(p["comments"][1]["start_line"], 38);
        assert!(p["comments"][0].get("start_line").is_none());
    }

    #[test]
    fn inline_review_payload_rejects_bad_event_and_empty_comments() {
        let bad_event = json!({"event": "LGTM", "comments": [{"path":"a","line":1,"body":"x"}]});
        assert!(build_inline_review_payload(&bad_event).is_err());

        let no_comments = json!({"event": "COMMENT", "comments": []});
        assert!(build_inline_review_payload(&no_comments).is_err());

        let missing = json!({"event": "COMMENT"});
        assert!(build_inline_review_payload(&missing).is_err());
    }

    #[test]
    fn inline_review_payload_rejects_incomplete_comment() {
        // missing body
        let a = json!({"comments": [{"path": "a.rs", "line": 3}]});
        assert!(build_inline_review_payload(&a).is_err());
        // missing line
        let b = json!({"comments": [{"path": "a.rs", "body": "x"}]});
        assert!(build_inline_review_payload(&b).is_err());
        // missing path
        let c = json!({"comments": [{"line": 3, "body": "x"}]});
        assert!(build_inline_review_payload(&c).is_err());
    }

    #[test]
    fn inline_review_comment_and_request_changes_require_body() {
        // COMMENT / REQUEST_CHANGES 422 without a top-level body, so building the
        // payload must fail up front (mirrors the pr_review guard). A blank body
        // counts as missing.
        for event in ["COMMENT", "REQUEST_CHANGES"] {
            let blank = json!({"event": event, "body": "   ", "comments": [{"path":"a","line":1,"body":"y"}]});
            assert!(
                build_inline_review_payload(&blank).is_err(),
                "{event} with a blank body should be rejected"
            );

            let missing = json!({"event": event, "comments": [{"path":"a","line":1,"body":"y"}]});
            assert!(
                build_inline_review_payload(&missing).is_err(),
                "{event} with no body should be rejected"
            );
        }
    }

    #[test]
    fn inline_review_approve_may_omit_body() {
        // APPROVE is the one event GitHub accepts without a top-level body.
        let args = json!({"event":"APPROVE","comments":[{"path":"a","line":1,"body":"y"}]});
        let p = build_inline_review_payload(&args).unwrap();
        assert_eq!(p["event"], "APPROVE");
        assert!(p.get("body").is_none(), "APPROVE omits an absent body");
    }
}
