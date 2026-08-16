//! The tool registry ↔ ACP boundary projection.
//!
//! Both directions of the ACP boundary funnel through this one module, so the
//! permission model cannot split into "native" and "ACP" variants that drift
//! (ticket #1204).
//!
//! - **Outbound** ([`advertised_for_acp`]): which tools we advertise to an ACP
//!   client. This is where FlowForge's `Deny` cell gets enforced, because ACP
//!   cannot express it.
//! - **Inbound** ([`acp_method_to_native`] / [`inbound_permission_cell`]): how
//!   agent→client requests (`fs/*`, `terminal/*`) resolve onto the **same**
//!   native tool identity and safety tier a native call would use, so
//!   [`PermissionMatrix::effective_cell`] gates them identically.

use ff_core::{Mode, PermissionCell, PermissionMatrix, Safety};
use ff_tools::ToolRegistry;
use serde_json::Value;

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

// ---------------------------------------------------------------------------
// Inbound projection: agent→client method → native tool identity
// ---------------------------------------------------------------------------

/// How an inbound (agent→client) ACP method projects onto FlowForge's tool
/// model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundProjection {
    /// A tool-shaped effect: resolved through the named native tool's safety
    /// tier and [`PermissionMatrix::effective_cell`] exactly like a native call.
    Tool { name: &'static str },
    /// A protocol operation on an already-gated resource (e.g. `terminal/output`
    /// on a terminal whose `terminal/create` already passed the gate). No tool
    /// identity of its own, so no independent permission check.
    Protocol,
}

/// Error from [`acp_method_to_native`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcpMethodError {
    /// The method name has no entry in the projection table.
    #[error("ACP method {0:?} has no native tool projection")]
    UnmappedMethod(String),
}

/// Map an inbound (agent→client) ACP method name onto FlowForge's tool model.
///
/// **Exhaustive** over the §Q3 `fs/*` and `terminal/*` method set — every
/// known method maps to either a [`Tool`](InboundProjection::Tool) (resolved
/// through the permission matrix) or [`Protocol`](InboundProjection::Protocol)
/// (no tool identity, handled by the protocol layer). Anything not in the known
/// set is an **explicit error**, never a silent allow that would bypass the
/// permission gate.
pub fn acp_method_to_native(method: &str) -> Result<InboundProjection, AcpMethodError> {
    match method {
        "fs/read_text_file" => Ok(InboundProjection::Tool { name: "view" }),
        "fs/write_text_file" => Ok(InboundProjection::Tool { name: "write" }),
        "terminal/create" => Ok(InboundProjection::Tool { name: "bash" }),
        "terminal/output" | "terminal/release" | "terminal/kill" | "terminal/wait_for_exit" => {
            Ok(InboundProjection::Protocol)
        }
        other => Err(AcpMethodError::UnmappedMethod(other.to_string())),
    }
}

/// Resolve the permission cell an inbound ACP request resolves through.
///
/// Uses the **identical** path a native call would — looks up the tool in the
/// registry, resolves its safety tier via [`Tool::safety`] (the same function
/// every native call uses), and feeds the result through
/// [`PermissionMatrix::effective_cell`]. ACP-originated and native calls cannot
/// drift because they share every step.
///
/// Returns [`None`] for [`InboundProjection::Protocol`] (no tool gate).
///
/// [`Tool::safety`]: ff_tools::Tool::safety
pub fn inbound_permission_cell(
    projection: InboundProjection,
    args: &Value,
    registry: &ToolRegistry,
    matrix: &PermissionMatrix,
    mode: Mode,
) -> Option<PermissionCell> {
    match projection {
        InboundProjection::Tool { name } => {
            let safety = registry
                .get(name)
                .map(|tool| tool.safety(args))
                .unwrap_or(Safety::Dangerous);
            Some(matrix.effective_cell(name, mode, safety))
        }
        InboundProjection::Protocol => None,
    }
}

