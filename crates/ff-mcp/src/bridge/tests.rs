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

#[test]
fn bridged_tool_safety_floor_and_ceiling_track_read_only_hint() {
    use ff_tools::Tool;
    // #846: Plan-mode advertisement gates on `min_safety() == ReadOnly`
    // (ToolRegistry::readonly_capable_names), so a readOnlyHint tool (codegraph)
    // must report ReadOnly for safety AND min_safety AND max_safety — not just
    // safety() (the gap #841 left, which kept codegraph gated out of Plan).
    let ro = McpBridgedTool::for_test(true);
    assert_eq!(ro.safety(&serde_json::Value::Null), Safety::ReadOnly);
    assert_eq!(ro.min_safety(), Safety::ReadOnly);
    assert_eq!(ro.max_safety(), Safety::ReadOnly);

    // A tool without the hint stays Write at every level → still gated in Plan.
    let rw = McpBridgedTool::for_test(false);
    assert_eq!(rw.safety(&serde_json::Value::Null), Safety::Write);
    assert_eq!(rw.min_safety(), Safety::Write);
    assert_eq!(rw.max_safety(), Safety::Write);
}
