//! Tool bridge: MCP tools as first-class [`ff_tools::Tool`] implementors (RFC 0003 §6).
//!
//! Each running MCP server's tools are registered into the per-turn `ToolRegistry`
//! under a namespaced id `mcp__<server>__<tool>` (double-underscore, matching the
//! Claude convention) to prevent collisions with built-ins and across servers.
//! A bridged tool defaults to `Safety::Write` so it is approval-gated — external
//! code touching the user's machine is never auto-run — unless the server marks it
//! `readOnlyHint` (MCP annotations), in which case it is `Safety::ReadOnly` (e.g.
//! codegraph's local index queries, usable in Plan mode).
//!
//! # Turn lifecycle & instance routing
//!
//! [`build_bridged_tools`] snapshots the supervisor's tool list and returns
//! `Box<dyn Tool>` instances ready for `ToolRegistry::register`. The snapshot is taken
//! once per turn so a hot-reload mid-turn never races an in-flight call.
//!
//! Each tool is bound to the [`InstanceKey`] of the instance that serves it (RFC 0018
//! §4.6). For a turn on workspace `/A`, only `Global` instances and the `Workspace(/A)`
//! instances are exposed, and a `mcp__codegraph__context` call routes to
//! `Workspace(/A)`; the same-named call in a concurrent turn on `/B` routes to
//! `Workspace(/B)`. The model-facing name stays stable across instances.

use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;

use ff_tools::{Safety, Tool, ToolOutcome};

use crate::key::{InstanceKey, ScopeKey};
use crate::supervisor::SupervisorHandle;

/// A single MCP tool exposed through the `ToolRegistry`. Routes calls through the
/// [`SupervisorHandle`] (keyed by the bound [`InstanceKey`]) so the supervisor's actor
/// — which owns the live client — does the actual `call_tool`, preserving
/// single-ownership + clean reaping.
pub struct McpBridgedTool {
    handle: SupervisorHandle,
    key: InstanceKey,
    tool_name: String,
    full_name: String,
    description: String,
    input_schema: Value,
    read_only_hint: bool,
}

impl McpBridgedTool {
    /// The `mcp__<server>__<tool>` namespaced name this tool is registered under.
    pub fn namespaced_name(server: &str, tool: &str) -> String {
        format!("mcp__{server}__{tool}")
    }

    fn new(handle: SupervisorHandle, key: InstanceKey, info: &ff_core::McpToolInfo) -> Self {
        let full_name = Self::namespaced_name(&key.id, &info.name);
        Self {
            handle,
            key,
            tool_name: info.name.clone(),
            full_name,
            description: info.description.clone(),
            input_schema: info.input_schema.clone(),
            read_only_hint: info.read_only_hint,
        }
    }
}

/// Map a bridged tool's `readOnlyHint` to its [`Safety`]. Read-only tools run
/// without an approval gate (usable in Plan mode); everything else stays `Write`
/// so external-process calls remain approval-gated (RFC 0003 §9.4).
fn safety_for(read_only_hint: bool) -> Safety {
    if read_only_hint {
        Safety::ReadOnly
    } else {
        Safety::Write
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
        // A tool that advertises `readOnlyHint` (MCP annotations) doesn't modify its
        // environment — e.g. codegraph's local index queries — so it can run without
        // an approval gate (and stays usable in Plan mode). Everything else defaults
        // to Write so external-process calls remain approval-gated (RFC 0003 §9.4).
        safety_for(self.read_only_hint)
    }

    async fn run(&self, args: Value, _root: &Path) -> ToolOutcome {
        match self
            .handle
            .call_tool(&self.key, &self.tool_name, args)
            .await
        {
            Ok(text) => ToolOutcome::ok(text),
            Err(e) => ToolOutcome::error(e.to_string()),
        }
    }
}

/// Snapshot the supervisor's running tools and build bridge instances for the turn on
/// `session_root`. Only the tools served by instances this session resolves to are
/// included: every `Global` instance plus the `Workspace(session_root)` instances
/// (RFC 0018 §4.6). Call once per turn so the model sees exactly the tools that were
/// live at turn start (same discipline as skill snapshots).
pub fn build_bridged_tools(handle: &SupervisorHandle, session_root: &Path) -> Vec<Box<dyn Tool>> {
    let session_scope = ScopeKey::workspace(session_root);
    handle
        .tools_snapshot()
        .into_iter()
        .filter(|t| match &t.key.scope {
            ScopeKey::Global => true,
            ScopeKey::Workspace(_) => t.key.scope == session_scope,
        })
        .map(|t| {
            Box::new(McpBridgedTool::new(handle.clone(), t.key.clone(), &t.info)) as Box<dyn Tool>
        })
        .collect()
}

#[cfg(test)]
mod tests;
