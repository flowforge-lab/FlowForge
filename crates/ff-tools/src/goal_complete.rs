//! `goal_complete` — the agent signals that the active goal's objective is met
//! (RFC 0020 §7, #716). In goal mode the self-continue loop drives repeated
//! turns until the agent decides the objective is done; calling this tool is how
//! it says "stop, we're finished". It is `Safety::ReadOnly` — declaring
//! completion is not a trust decision, so it never hits the approval gate — and
//! it does no workspace work: the host (goal loop) observes the call and
//! transitions the `Goal` to `Completed`. Running it outside goal mode is a
//! harmless no-op acknowledgement.
//!
//! Dual-surface per RFC 0020 §7: the same capability is also an IPC command
//! (`goal_complete`) for the FE; this is the agent-facing tool half.

use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

use crate::registry::{Safety, Tool, ToolOutcome};

/// The tool name, exported so the host loop can match the finished tool call
/// without stringly-typing it at the call site.
pub const GOAL_COMPLETE_TOOL_NAME: &str = "goal_complete";

pub struct GoalCompleteTool;

#[async_trait]
impl Tool for GoalCompleteTool {
    fn name(&self) -> &str {
        GOAL_COMPLETE_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Signal that the current goal's objective is fully met, ending the \
         self-continue loop. Call this ONLY when the objective is genuinely \
         complete and verified — not merely attempted. Optionally include a \
         short summary of what was accomplished."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "A short summary of what was accomplished. Optional."
                }
            },
            "required": []
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::ReadOnly
    }

    fn max_safety(&self) -> Safety {
        Safety::ReadOnly
    }

    /// Acknowledge completion. The host loop detects this call and transitions
    /// the goal; the tool result is only what the model sees. Echoing the
    /// summary keeps the transcript self-describing. Outside goal mode this is a
    /// harmless acknowledgement with no side effect.
    async fn run(&self, args: Value, _root: &Path) -> ToolOutcome {
        let summary = args
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if summary.is_empty() {
            ToolOutcome::ok("Goal marked complete.")
        } else {
            ToolOutcome::ok(format!("Goal marked complete: {summary}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertises_readonly_optional_summary_schema() {
        let t = GoalCompleteTool;
        assert_eq!(t.name(), "goal_complete");
        assert_eq!(t.safety(&Value::Null), Safety::ReadOnly);
        assert_eq!(t.max_safety(), Safety::ReadOnly);
        // Not interactive — it's a real (no-op) tool call the loop observes.
        assert!(!t.interactive());
        let params = t.parameters();
        // `summary` is advertised but optional — never in `required`.
        assert_eq!(params["properties"]["summary"]["type"], "string");
        assert_eq!(params["required"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn run_echoes_summary_when_present() {
        let root = std::path::PathBuf::from(".");
        let out = GoalCompleteTool
            .run(
                serde_json::json!({ "summary": "shipped the feature" }),
                &root,
            )
            .await;
        assert!(out.content.contains("shipped the feature"));

        let bare = GoalCompleteTool.run(serde_json::json!({}), &root).await;
        assert!(bare.content.contains("complete"));
    }
}
