//! `agent` — delegate a scoped subtask to a fresh-context child agent (#234).
//!
//! Like [`crate::ask_user::AskUserTool`], this is a schema-only stub: the agent loop
//! recognizes the call by name and intercepts it *before* dispatch, driving a child
//! turn against an ephemeral session with the same provider/store/approver. The child
//! runs the normal research -> plan -> implement -> verify loop in its own context and
//! returns only a concise summary to the parent — the orchestrator never inherits the
//! child's full transcript. [`Tool::run`] is therefore unreachable in the normal flow.
//!
//! Delegation never escalates privilege: the child's tool calls flow through the same
//! approval gate as the parent, and a depth guard prevents an unbounded sub-agent tree.

use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

use crate::registry::{Safety, Tool, ToolOutcome};

/// The tool name the agent loop intercepts to spawn a sub-agent.
pub const AGENT_TOOL_NAME: &str = "agent";

pub struct AgentTool;

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        AGENT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Delegate a self-contained subtask to a fresh sub-agent that runs in its own \
         context and returns only a summary. Use this to keep your own context lean on \
         large tasks (the child reads the files, runs the tests, and reports back), to \
         get an independent verifier that starts clean, or to fan out parallel subtasks. \
         The child works in the same workspace and its tool calls require the same \
         approval as yours. Give it a complete, standalone brief — it does not see this \
         conversation, only the `task` you write."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The complete, standalone subtask brief for the \
                                    sub-agent. State the goal, the relevant files/paths, \
                                    and exactly what to report back."
                },
                "tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional allowlist of tool names the sub-agent may \
                                    use (e.g. [\"view\", \"grep\", \"glob\"] for a \
                                    read-only audit). Omit to inherit the full toolset."
                },
                "max_iterations": {
                    "type": "integer",
                    "description": "Optional cap on the sub-agent's tool-call iterations. \
                                    Clamped to a safe maximum.",
                    "minimum": 1
                }
            },
            "required": ["task"]
        })
    }

    /// The child may write, and per-call approval is enforced inside the child loop.
    fn safety(&self, _args: &Value) -> Safety {
        Safety::Write
    }

    /// Never reached in the normal flow — the agent loop intercepts `agent` calls
    /// before dispatch. Implemented defensively so a host that bypasses that routing
    /// gets a clear error instead of a silent no-op.
    async fn run(&self, _args: Value, _root: &Path) -> ToolOutcome {
        ToolOutcome::error("agent is handled by the agent loop (sub-agent spawn), not run directly")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertises_delegation_schema() {
        let t = AgentTool;
        assert_eq!(t.name(), "agent");
        assert!(!t.interactive());
        assert_eq!(t.safety(&Value::Null), Safety::Write);
        let params = t.parameters();
        assert_eq!(params["required"][0], "task");
        assert_eq!(params["properties"]["task"]["type"], "string");
        assert_eq!(params["properties"]["tools"]["type"], "array");
    }

    #[tokio::test]
    async fn run_directly_is_a_defensive_error() {
        let out = AgentTool.run(Value::Null, Path::new(".")).await;
        assert!(!out.success);
        assert!(out.content.contains("agent loop"));
    }
}
