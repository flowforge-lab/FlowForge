use super::*;

#[test]
fn namespaced_name_format() {
    assert_eq!(
        McpBridgedTool::namespaced_name("my-server", "do_thing"),
        "mcp__my-server__do_thing"
    );
}

#[test]
fn read_only_hint_maps_to_read_only_else_write() {
    // A readOnlyHint tool (e.g. codegraph queries) is ReadOnly so it isn't
    // approval-gated and stays usable in Plan mode; otherwise Write (gated).
    assert_eq!(safety_for(true), Safety::ReadOnly);
    assert_eq!(safety_for(false), Safety::Write);
}
