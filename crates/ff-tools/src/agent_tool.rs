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

    /// The sub-agent is a *local dispatch* — it spawns an in-process child, it does
    /// not itself reach the network. Egress is enforced by inheritance: the child's
    /// `ToolContext.egress` is cloned from the parent, so a `LocalOnly` parent yields
    /// a `LocalOnly` child whose own advertised set strips network tools (RFC 0013).
    /// Classifying `agent` as network-capable would wrongly stop `enclave` delegating.
    fn reaches_network(&self) -> bool {
        false
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
                                    read-only audit). Omit to inherit the full toolset. \
                                    Naming a deferred capability here (e.g. an MCP-bridged \
                                    tool) also seeds it into the child's advertised set, \
                                    sparing it a `tool_search` round-trip to discover a \
                                    tool its task obviously needs."
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

    /// Sub-agent spawn is externally-visible, so it is [`Safety::Sensitive`]
    /// (#698); per-call approval is still enforced inside the child loop. Treated
    /// identically to `Write` for now — same approval behavior.
    fn safety(&self, _args: &Value) -> Safety {
        Safety::Sensitive
    }

    /// Ceiling matches [`safety`](Self::safety): this tool is always `Sensitive`,
    /// so it stays hidden in Plan-mode advertisement (same as `Write`).
    fn max_safety(&self) -> Safety {
        Safety::Sensitive
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
        assert_eq!(t.safety(&Value::Null), Safety::Sensitive);
        let params = t.parameters();
        assert_eq!(params["required"][0], "task");
        assert_eq!(params["properties"]["task"]["type"], "string");
        assert_eq!(params["properties"]["tools"]["type"], "array");
    }

    #[test]
    fn tools_param_hints_preloading_a_deferred_capability() {
        // #1273: spawning with an explicit `tools` allowlist that names a
        // deferred (e.g. MCP-bridged) tool now seeds it into the child's
        // advertised set (#1272), sparing the child a `tool_search` discovery
        // round-trip. The schema should point the orchestrator at that lever.
        let desc = AgentTool.parameters()["properties"]["tools"]["description"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            desc.contains("tool_search"),
            "tools param must reference the discovery round-trip it saves: {desc}"
        );
        assert!(
            desc.to_lowercase().contains("deferred") || desc.to_lowercase().contains("mcp"),
            "tools param must mention naming a deferred/MCP capability up front: {desc}"
        );
    }

    #[tokio::test]
    async fn run_directly_is_a_defensive_error() {
        let out = AgentTool.run(Value::Null, Path::new(".")).await;
        assert!(!out.success);
        assert!(out.content.contains("agent loop"));
    }
}
