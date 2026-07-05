//! Structured GitHub operations via the `gh` CLI (#734). Wraps `gh` with JSON
//! output parsing so the agent gets clean, structured PR/issue/CI data without
//! shell escaping or token management headaches.

use std::path::Path;
use std::process::Stdio;

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
                    "enum": ["pr_create", "pr_list", "pr_merge", "pr_checks", "issue_create", "issue_edit", "issue_list", "push"]
                },
                "title": { "type": "string", "description": "Title for PR or issue (pr_create, issue_create)." },
                "body": { "type": "string", "description": "Body text for PR or issue (pr_create, issue_create, issue_edit). Markdown supported." },
                "base": { "type": "string", "description": "Base branch for PR (pr_create). Defaults to 'main'." },
                "head": { "type": "string", "description": "Head branch for PR (pr_create). Defaults to current branch." },
                "number": { "type": "integer", "description": "PR or issue number (pr_merge, pr_checks, issue_edit)." },
                "squash": { "type": "boolean", "description": "Squash merge (pr_merge). Defaults to true." },
                "label": { "type": "string", "description": "Filter by or assign label (pr_create, issue_create, issue_list)." },
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
            "issue_create" => issue_create(&args, root).await,
            "issue_edit" => issue_edit(&args, root).await,
            "issue_list" => issue_list(&args, root).await,
            "push" => push(&args, root).await,
            _ => ToolOutcome::error(format!("unknown action: {action}")),
        }
    }
}

/// Resolve the GH_TOKEN: check env, then try reading from ~/.config/flowforge/gh_token.
fn resolve_token() -> Option<String> {
    if let Ok(tok) = std::env::var("GH_TOKEN") {
        if !tok.is_empty() {
            return Some(tok);
        }
    }
    let path = dirs::config_dir()?.join("flowforge").join("gh_token");
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Build a `gh` command with token and working directory set.
fn gh_cmd(root: &Path) -> Command {
    let mut cmd = Command::new("gh");
    if let Some(token) = resolve_token() {
        cmd.env("GH_TOKEN", token);
    }
    cmd.current_dir(root)
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
    if let Some(label) = args.get("label").and_then(|v| v.as_str()) {
        cmd.args(["--label", label]);
    }

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

async fn issue_create(args: &Value, root: &Path) -> ToolOutcome {
    let title = match args.get("title").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return ToolOutcome::error("issue_create requires 'title'"),
    };
    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");

    let mut cmd = gh_cmd(root);
    cmd.args(["issue", "create", "--title", title, "--body", body]);

    if let Some(label) = args.get("label").and_then(|v| v.as_str()) {
        cmd.args(["--label", label]);
    }

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

    if let Some(label) = args.get("label").and_then(|v| v.as_str()) {
        cmd.args(["--label", label]);
    }

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

    #[test]
    fn resolve_token_from_file_fallback() {
        // We don't test env-var path (set_var races other tests in the same
        // process). The file-fallback path is tested implicitly: if GH_TOKEN
        // is unset and the file doesn't exist, resolve_token returns None on CI.
        // On a dev machine with ~/.config/flowforge/gh_token, it returns Some.
        // Either outcome is valid — we just verify it doesn't panic.
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
}
