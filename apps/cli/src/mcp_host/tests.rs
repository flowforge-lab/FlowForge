use super::*;
use ff_core::McpServerState;

/// A stand-in for a bridged MCP tool. The real `McpBridgedTool::new` is private to
/// `ff-mcp` and `SupervisorHandle::for_test` is `#[cfg(test)]`-gated there, so the
/// deferral policy is exercised through the `Tool` trait instead — which is exactly the
/// surface `partition_and_register` reads.
struct StubTool {
    name: String,
    defer: bool,
}

impl StubTool {
    fn boxed(name: &str, defer: bool) -> Box<dyn ff_tools::Tool> {
        Box::new(Self {
            name: name.to_string(),
            defer,
        })
    }
}

#[async_trait::async_trait]
impl ff_tools::Tool for StubTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "stub"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }

    fn defer(&self) -> bool {
        self.defer
    }

    async fn run(
        &self,
        _args: serde_json::Value,
        _root: &std::path::Path,
    ) -> ff_tools::ToolOutcome {
        ff_tools::ToolOutcome::ok("stub")
    }
}

fn names(registry: &ff_tools::ToolRegistry) -> Vec<String> {
    let mut v: Vec<String> = registry
        .iter_tools()
        .map(|t| t.name().to_string())
        .collect();
    v.sort();
    v
}

/// The load-bearing property of the CLI's MCP wiring. `defer` defaults to *deferred*
/// (`ff-mcp/src/config.rs:51`), and a deferred tool is only advertised once
/// `tool_search` admits it (RFC 0024 Layer 1) — a thing the CLI does not have. Bridging
/// one anyway would register a tool the model can never see, with no error surfaced
/// anywhere, so they are skipped instead.
#[test]
fn deferred_tools_are_skipped_and_resident_ones_are_registered() {
    let mut reg = ff_tools::ToolRegistry::new();
    let registered = partition_and_register(
        vec![
            StubTool::boxed("mcp__a__resident", false),
            StubTool::boxed("mcp__b__deferred", true),
            StubTool::boxed("mcp__c__also_resident", false),
        ],
        &mut reg,
    );

    assert_eq!(registered, 2, "only the two non-deferred tools count");
    assert_eq!(
        names(&reg),
        vec!["mcp__a__resident", "mcp__c__also_resident"],
        "a deferred tool must not reach the registry: nothing in the CLI can admit it"
    );
}

/// The all-deferred case is the one a user hits by default, since an `mcp.json` entry
/// without an explicit `defer` is deferred. It must be a no-op on the registry rather
/// than a partial registration.
#[test]
fn an_all_deferred_server_registers_nothing() {
    let mut reg = ff_tools::ToolRegistry::new();
    let registered = partition_and_register(
        vec![
            StubTool::boxed("mcp__x__one", true),
            StubTool::boxed("mcp__x__two", true),
        ],
        &mut reg,
    );

    assert_eq!(registered, 0);
    assert!(
        names(&reg).is_empty(),
        "an all-deferred server must leave the registry untouched"
    );
}

/// Bridging must be additive: it may never displace or rename a tool the CLI already
/// registered. Guards against a future refactor that rebuilds the registry here.
#[test]
fn bridging_preserves_the_tools_already_registered() {
    let mut reg = ff_tools::ToolRegistry::with_defaults();
    let before = names(&reg);
    assert!(!before.is_empty(), "with_defaults must register something");

    partition_and_register(vec![StubTool::boxed("mcp__s__t", false)], &mut reg);

    let after = names(&reg);
    for tool in &before {
        assert!(
            after.contains(tool),
            "bridging dropped the pre-existing tool {tool}"
        );
    }
    assert!(after.contains(&"mcp__s__t".to_string()));
    assert_eq!(after.len(), before.len() + 1);
}

/// With no `mcp.json`, `init` must return `None` rather than erroring — the CLI stays
/// fully usable when MCP is simply not configured (RFC 0003 §3, §5). Pointing `HOME` at
/// an empty dir is the closest this can get to that state without touching the real one.
#[test]
fn init_is_a_no_op_when_no_config_exists() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_str().unwrap();
    crate::test_support::with_env_set(&[("HOME", home)], || {
        assert!(
            init().is_none(),
            "a missing mcp.json must disable MCP, not fail"
        );
    });
}

fn status(id: &str, state: McpServerState) -> ff_core::McpServerStatus {
    ff_core::McpServerStatus {
        id: id.to_string(),
        state,
        tool_count: 0,
        last_error: None,
        restarts: 0,
        pid: None,
        scope_key: None,
    }
}

/// The startup race, pinned. `spawn_supervisor` returns a handle whose status list is
/// empty and fills it from a spawned task, so an empty snapshot means "the first
/// `reconcile` has not run", *not* "there is nothing to wait for". Reading it as the
/// latter is what shipped zero MCP tools on every CLI run while logging nothing but
/// "MCP host started" — no error, no bridged count, no skip warning.
///
/// This is the assertion that distinguishes the bug from the fix: an empty snapshot must
/// **not** classify as `Settled`.
#[test]
fn an_empty_status_snapshot_is_not_settled() {
    assert_eq!(
        settle_state(&[]),
        Settle::NotYetPublished,
        "an empty snapshot means the supervisor has not reconciled yet; treating it as \
         settled bridges zero tools silently"
    );
}

/// Once every server reports a terminal state the wait must end promptly — the budget is
/// a ceiling, not a sleep. `Failed`/`Disabled` count as settled: a failed server will
/// never publish tools, so blocking on it would burn the budget for nothing.
#[test]
fn terminal_states_settle_including_failed_and_disabled() {
    assert_eq!(
        settle_state(&[
            status("ok", McpServerState::Running),
            status("bad", McpServerState::Failed),
            status("off", McpServerState::Disabled),
        ]),
        Settle::Settled
    );
}

/// Transient states keep the wait alive and are reported by name, so a user whose tools
/// are missing learns which server was too slow rather than just seeing fewer tools.
#[test]
fn transient_states_are_reported_as_pending() {
    assert_eq!(
        settle_state(&[
            status("fast", McpServerState::Running),
            status("slow", McpServerState::Starting),
            status("flapping", McpServerState::Restarting),
        ]),
        Settle::Pending(vec!["slow".to_string(), "flapping".to_string()])
    );
}
