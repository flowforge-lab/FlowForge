//! Tool bridge: MCP tools as first-class [`ff_tools::Tool`] implementors (RFC 0003 §6).
//!
//! Each running MCP server's tools are registered into the per-turn `ToolRegistry`
//! under a namespaced id `mcp__<server>__<tool>` (double-underscore, matching the
//! Aki/Claude convention) to prevent collisions with built-ins and across servers.
//! Every bridged tool defaults to `Safety::Write` so it is approval-gated — external
//! code touching the user's machine is never auto-run.
//!
//! # Turn lifecycle
//!
//! `build_bridged_tools(handle)` snapshots the supervisor's tool list and returns
//! `Box<dyn Tool>` instances ready for `ToolRegistry::register`. The snapshot is
//! taken once per turn so a hot-reload mid-turn never races an in-flight call.

use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;

use ff_core::McpToolInfo;
use ff_tools::{Safety, Tool, ToolOutcome};

use crate::supervisor::SupervisorHandle;

/// A single MCP tool exposed through the `ToolRegistry`. Routes calls through the
/// [`SupervisorHandle`] so the supervisor's actor (which owns the live client) does
/// the actual `call_tool` — preserving single-ownership + clean reaping.
pub struct McpBridgedTool {
    handle: SupervisorHandle,
    server: String,
    tool_name: String,
    full_name: String,
    description: String,
    input_schema: Value,
}

impl McpBridgedTool {
    /// The `mcp__<server>__<tool>` namespaced name this tool is registered under.
    pub fn namespaced_name(server: &str, tool: &str) -> String {
        format!("mcp__{server}__{tool}")
    }

    fn new(handle: SupervisorHandle, info: &McpToolInfo) -> Self {
        let full_name = Self::namespaced_name(&info.server, &info.name);
        Self {
            handle,
            server: info.server.clone(),
            tool_name: info.name.clone(),
            full_name,
            description: info.description.clone(),
            input_schema: info.input_schema.clone(),
        }
    }
}

#[async_trait]
impl Tool for McpBridgedTool {
    fn name(&self) -> &str {
        &self.full_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.input_schema.clone()
    }

    fn safety(&self, _args: &Value) -> Safety {
        // All external-process tool calls are approval-gated (RFC 0003 §9.4).
        Safety::Write
    }

    async fn run(&self, args: Value, _root: &Path) -> ToolOutcome {
        match self
            .handle
            .call_tool(&self.server, &self.tool_name, args)
            .await
        {
            Ok(text) => ToolOutcome::ok(text),
            Err(e) => ToolOutcome::error(e.to_string()),
        }
    }
}

/// Snapshot the supervisor's running tools and build bridge instances ready for
/// registration. Call once per turn so the model sees exactly the tools that were
/// live at turn start (RFC 0003 §6, same discipline as skill snapshots).
pub fn build_bridged_tools(handle: &SupervisorHandle) -> Vec<Box<dyn Tool>> {
    handle
        .tools_snapshot()
        .iter()
        .map(|info| Box::new(McpBridgedTool::new(handle.clone(), info)) as Box<dyn Tool>)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaced_name_format() {
        assert_eq!(
            McpBridgedTool::namespaced_name("my-server", "do_thing"),
            "mcp__my-server__do_thing"
        );
    }
}
