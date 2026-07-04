//! Safety tiers, permission cells, and the permission matrix — the single source
//! of truth for what each mode allows at each safety tier (RFC 0019 §3, #682/#699).

use crate::Mode;
use serde::{Deserialize, Serialize};

/// How much trust a given tool invocation needs. The agent loop auto-runs
/// [`Safety::ReadOnly`] and defers higher tiers to the [`PermissionMatrix`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Safety {
    ReadOnly,
    Write,
    /// Externally-visible actions (network egress, git push, sub-agent spawn)
    /// that warrant a distinct trust tier between [`Write`] and [`Dangerous`]
    /// (#682). In the default matrix: auto-approved in Act, prompted in Auto,
    /// denied (hidden) in Plan.
    Sensitive,
    Dangerous,
}

/// What happens when a tool at a given [`Safety`] tier is invoked in a given
/// [`Mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionCell {
    /// Execute without prompting.
    Allow,
    /// Prompt the user for approval before executing.
    Ask,
    /// Tool is hidden from the model and cannot be invoked.
    Deny,
}

impl PermissionCell {
    pub fn is_allow(self) -> bool {
        self == Self::Allow
    }

    pub fn is_deny(self) -> bool {
        self == Self::Deny
    }
}

/// A 2-D lookup: [`Mode`] × [`Safety`] → [`PermissionCell`].
///
/// Persisted as JSON with `#[serde(default)]` so a missing or corrupt file
/// gracefully falls back to the RFC 0019 defaults without data loss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionMatrix {
    /// Row-major: `cells[mode_idx][safety_idx]`.
    cells: [[PermissionCell; 4]; 3],
}

impl Default for PermissionMatrix {
    /// RFC 0019 §3 default table:
    /// ```text
    ///          ReadOnly  Write  Sensitive  Dangerous
    /// Plan     Allow     Deny   Deny       Deny
    /// Auto     Allow     Allow  Ask        Deny
    /// Act      Allow     Allow  Allow      Ask
    /// ```
    fn default() -> Self {
        use PermissionCell::*;
        Self {
            cells: [
                // Plan
                [Allow, Deny, Deny, Deny],
                // Auto
                [Allow, Allow, Ask, Deny],
                // Act
                [Allow, Allow, Allow, Ask],
            ],
        }
    }
}

impl PermissionMatrix {
    pub fn cell(&self, mode: Mode, safety: Safety) -> PermissionCell {
        self.cells[mode_idx(mode)][safety_idx(safety)]
    }

    pub fn set_cell(&mut self, mode: Mode, safety: Safety, value: PermissionCell) {
        self.cells[mode_idx(mode)][safety_idx(safety)] = value;
    }
}

fn mode_idx(mode: Mode) -> usize {
    match mode {
        Mode::Plan => 0,
        Mode::Auto => 1,
        Mode::Act => 2,
    }
}

fn safety_idx(safety: Safety) -> usize {
    match safety {
        Safety::ReadOnly => 0,
        Safety::Write => 1,
        Safety::Sensitive => 2,
        Safety::Dangerous => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_rfc0019() {
        let m = PermissionMatrix::default();
        use PermissionCell::*;

        // Plan: only ReadOnly allowed.
        assert_eq!(m.cell(Mode::Plan, Safety::ReadOnly), Allow);
        assert_eq!(m.cell(Mode::Plan, Safety::Write), Deny);
        assert_eq!(m.cell(Mode::Plan, Safety::Sensitive), Deny);
        assert_eq!(m.cell(Mode::Plan, Safety::Dangerous), Deny);

        // Auto: Write auto-approved, Sensitive prompts, Dangerous hidden.
        assert_eq!(m.cell(Mode::Auto, Safety::ReadOnly), Allow);
        assert_eq!(m.cell(Mode::Auto, Safety::Write), Allow);
        assert_eq!(m.cell(Mode::Auto, Safety::Sensitive), Ask);
        assert_eq!(m.cell(Mode::Auto, Safety::Dangerous), Deny);

        // Act: Write+Sensitive auto-approved, Dangerous prompts.
        assert_eq!(m.cell(Mode::Act, Safety::ReadOnly), Allow);
        assert_eq!(m.cell(Mode::Act, Safety::Write), Allow);
        assert_eq!(m.cell(Mode::Act, Safety::Sensitive), Allow);
        assert_eq!(m.cell(Mode::Act, Safety::Dangerous), Ask);
    }

    #[test]
    fn set_cell_mutates() {
        let mut m = PermissionMatrix::default();
        m.set_cell(Mode::Act, Safety::Dangerous, PermissionCell::Allow);
        assert_eq!(m.cell(Mode::Act, Safety::Dangerous), PermissionCell::Allow);
    }

    #[test]
    fn serde_round_trip() {
        let m = PermissionMatrix::default();
        let json = serde_json::to_string_pretty(&m).unwrap();
        let deser: PermissionMatrix = serde_json::from_str(&json).unwrap();
        assert_eq!(m, deser);
    }

    #[test]
    fn serde_default_on_missing_fields() {
        // An empty object should deserialize to the default matrix.
        let deser: PermissionMatrix = serde_json::from_str("{}").unwrap();
        assert_eq!(deser, PermissionMatrix::default());
    }

    #[test]
    fn is_allow_is_deny() {
        assert!(PermissionCell::Allow.is_allow());
        assert!(!PermissionCell::Allow.is_deny());
        assert!(PermissionCell::Deny.is_deny());
        assert!(!PermissionCell::Deny.is_allow());
        assert!(!PermissionCell::Ask.is_allow());
        assert!(!PermissionCell::Ask.is_deny());
    }
}
