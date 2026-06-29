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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T>(value: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).unwrap();
        let back: T = serde_json::from_str(&json).unwrap();
        assert_eq!(value, &back);
    }

    #[test]
    fn server_config_round_trips_and_is_camel_case() {
        let cfg = McpServerConfig {
            id: "filesystem".into(),
            command: "npx".into(),
            args: vec![
                "-y".into(),
                "@modelcontextprotocol/server-filesystem".into(),
            ],
            env: BTreeMap::from([("LOG_LEVEL".to_string(), "info".to_string())]),
            disabled: false,
            scope: McpScope::Global,
        };
        round_trip(&cfg);
        // `disabled` defaults so a minimal config parses.
        let minimal: McpServerConfig =
            serde_json::from_str(r#"{"id":"x","command":"echo"}"#).unwrap();
        assert!(minimal.args.is_empty() && !minimal.disabled);
        // An absent `scope` field defaults to Global (RFC 0018 back-compat).
        assert_eq!(minimal.scope, McpScope::Global);
        // Global is skip-serialized, so existing configs round-trip without a
        // `scope` key on the wire.
        let json = serde_json::to_string(&minimal).unwrap();
        assert!(!json.contains("scope"), "{json}");
        // An explicit workspace scope parses and round-trips.
        let ws: McpServerConfig =
            serde_json::from_str(r#"{"id":"x","command":"echo","scope":"workspace"}"#).unwrap();
        assert_eq!(ws.scope, McpScope::Workspace);
        round_trip(&ws);
        assert!(serde_json::to_string(&ws)
            .unwrap()
            .contains("\"scope\":\"workspace\""));
    }

    #[test]
    fn scope_defaults_to_global_and_serializes_lowercase() {
        assert_eq!(McpScope::default(), McpScope::Global);
        assert_eq!(
            serde_json::to_string(&McpScope::Workspace).unwrap(),
            "\"workspace\""
        );
        round_trip(&McpScope::Workspace);
    }

    #[test]
    fn server_state_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&McpServerState::Running).unwrap(),
            "\"running\""
        );
        round_trip(&McpServerState::Failed);
    }

    #[test]
    fn server_status_round_trips() {
        round_trip(&McpServerStatus {
            id: "github".into(),
            state: McpServerState::Failed,
            tool_count: 0,
            last_error: Some("handshake timed out".into()),
            restarts: 3,
            pid: None,
        });
    }

    #[test]
    fn tool_info_round_trips_with_schema() {
        let tool = McpToolInfo {
            server: "filesystem".into(),
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        };
        round_trip(&tool);
        // camelCase: the Rust `input_schema` field is `inputSchema` on the wire.
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("\"inputSchema\""), "{json}");
    }
}
