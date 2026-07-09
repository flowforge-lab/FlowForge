use std::collections::BTreeMap;

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
        reaches_network: None,
    };
    round_trip(&cfg);
    // `disabled` defaults so a minimal config parses.
    let minimal: McpServerConfig = serde_json::from_str(r#"{"id":"x","command":"echo"}"#).unwrap();
    assert!(minimal.args.is_empty() && !minimal.disabled);
    // An absent `scope` field defaults to Global (RFC 0018 back-compat).
    assert_eq!(minimal.scope, McpScope::Global);
    // RFC 0013: `reaches_network` is fail-safe None when absent, so a LocalOnly
    // phenotype strips the server unless the operator vetted it as local.
    assert_eq!(minimal.reaches_network, None);
    let local: McpServerConfig =
        serde_json::from_str(r#"{"id":"cg","command":"codegraph","reachesNetwork":false}"#)
            .unwrap();
    assert_eq!(local.reaches_network, Some(false));
    round_trip(&local);
    assert!(serde_json::to_string(&local)
        .unwrap()
        .contains("\"reachesNetwork\":false"));
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
        scope_key: None,
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
        read_only_hint: true,
        reaches_network: true,
    };
    round_trip(&tool);
    // camelCase: the Rust `input_schema` field is `inputSchema` on the wire.
    let json = serde_json::to_string(&tool).unwrap();
    assert!(json.contains("\"inputSchema\""), "{json}");
    // RFC 0013 #884: `reaches_network` is fail-safe true when absent from the wire
    // (a tool the supervisor hasn't stamped is treated as network-capable).
    let bare: McpToolInfo = serde_json::from_str(
        r#"{"server":"s","name":"t","description":"","inputSchema":{"type":"object"}}"#,
    )
    .unwrap();
    assert!(bare.reaches_network);
    assert!(!bare.read_only_hint);
}
