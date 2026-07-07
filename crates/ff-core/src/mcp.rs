//! MCP (Model Context Protocol) host types — external tool servers and the tools
//! they expose. These ARE the IPC contract for the server-status panel (M4.4), so
//! changing one regenerates bindings. See `docs/rfcs/0003-mcp-host.md` §4.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Where an MCP server instance is keyed (RFC 0018 §4.1). `Global` keeps RFC 0003's
/// one-instance-per-id semantics; `Workspace` runs one instance per distinct
/// workspace root. Absent in `mcp.json` means `Global`, so existing configs are
/// unchanged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum McpScope {
    #[default]
    Global,
    Workspace,
}

impl McpScope {
    /// Predicate for `skip_serializing_if`: omit `scope` when it is the default `Global`.
    pub fn is_global(&self) -> bool {
        matches!(self, McpScope::Global)
    }
}

/// One external MCP server definition, as it appears in `~/.flowforge/mcp.json`
/// (Claude/Cursor `mcpServers` shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct McpServerConfig {
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "McpScope::is_global")]
    pub scope: McpScope,
}

/// Lifecycle state of a supervised MCP server (RFC 0003 §5). The supervisor (M4.2)
/// drives the transitions; M4.0 only ever reports `Running` (or surfaces a connect
/// failure as an error).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum McpServerState {
    Starting,
    Running,
    Restarting,
    Failed,
    Disabled,
}

/// A snapshot of one server's status for the UI / supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct McpServerStatus {
    pub id: String,
    pub state: McpServerState,
    pub tool_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_error: Option<String>,
    pub restarts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pid: Option<u32>,
    /// For a workspace-scoped instance, a short label for its root (the path) so the
    /// UI can disambiguate two instances of the same server id (RFC 0018 section 4.2).
    /// `None` for a global instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub scope_key: Option<String>,
}

/// One tool advertised by an MCP server (RFC 0003 §4). `input_schema` is the raw
/// JSON Schema the server reports, carried verbatim onto the bridged tool (M4.3) so
/// the model gets accurate argument typing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct McpToolInfo {
    /// The id of the server that advertised this tool.
    pub server: String,
    /// The bare tool name as the server reports it (before `mcp__<server>__` namespacing).
    pub name: String,
    pub description: String,
    #[ts(type = "unknown")]
    pub input_schema: serde_json::Value,
    /// The server's `annotations.readOnlyHint` (MCP spec): the tool declares it
    /// does not modify its environment. Absent hint defaults to `false` (treated
    /// as write-capable), so the safety default stays conservative. Used to let a
    /// read-only bridged tool run without an approval gate (e.g. codegraph queries).
    #[serde(default)]
    pub read_only_hint: bool,
}

#[cfg(test)]
mod tests;
