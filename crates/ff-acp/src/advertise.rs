//! Tool advertisement across the ACP boundary.
//!
//! This is where FlowForge's `Deny` cell gets enforced, because ACP cannot express it.

use ff_core::{Mode, PermissionCell, PermissionMatrix};
use ff_tools::ToolRegistry;

/// The tools to advertise to an ACP client.
///
/// # Deliberately stricter than the in-process toolset (#1201 decision B2)
///
/// In-process, `ff_agent`'s `advertised_tools` filters on the `Deny` ceiling **only in
/// Plan mode**; in Act and Auto a denied tool is still advertised and then refused at
/// call time by `pre_prompt_decision`. That is defensible in-process, where the model
/// and the enforcement point sit in the same trust domain and the refusal is immediate.
///
/// An ACP client is an external consumer, so the advertised set is effectively a
/// published API. Here the filter applies in **every** mode, because advertising a tool
/// we will always refuse leaks the shape of the permission matrix and invites the client
/// to build UI for something that can never run.
///
/// **Do not "fix" this to match the in-process behaviour.** The divergence is the point:
/// ACP has no way to hide a tool from the model while keeping it callable, so
/// non-advertisement *is* the enforcement mechanism for `Deny` here, and a `Deny` cell
/// reaching the advertised set is a security regression rather than a cosmetic
/// difference.
///
/// # Why the floor, not the ceiling
///
/// The test is on [`Tool::min_safety`] — the *best* case a tool can reach — so a tool is
/// dropped only when even its most benign possible call is denied. Filtering on the
/// ceiling would hide tools that have a genuine legal path: `bash`'s ceiling is
/// `Dangerous`, but `bash ls` is `ReadOnly`, and the in-process Plan filter admits it
/// for exactly that reason. Concrete calls are still gated per-invocation by
/// [`Tool::safety`] and the matrix, so admitting it here does not widen what can run —
/// it only avoids lying about what exists.
///
/// Per-tool overrides are honoured via [`PermissionMatrix::effective_cell`], so a tool
/// denied by name is filtered even when its safety tier alone would be allowed.
///
/// [`Tool::min_safety`]: ff_tools::Tool::min_safety
/// [`Tool::safety`]: ff_tools::Tool::safety
pub fn advertised_for_acp(
    registry: &ToolRegistry,
    mode: Mode,
    matrix: &PermissionMatrix,
) -> Vec<String> {
    registry
        .iter_tools()
        .filter(|tool| {
            matrix.effective_cell(tool.name(), mode, tool.min_safety()) != PermissionCell::Deny
        })
        .map(|tool| tool.name().to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ff_core::Safety;
    use ff_tools::{Tool, ToolOutcome};
    use serde_json::Value;
    use std::path::Path;

    struct StubTool {
        name: &'static str,
        floor: Safety,
        ceiling: Safety,
    }

    impl StubTool {
        /// A tool with a single fixed safety (floor == ceiling).
        fn fixed(name: &'static str, safety: Safety) -> Self {
            Self {
                name,
                floor: safety,
                ceiling: safety,
            }
        }
    }

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn parameters(&self) -> Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn max_safety(&self) -> Safety {
            self.ceiling
        }
        fn min_safety(&self) -> Safety {
            self.floor
        }
        async fn run(&self, _args: Value, _root: &Path) -> ToolOutcome {
            ToolOutcome::ok("stub")
        }
    }

    fn registry() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        r.register(Box::new(StubTool::fixed(
            "read_only_tool",
            Safety::ReadOnly,
        )));
        r.register(Box::new(StubTool::fixed("write_tool", Safety::Write)));
        r.register(Box::new(StubTool::fixed(
            "dangerous_tool",
            Safety::Dangerous,
        )));
        // Dynamic safety, like `bash`: a genuine read-only path under a dangerous ceiling.
        r.register(Box::new(StubTool {
            name: "dynamic_tool",
            floor: Safety::ReadOnly,
            ceiling: Safety::Dangerous,
        }));
        r
    }

    fn floor_of(reg: &ToolRegistry, name: &str) -> Safety {
        reg.iter_tools()
            .find(|t| t.name() == name)
            .expect("tool registered")
            .min_safety()
    }

    #[test]
    fn nothing_denied_is_advertised_in_any_mode() {
        let reg = registry();
        let matrix = PermissionMatrix::default();

        for mode in [Mode::Plan, Mode::Act, Mode::Auto] {
            for name in advertised_for_acp(&reg, mode, &matrix) {
                assert_ne!(
                    matrix.effective_cell(&name, mode, floor_of(&reg, &name)),
                    PermissionCell::Deny,
                    "{name} is Deny in {mode:?} but was advertised"
                );
            }
        }
    }

    #[test]
    fn a_tool_denied_by_name_is_filtered_even_when_its_tier_is_allowed() {
        let reg = registry();
        let mut matrix = PermissionMatrix::default();
        // ReadOnly is Allow in every mode, so only a per-tool override can deny it.
        matrix.set_override("read_only_tool", PermissionCell::Deny);

        for mode in [Mode::Plan, Mode::Act, Mode::Auto] {
            assert!(
                !advertised_for_acp(&reg, mode, &matrix).contains(&"read_only_tool".to_string()),
                "per-tool Deny override leaked in {mode:?}"
            );
        }
    }

    #[test]
    fn allowed_tools_are_still_advertised() {
        let reg = registry();
        let matrix = PermissionMatrix::default();
        // If this is empty the filter is not discriminating, it is refusing everything —
        // a vacuous version of the test above.
        for mode in [Mode::Plan, Mode::Act, Mode::Auto] {
            assert!(
                advertised_for_acp(&reg, mode, &matrix).contains(&"read_only_tool".to_string()),
                "read-only tool must be advertised in {mode:?}"
            );
        }
    }

    #[test]
    fn a_tool_with_a_read_only_path_survives_a_denied_ceiling() {
        let reg = registry();
        let matrix = PermissionMatrix::default();
        for mode in [Mode::Plan, Mode::Act, Mode::Auto] {
            assert!(
                advertised_for_acp(&reg, mode, &matrix).contains(&"dynamic_tool".to_string()),
                "a tool with a legal read-only call must not be hidden in {mode:?}"
            );
        }
    }

    #[test]
    fn plan_advertises_no_more_than_act() {
        let reg = registry();
        let matrix = PermissionMatrix::default();
        let plan = advertised_for_acp(&reg, Mode::Plan, &matrix).len();
        let act = advertised_for_acp(&reg, Mode::Act, &matrix).len();
        assert!(
            plan <= act,
            "Plan ({plan}) advertised more than Act ({act})"
        );
    }
}
