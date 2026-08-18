//! `propose_pr` — the propose-PR workflow (#1255, #684 D1 keystone).
//!
//! Atomically composes four existing primitives — `git branch`, `git commit`,
//! `git push`, and `gh pr create --draft` — into one built-in so an agent can
//! turn working-tree changes into a reviewable proposal in a single call.
//!
//! The PR is opened as a **draft** by design (the D4-drop decision on #684): the
//! review surface is GitHub and a human reviews + merges, so the push is a
//! *proposal*, not a ready-for-review commitment.
//!
//! It does not re-implement any git/gh logic; it calls the same module-private
//! functions the `git` and `github` tools use, so path jailing, the write
//! timeout, token-redaction, and `--draft` handling are inherited unchanged. The
//! steps run in order and stop at the first failure, reporting which step failed
//! — there is no partial rollback, because git state is observable and a
//! half-finished proposal is better surfaced than silently undone.

use std::path::Path;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::git::{git_branch, git_commit};
use crate::github::{pr_create, push};
use crate::registry::{Safety, Tool, ToolOutcome};

pub struct ProposePrTool;

/// The tool name, exported so the goal-loop authorisation wiring can key its
/// per-tool override without stringly-typing it at the call site (#1256).
pub const PROPOSE_PR_TOOL_NAME: &str = "propose_pr";

#[async_trait]
impl Tool for ProposePrTool {
    fn name(&self) -> &str {
        PROPOSE_PR_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Turn working-tree changes into a reviewable proposal in one atomic step: \
         create and switch to a new branch, stage and commit, push, and open a \
         DRAFT pull request. The PR is always a draft — it is a proposal for a \
         human to review and merge, not a ready-for-review commitment. Steps run \
         in order and stop at the first failure, reporting which step failed. \
         Publish-tier: denied in Plan, prompts in Auto, allowed in Act."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "branch": {
                    "type": "string",
                    "description": "Name of the new branch to create and switch to (must not already exist)."
                },
                "message": {
                    "type": "string",
                    "description": "Commit message for the staged changes."
                },
                "title": {
                    "type": "string",
                    "description": "Title of the draft pull request."
                },
                "body": {
                    "type": "string",
                    "description": "Body/description of the pull request. Defaults to empty."
                },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional explicit set of paths to stage (jailed to the workspace root). When omitted, all changes are staged (git add -A)."
                },
                "base": {
                    "type": "string",
                    "description": "Base branch the PR targets. Defaults to 'main'."
                }
            },
            "required": ["branch", "message", "title"]
        })
    }

    /// Publish: this produces an externally-visible PR. Denied in Plan, prompts
    /// in Auto, allowed in Act — stronger than a plain Write (which Auto allows).
    fn safety(&self, _args: &Value) -> Safety {
        Safety::Publish
    }

    fn max_safety(&self) -> Safety {
        Safety::Publish
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        // Validate every required arg up front, before any git command runs, so a
        // missing/blank field cannot leave a half-applied state — e.g. a branch
        // created and pushed to origin that then fails at pr_create and cannot be
        // retried because step 1 would collide with the existing branch (#1262
        // review). All three are trimmed and must be non-empty.
        let branch = match required_arg(&args, "branch") {
            Ok(v) => v,
            Err(e) => return e,
        };
        let message = match required_arg(&args, "message") {
            Ok(v) => v,
            Err(e) => return e,
        };
        let title = match required_arg(&args, "title") {
            Ok(v) => v,
            Err(e) => return e,
        };

        // Step 1: branch. git_branch reads `name`.
        let branch_out = git_branch(&json!({ "name": branch }), root).await;
        if !branch_out.success {
            return step_failure("branch", &branch_out.content);
        }

        // Step 2: commit. git_commit reads `message` and optional `paths`.
        let mut commit_args = json!({ "message": message });
        if let Some(paths) = args.get("paths") {
            commit_args["paths"] = paths.clone();
        }
        let commit_out = git_commit(&commit_args, root).await;
        if !commit_out.success {
            return step_failure("commit", &commit_out.content);
        }

        // Step 3: push. HEAD is now on the new branch, so `git push origin HEAD`
        // (what push does) targets it — no branch argument needed.
        let push_out = push(&json!({}), root).await;
        if !push_out.success {
            return step_failure("push", &push_out.content);
        }

        // Step 4: draft PR. head is the branch we just pushed.
        let pr_args = json!({
            "title": title,
            "body": args.get("body").cloned().unwrap_or_else(|| Value::String(String::new())),
            "base": args.get("base").cloned().unwrap_or_else(|| Value::String("main".to_string())),
            "head": branch,
            "draft": true,
        });
        let pr_out = pr_create(&pr_args, root).await;
        if !pr_out.success {
            return step_failure("pr_create", &pr_out.content);
        }

        ToolOutcome::ok(format!(
            "Proposed PR.\n1. {}\n2. {}\n3. {}\n4. {}",
            branch_out.content.trim(),
            commit_out.content.trim(),
            push_out.content.trim(),
            pr_out.content.trim(),
        ))
    }
}

/// Report which step failed, preserving the underlying tool's error, so a
/// half-finished proposal is diagnosable rather than silently swallowed.
fn step_failure(step: &str, detail: &str) -> ToolOutcome {
    ToolOutcome::error(format!("propose_pr failed at step '{step}': {detail}"))
}

