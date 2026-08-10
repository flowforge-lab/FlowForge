//! `goal_step` — the agent records one evidence-first step in the active goal's
//! ledger (#74, #1225). In goal mode the system prompt already renders the tail
//! of `Goal.ledger` back into each iteration, so a recorded step is how the loop
//! reconstructs progress from evidence instead of a prose summary.
//!
//! Like [`GoalCompleteTool`](crate::GoalCompleteTool), this tool is a no-op that
//! only *signals*: a tool's `run` receives `(args, root)` and has no handle on
//! the `Goal`, so it cannot mutate the ledger itself. The host (goal loop)
//! observes the call and does the bookkeeping — see `handle_event` in
//! `apps/cli/src/goal_loop.rs` and the desktop equivalent. It is
//! `Safety::ReadOnly`: recording a claim touches no file and leaves no trace
//! outside the goal, so it must never sit behind an approval gate.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

use crate::{Safety, Tool, ToolOutcome};

/// Tool name the goal loop matches on to record a ledger step.
pub const GOAL_STEP_TOOL_NAME: &str = "goal_step";

/// Records one step (claim + verdict) in the active goal's ledger.
pub struct GoalStepTool;

#[async_trait]
impl Tool for GoalStepTool {
    // Purely local: appends one entry to the in-memory goal ledger. Must override
    // the fail-safe `true` default, or `local_tool_names()` excludes it and the
    // agent loses `goal_step` under the LocalOnly/enclave phenotype (#1226).
    fn reaches_network(&self) -> bool {
        false
    }
    fn name(&self) -> &str {
        GOAL_STEP_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Record one step of progress toward the active goal in its durable ledger. \
         Use this after you have attempted something and can say what it proved: pass \
         the `claim` you were testing and the `verdict` once you have checked it. \
         Unlike the `todo` tool, whose checklist lives only in the transcript and is \
         lost on compaction, ledger steps persist with the goal and are shown back to \
         you on later iterations. Pass an existing `id` to update that step in place \
         rather than appending a duplicate. Only meaningful in goal mode."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "claim": {
                    "type": "string",
                    "description": "What this step is supposed to prove or change."
                },
                "verdict": {
                    "type": "string",
                    "enum": ["match", "drift", "unverifiable"],
                    "description": "The outcome of checking the claim against evidence. \
                                    `match` = evidence supports it, `drift` = evidence \
                                    contradicts it, `unverifiable` = it could not be \
                                    checked. Record `unverifiable` rather than omitting \
                                    the verdict, so a later run never inherits \
                                    confidence without evidence. Omit only while the \
                                    step is still in progress."
                },
                "evidence": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Evidence pointers: command output, a test path, a \
                                    diff, a URL, an artifact id."
                },
                "id": {
                    "type": "string",
                    "description": "Id of an existing step to update in place. Omit to \
                                    append a new step."
                }
            },
            "required": ["claim"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::ReadOnly
    }

    fn max_safety(&self) -> Safety {
        Safety::ReadOnly
    }

    async fn run(&self, args: Value, _root: &Path) -> ToolOutcome {
        // A no-op by construction: the host observes `ToolCallStarted`/`Finished`
        // and writes the ledger, because tools get no `Goal` handle. Validation
        // still happens here so a malformed call fails loudly at the call site
        // rather than being silently dropped by the observer.
        let Some(claim) = args.get("claim").and_then(|v| v.as_str()) else {
            return ToolOutcome::error("missing required argument: claim (a string)");
        };
        if claim.trim().is_empty() {
            return ToolOutcome::error("claim must not be empty");
        }
        if let Some(verdict) = args.get("verdict").and_then(|v| v.as_str()) {
            if !matches!(verdict, "match" | "drift" | "unverifiable") {
                return ToolOutcome::error("verdict must be one of: match, drift, unverifiable");
            }
        }
        ToolOutcome::ok(format!("Recorded ledger step: {claim}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn advertises_readonly_with_claim_required() {
        let t = GoalStepTool;
        assert_eq!(t.name(), GOAL_STEP_TOOL_NAME);
        assert_eq!(t.safety(&json!({})), Safety::ReadOnly);
        assert_eq!(t.max_safety(), Safety::ReadOnly);
        // Purely local: must override the fail-safe `true` default so
        // `local_tool_names()` keeps it under the LocalOnly phenotype (#1226).
        assert!(!t.reaches_network());

        let params = t.parameters();
        assert_eq!(params["properties"]["claim"]["type"], "string");
        // `claim` is the only required field: a step may be recorded before its
        // verdict is known.
        assert_eq!(params["required"], json!(["claim"]));
    }

    /// The advertised verdicts must match `Verdict`'s serde form exactly, or the
    /// host silently fails to map them. Guards against a second vocabulary.
    #[test]
    fn advertised_verdicts_match_the_core_enum() {
        let params = GoalStepTool.parameters();
        assert_eq!(
            params["properties"]["verdict"]["enum"],
            json!(["match", "drift", "unverifiable"])
        );
    }

    #[tokio::test]
    async fn records_a_claim() {
        let out = GoalStepTool
            .run(
                json!({ "claim": "ensure_session prevents the FK panic" }),
                &tmp(),
            )
            .await;
        assert!(out.success);
        assert!(out.content.contains("ensure_session prevents the FK panic"));
    }

    #[tokio::test]
    async fn rejects_a_missing_or_empty_claim() {
        let missing = GoalStepTool.run(json!({}), &tmp()).await;
        assert!(!missing.success, "no claim must fail");

        let empty = GoalStepTool.run(json!({ "claim": "   " }), &tmp()).await;
        assert!(!empty.success, "whitespace-only claim must fail");
    }

    #[tokio::test]
    async fn rejects_a_verdict_outside_the_enum() {
        let out = GoalStepTool
            .run(json!({ "claim": "c", "verdict": "passed" }), &tmp())
            .await;
        assert!(
            !out.success,
            "`passed` is not a Verdict variant and must be rejected, not coerced"
        );
    }

    #[tokio::test]
    async fn accepts_every_advertised_verdict() {
        for v in ["match", "drift", "unverifiable"] {
            let out = GoalStepTool
                .run(json!({ "claim": "c", "verdict": v }), &tmp())
                .await;
            assert!(out.success, "advertised verdict {v} must be accepted");
        }
    }
}
