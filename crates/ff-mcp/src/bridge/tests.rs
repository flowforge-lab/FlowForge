use super::*;

#[test]
fn namespaced_name_format() {
    assert_eq!(
        McpBridgedTool::namespaced_name("my-server", "do_thing"),
        "mcp__my-server__do_thing"
    );
}

#[test]
fn namespaced_name_sanitizes_dotted_tool_segment() {
    // #1070: Obsidian CLI reports dotted names like `base.query`; a raw `.` in the
    // minted id fails the provider tool-name regex `[a-zA-Z0-9_-]+` and 400s the
    // whole request. The `.` collapses to `_`; the `mcp__`/`server` structure and
    // legal chars (`_`, `-`, alphanumerics) are untouched.
    assert_eq!(
        McpBridgedTool::namespaced_name("obsidian", "base.query"),
        "mcp__obsidian__base_query"
    );
    assert_eq!(
        McpBridgedTool::namespaced_name("obsidian", "daily.append"),
        "mcp__obsidian__daily_append"
    );
    assert_eq!(sanitize_tool_segment("search.context"), "search_context");
    assert_eq!(sanitize_tool_segment("ok_name-1"), "ok_name-1");
    assert_eq!(sanitize_tool_segment("a b/c"), "a_b_c");
}

#[test]
fn sanitized_names_match_provider_charset() {
    // Every minted id from a dotted-name server must satisfy the provider regex.
    for tool in [
        "base.query",
        "base.views",
        "daily.append",
        "daily.path",
        "property.read",
        "search.context",
        "template.read",
    ] {
        let name = McpBridgedTool::namespaced_name("obsidian", tool);
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')),
            "minted name {name:?} must match [a-zA-Z0-9_-]+"
        );
    }
}

#[test]
fn disambiguated_name_suffixes_on_collision() {
    use std::collections::HashSet;
    // #1070: when two server names collapse to the same minted id after
    // sanitization, the second (and third) get `_2`/`_3` so the registry key
    // stays unique instead of silently overwriting.
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert("mcp__obsidian__base_query".to_string());
    let second = disambiguated_name(&seen, "mcp__obsidian__base_query");
    assert_eq!(second, "mcp__obsidian__base_query_2");
    seen.insert(second);
    let third = disambiguated_name(&seen, "mcp__obsidian__base_query");
    assert_eq!(third, "mcp__obsidian__base_query_3");
}

#[test]
fn build_bridged_tools_disambiguates_names_that_collide_after_sanitization() {
    // #1070 end-to-end: two distinct bare tool names from the same server that
    // collapse to the *same* minted id after sanitization ("search.query" and
    // "search_query" both → "mcp__mem__search_query") must yield two tools with
    // DISTINCT `.name()`s, so `ToolRegistry::register` doesn't silently drop one.
    //
    // This pins the de-dup *branch in `build_bridged_tools`* (not just the
    // `disambiguated_name` helper in isolation): remove or break that branch and
    // this test goes red.
    use crate::supervisor::PublishedTool;
    use ff_core::McpToolInfo;

    fn info(server: &str, name: &str) -> McpToolInfo {
        McpToolInfo {
            server: server.to_string(),
            name: name.to_string(),
            description: String::new(),
            input_schema: serde_json::json!({}),
            read_only_hint: false,
            reaches_network: false,
            defer: true,
        }
    }

    let handle = SupervisorHandle::for_test();
    {
        let mut tools = handle.tools.write().unwrap();
        // Global instances are always in scope for any session_root, so these
        // pass the scope filter without depending on the temp path.
        tools.push(PublishedTool {
            key: InstanceKey::global("mem"),
            info: info("mem", "search.query"),
        });
        tools.push(PublishedTool {
            key: InstanceKey::global("mem"),
            info: info("mem", "search_query"),
        });
    }

    let built = build_bridged_tools(&handle, std::path::Path::new("/tmp/ff-test-collide"));
    let names: Vec<&str> = built.iter().map(|t| t.name()).collect();

    assert_eq!(names.len(), 2, "both colliding tools must be bridged");
    assert_ne!(
        names[0], names[1],
        "colliding tools must get distinct minted names, got {names:?}"
    );
    assert!(
        names.contains(&"mcp__mem__search_query"),
        "first tool keeps the bare minted name: {names:?}"
    );
    assert!(
        names.contains(&"mcp__mem__search_query_2"),
        "second tool is disambiguated with a _2 suffix: {names:?}"
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
    let ro = McpBridgedTool::for_test(true, true);
    assert_eq!(ro.safety(&serde_json::Value::Null), Safety::ReadOnly);
    assert_eq!(ro.min_safety(), Safety::ReadOnly);
    assert_eq!(ro.max_safety(), Safety::ReadOnly);

    // A tool without the hint stays Write at every level → still gated in Plan.
    let rw = McpBridgedTool::for_test(false, true);
    assert_eq!(rw.safety(&serde_json::Value::Null), Safety::Write);
    assert_eq!(rw.min_safety(), Safety::Write);
    assert_eq!(rw.max_safety(), Safety::Write);
}

#[test]
fn bridged_tool_reaches_network_tracks_the_resolved_hint() {
    use ff_tools::Tool;
    // RFC 0013 #884: reaches_network mirrors read_only_hint's plumbing. A server
    // vetted as local (reaches_network=false) yields a bridged tool that survives
    // a LocalOnly phenotype's advertised-set filter; the fail-safe default (true)
    // is stripped. Orthogonal to readOnlyHint — a read-only tool can still egress.
    assert!(!McpBridgedTool::for_test(true, false).reaches_network());
    assert!(McpBridgedTool::for_test(true, true).reaches_network());
    assert!(!McpBridgedTool::for_test(false, false).reaches_network());
    assert!(McpBridgedTool::for_test(false, true).reaches_network());
}

#[test]
fn bridged_tool_defers_by_default_and_honours_an_opt_out() {
    // RFC 0024 Layer 1: bridged tools are ~81% of the standing tools-block cost, so
    // they stay out of it unless their server opts out. Unlike `reaches_network`,
    // getting this wrong costs a `tool_search` round-trip, never a capability leak —
    // hence "default on" rather than "fail-safe".
    let mut info = ff_core::McpToolInfo {
        server: "s".into(),
        name: "t".into(),
        description: String::new(),
        input_schema: serde_json::json!({}),
        read_only_hint: false,
        reaches_network: false,
        defer: true,
    };
    let handle = SupervisorHandle::for_test();
    assert!(
        McpBridgedTool::new(handle.clone(), InstanceKey::global("s"), &info).defer(),
        "a bridged tool is deferred unless its server says otherwise"
    );

    info.defer = false;
    assert!(
        !McpBridgedTool::new(handle, InstanceKey::global("s"), &info).defer(),
        "a server marked defer=false keeps its tools resident"
    );
}