/// The outcome of enforcing a permission cell for an inbound request.
///
/// Used by all four inbound entry points (`fs/read_text_file`,
/// `fs/write_text_file`, `terminal/create`, `session/request_permission`)
/// so `Ask` is handled consistently — no approval UI is wired on the
/// client side yet, so `Ask` and `Deny` both refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundDecision {
    /// The call is allowed to proceed (cell was `Allow` or `Protocol`).
    Execute,
    /// The call is refused.
    Refuse,
}

/// Enforce a permission cell for an inbound request.
///
/// - [`PermissionCell::Allow`] → [`InboundDecision::Execute`]
/// - [`PermissionCell::Ask`] → [`InboundDecision::Refuse`] (no approval UI)
/// - [`PermissionCell::Deny`] → [`InboundDecision::Refuse`]
/// - [`None`] (protocol op) → [`InboundDecision::Execute`] (no tool gate)
pub fn enforce_inbound(cell: Option<PermissionCell>) -> InboundDecision {
    match cell {
        Some(PermissionCell::Allow) | None => InboundDecision::Execute,
        Some(PermissionCell::Ask) | Some(PermissionCell::Deny) => InboundDecision::Refuse,
    }
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
        fn safety(&self, _args: &Value) -> Safety {
            self.ceiling
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

    // ---- AC1: one function decides ACP visibility, over all 15 cells ----

    /// Every `Mode` × `Safety` cell, so the enumeration below is visibly total
    /// (3 × 5 = 15) rather than a silently-incomplete sample.
    const ALL_SAFETIES: [Safety; 5] = [
        Safety::ReadOnly,
        Safety::Write,
        Safety::Sensitive,
        Safety::Dangerous,
        Safety::Publish,
    ];

    #[test]
    fn advertised_set_matches_the_native_outcome_in_every_mode_safety_cell() {
        let matrix = PermissionMatrix::default();
        for mode in [Mode::Plan, Mode::Act, Mode::Auto] {
            for safety in ALL_SAFETIES {
                let mut reg = ToolRegistry::new();
                reg.register(Box::new(StubTool::fixed("probe", safety)));
                let advertised = advertised_for_acp(&reg, mode, &matrix);
                match matrix.cell(mode, safety) {
                    PermissionCell::Deny => assert!(
                        advertised.is_empty(),
                        "{mode:?}×{safety:?} is Deny but \"probe\" was advertised"
                    ),
                    _ => assert!(
                        advertised.contains(&"probe".to_string()),
                        "{mode:?}×{safety:?} is not Deny but \"probe\" was hidden"
                    ),
                }
            }
        }
    }

    // ---- AC2: a new tool in `ff-tools` needs no ACP registration step ----

    #[test]
    fn a_freshly_registered_tool_is_advertised_without_any_acp_step() {
        let matrix = PermissionMatrix::default();
        let mut reg = ToolRegistry::with_defaults();
        // Count from the registry at runtime — a hard-coded count is the hidden
        // coupling the ticket's watch item warns about (#1190/#1193 touched the
        // `view`/`edit`/`write` set on `main`).
        let before = reg.iter_tools().count();
        reg.register(Box::new(StubTool::fixed(
            "brand_new_tool",
            Safety::ReadOnly,
        )));
        assert_eq!(reg.iter_tools().count(), before + 1);

        for mode in [Mode::Plan, Mode::Act, Mode::Auto] {
            let advertised = advertised_for_acp(&reg, mode, &matrix);
            assert!(
                advertised.contains(&"brand_new_tool".to_string()),
                "a ReadOnly tool must surface in {mode:?} with no ACP registration"
            );
            // Everything the registry carries is either advertised or denied —
            // the projection never invents tools and never drops allowed ones.
            for tool in reg.iter_tools() {
                let expected = matrix
                    .effective_cell(tool.name(), mode, tool.min_safety())
                    .is_deny();
                assert_eq!(
                    !advertised.contains(&tool.name().to_string()),
                    expected,
                    "{:?} advertised-set membership disagrees with the matrix in {mode:?}",
                    tool.name()
                );
            }
        }
    }

    // ---- AC3: a tool landing in a Deny cell is provably absent ----

    #[test]
    fn a_deny_cell_tool_is_provably_absent_from_every_advertisement() {
        let matrix = PermissionMatrix::default();
        // Plan×Write is Deny in the default matrix — a write tool there is the
        // exact leak that is invisible in review unless asserted.
        for mode in [Mode::Plan, Mode::Act, Mode::Auto] {
            let mut reg = ToolRegistry::new();
            reg.register(Box::new(StubTool::fixed("write_probe", Safety::Write)));
            let advertised = advertised_for_acp(&reg, mode, &matrix);
            if matrix.cell(mode, Safety::Write).is_deny() {
                assert!(
                    advertised.is_empty(),
                    "the write probe must be entirely absent in {mode:?} (Deny cell)"
                );
            }
        }
    }

    // ---- AC4: the inbound method → native tool mapping is exhaustive ----

    #[test]
    fn every_fs_and_terminal_method_maps_and_unmapped_methods_error() {
        // The §Q3 agent→client method set. Each must resolve (a tool or a
        // protocol op) — an `Err` here is a method we silently fail to gate.
        for method in [
            "fs/read_text_file",
            "fs/write_text_file",
            "terminal/create",
            "terminal/output",
            "terminal/release",
            "terminal/wait_for_exit",
            "terminal/kill",
        ] {
            assert!(
                acp_method_to_native(method).is_ok(),
                "{method} is a known ACP method and must have a projection"
            );
        }
        // Anything else is an explicit error, never a silent allow.
        for unknown in ["fs/read_directory", "fs/stat", "foo/bar", "session/prompt"] {
            assert_eq!(
                acp_method_to_native(unknown),
                Err(AcpMethodError::UnmappedMethod(unknown.to_string())),
                "{unknown} must be refused by name, not allowed through"
            );
        }
    }

    #[test]
    fn fs_methods_project_onto_the_native_tools() {
        assert_eq!(
            acp_method_to_native("fs/read_text_file"),
            Ok(InboundProjection::Tool { name: "view" })
        );
        assert_eq!(
            acp_method_to_native("fs/write_text_file"),
            Ok(InboundProjection::Tool { name: "write" })
        );
        assert_eq!(
            acp_method_to_native("terminal/create"),
            Ok(InboundProjection::Tool { name: "bash" })
        );
    }

    #[test]
    fn terminal_management_methods_are_protocol_ops_not_tool_effects() {
        for method in [
            "terminal/output",
            "terminal/release",
            "terminal/kill",
            "terminal/wait_for_exit",
        ] {
            assert_eq!(
                acp_method_to_native(method),
                Ok(InboundProjection::Protocol),
                "{method} operates on an already-gated terminal and must not re-open a tool gate"
            );
        }
    }

    #[test]
    fn inbound_permission_cell_resolves_through_the_native_tool() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(StubTool::fixed("view", Safety::ReadOnly)));
        reg.register(Box::new(StubTool::fixed("write", Safety::Write)));
        let matrix = PermissionMatrix::default();

        // fs/read_text_file → view (ReadOnly): Allow in every mode.
        let projection = acp_method_to_native("fs/read_text_file").unwrap();
        for mode in [Mode::Plan, Mode::Act, Mode::Auto] {
            assert_eq!(
                inbound_permission_cell(projection, &serde_json::json!({}), &reg, &matrix, mode),
                Some(matrix.effective_cell("view", mode, Safety::ReadOnly)),
                "fs/read_text_file must resolve exactly like a native view call in {mode:?}"
            );
        }
        // fs/write_text_file → write (Write): Deny in Plan, Allow in Act/Auto.
        let projection = acp_method_to_native("fs/write_text_file").unwrap();
        assert_eq!(
            inbound_permission_cell(
                projection,
                &serde_json::json!({}),
                &reg,
                &matrix,
                Mode::Plan
            ),
            Some(PermissionCell::Deny),
            "fs/write_text_file must be denied in Plan exactly like the native write tool"
        );
        assert_eq!(
            inbound_permission_cell(projection, &serde_json::json!({}), &reg, &matrix, Mode::Act),
            Some(PermissionCell::Allow)
        );
        // Protocol ops carry no tool gate.
        let projection = acp_method_to_native("terminal/output").unwrap();
        assert_eq!(
            inbound_permission_cell(projection, &serde_json::json!({}), &reg, &matrix, Mode::Act),
            None,
            "a protocol operation has no tool cell"
        );
        // A projection naming a tool the registry does not carry fails closed,
        // not open: an unknown tool name resolves through the conservative
        // default tier (`Safety::Dangerous`), which is Deny in Plan — never a
        // silent Allow that would let the effect through regardless.
        let projection = InboundProjection::Tool {
            name: "no_such_tool",
        };
        assert_eq!(
            inbound_permission_cell(
                projection,
                &serde_json::json!({}),
                &reg,
                &matrix,
                Mode::Plan
            ),
            Some(PermissionCell::Deny),
            "a missing tool must fail closed in Plan, never resolve to a silent Allow"
        );
    }

    #[test]
    fn enforce_inbound_refuses_ask_in_every_inbound_entry_point() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(StubTool::fixed("write", Safety::Write)));
        reg.register(Box::new(StubTool::fixed("view", Safety::ReadOnly)));
        reg.register(Box::new(StubTool::fixed("bash", Safety::Dangerous)));
        let mut matrix = PermissionMatrix::default();
        // Override all three tools that the inbound projections map to:
        // the matrix cell alone for each mode×safety combo may already be
        // Allow/Ask/Deny, but a per-tool override to Ask is unambiguous.
        for tool in ["write", "view", "bash"] {
            matrix.set_override(tool, PermissionCell::Ask);
        }

        // fs/read_text_file → view: Ask cell → Refuse.
        let projection = acp_method_to_native("fs/read_text_file").unwrap();
        for mode in [Mode::Plan, Mode::Act, Mode::Auto] {
            let cell =
                inbound_permission_cell(projection, &serde_json::json!({}), &reg, &matrix, mode);
            assert_eq!(
                enforce_inbound(cell),
                InboundDecision::Refuse,
                "an Ask-cell fs/read_text_file must be refused in {mode:?}"
            );
        }

        // fs/write_text_file → write: Ask cell → Refuse.
        let projection = acp_method_to_native("fs/write_text_file").unwrap();
        for mode in [Mode::Plan, Mode::Act, Mode::Auto] {
            let cell =
                inbound_permission_cell(projection, &serde_json::json!({}), &reg, &matrix, mode);
            assert_eq!(
                enforce_inbound(cell),
                InboundDecision::Refuse,
                "an Ask-cell fs/write_text_file must be refused in {mode:?}"
            );
        }

        // terminal/create → bash: Ask cell → Refuse.
        let projection = acp_method_to_native("terminal/create").unwrap();
        for mode in [Mode::Plan, Mode::Act, Mode::Auto] {
            let cell = inbound_permission_cell(
                projection,
                &serde_json::json!({"command": "ls"}),
                &reg,
                &matrix,
                mode,
            );
            assert_eq!(
                enforce_inbound(cell),
                InboundDecision::Refuse,
                "an Ask-cell terminal/create must be refused in {mode:?}"
            );
        }

        // Protocol ops (terminal/output, etc.) have no cell → Execute.
        let projection = acp_method_to_native("terminal/output").unwrap();
        let cell =
            inbound_permission_cell(projection, &serde_json::json!({}), &reg, &matrix, Mode::Act);
        assert_eq!(
            enforce_inbound(cell),
            InboundDecision::Execute,
            "a protocol op must always be allowed through (no tool gate)"
        );
    }
}