/// Extract a required string arg, trimmed. Returns the trimmed value, or a
/// ready-to-return error `ToolOutcome` when the key is absent, not a string, or
/// blank after trimming — so `run` can reject bad input before any git command.
fn required_arg(args: &Value, key: &str) -> Result<String, ToolOutcome> {
    match args.get(key).and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        _ => Err(ToolOutcome::error(format!(
            "propose_pr requires a non-empty '{key}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    /// Hermetic throwaway git repo with an initial commit (mirrors the git-tool
    /// fixture). No remote, so push/pr_create steps are exercised only via the
    /// failure-path tests, not against real GitHub.
    fn init_temp_repo() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("git runs")
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.path().join("README.md"), "hi\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "--quiet", "-m", "initial commit"]);
        dir
    }

    fn current_branch(root: &Path) -> String {
        let out = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(root)
            .output()
            .expect("git runs");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[tokio::test]
    async fn missing_branch_is_rejected_before_any_git_runs() {
        let repo = init_temp_repo();
        let before = current_branch(repo.path());
        let out = ProposePrTool
            .run(json!({ "message": "m", "title": "t" }), repo.path())
            .await;
        assert!(!out.success);
        assert!(out.content.contains("'branch'"), "got: {}", out.content);
        // No branch was created.
        assert_eq!(current_branch(repo.path()), before);
    }

    #[tokio::test]
    async fn missing_message_is_rejected_before_any_git_runs() {
        // #1262 review: message is now validated up front, so a missing one is
        // rejected before step 1 — no branch created, nothing pushed.
        let repo = init_temp_repo();
        let before = current_branch(repo.path());
        std::fs::write(repo.path().join("new.txt"), "x\n").unwrap();
        let out = ProposePrTool
            .run(json!({ "branch": "feat/x", "title": "t" }), repo.path())
            .await;
        assert!(!out.success);
        assert!(out.content.contains("'message'"), "got: {}", out.content);
        assert_eq!(current_branch(repo.path()), before, "no branch created");
    }

    #[tokio::test]
    async fn missing_title_is_rejected_before_any_git_runs() {
        // The blocker the review found: without up-front validation, a missing
        // title reaches step 4 only after branch+commit+push, leaving a pushed
        // branch that cannot be retried. It must be rejected before any git runs.
        let repo = init_temp_repo();
        let before = current_branch(repo.path());
        std::fs::write(repo.path().join("new.txt"), "x\n").unwrap();
        let out = ProposePrTool
            .run(json!({ "branch": "feat/x", "message": "m" }), repo.path())
            .await;
        assert!(!out.success);
        assert!(out.content.contains("'title'"), "got: {}", out.content);
        assert_eq!(current_branch(repo.path()), before, "no branch created");
    }

    #[tokio::test]
    async fn blank_title_is_rejected_before_any_git_runs() {
        // A whitespace-only title passes pr_create's presence-only check but is
        // garbage input; trim-then-reject at the façade catches it before git.
        let repo = init_temp_repo();
        let before = current_branch(repo.path());
        std::fs::write(repo.path().join("new.txt"), "x\n").unwrap();
        let out = ProposePrTool
            .run(
                json!({ "branch": "feat/x", "message": "m", "title": "   " }),
                repo.path(),
            )
            .await;
        assert!(!out.success);
        assert!(out.content.contains("'title'"), "got: {}", out.content);
        assert_eq!(current_branch(repo.path()), before, "no branch created");
    }

    #[tokio::test]
    async fn stops_at_commit_when_nothing_staged() {
        // Clean tree: branch is created, commit finds nothing to stage and errors,
        // so the workflow stops at the commit step before ever reaching push.
        let repo = init_temp_repo();
        let out = ProposePrTool
            .run(
                json!({ "branch": "feat/empty", "message": "m", "title": "t" }),
                repo.path(),
            )
            .await;
        assert!(!out.success);
        assert!(
            out.content.contains("step 'commit'"),
            "should stop at commit, got: {}",
            out.content
        );
        assert_eq!(current_branch(repo.path()), "feat/empty");
    }

    #[tokio::test]
    async fn commit_paths_jail_escape_stops_at_commit() {
        // A `../` path is rejected by git_commit's jail; the workflow reports the
        // commit step and does not proceed to push.
        let repo = init_temp_repo();
        std::fs::write(repo.path().join("secret.txt"), "s\n").unwrap();
        let sub = repo.path().join("ws");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("mine.txt"), "m\n").unwrap();
        let out = ProposePrTool
            .run(
                json!({
                    "branch": "feat/escape",
                    "message": "m",
                    "title": "t",
                    "paths": ["../secret.txt"],
                }),
                &sub,
            )
            .await;
        assert!(!out.success);
        assert!(
            out.content.contains("step 'commit'"),
            "should stop at commit, got: {}",
            out.content
        );
    }

    #[test]
    fn safety_is_publish_regardless_of_args() {
        assert_eq!(ProposePrTool.safety(&json!({})), Safety::Publish);
        assert_eq!(ProposePrTool.max_safety(), Safety::Publish);
    }

    #[test]
    fn schema_required_matches_the_fields_run_reads() {
        let schema = ProposePrTool.parameters();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(required, ["branch", "message", "title"]);
        let props = schema["properties"].as_object().unwrap();
        for key in ["branch", "message", "title", "body", "paths", "base"] {
            assert!(props.contains_key(key), "schema missing '{key}'");
        }
    }
}
