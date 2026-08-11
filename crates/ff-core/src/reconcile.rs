//! Protocol-agnostic desired-vs-running diff.
//!
//! Hot-reload compares a freshly loaded config (the *desired* set) against the
//! set the supervisor currently has running, and emits the [`ReconcileAction`]s
//! that close the gap. The supervisor executes the actions; this module is the
//! pure diff.
//!
//! Generic over any config type that implements [`ReconcilableConfig`], so the
//! same algorithm works for MCP servers and ACP agents without protocol code
//! leaking into the comparison.

use std::collections::BTreeMap;

/// A config type that the reconcile algorithm can diff.
pub trait ReconcilableConfig: Clone + PartialEq + Eq {
    /// The unique id for this config entry.
    fn id(&self) -> &str;
    /// Whether this entry is disabled (should not be running).
    fn disabled(&self) -> bool;
}

/// A single step toward making the running set match the desired set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileAction<C: ReconcilableConfig> {
    /// Spawn a newly desired (enabled) config.
    Start(C),
    /// Stop a config that is no longer desired — removed from config or flipped
    /// to `disabled: true`.
    Stop(String),
    /// Restart a config whose definition changed while running.
    Restart(C),
}

/// Diff `desired` (just-loaded config) against `running` (configs the supervisor
/// currently has live) and return the actions that reconcile them.
///
/// `running` holds the configs entries were *started with*, so an unchanged entry
/// compares equal and produces no action. A `disabled: true` desired entry is
/// treated as "not wanted running": it never yields `Start`, and stops a running
/// counterpart.
///
/// Actions are ordered Stop → Restart → Start so a caller applying them sequentially
/// frees resources before claiming new ones; within each kind, ids are sorted for
/// deterministic output.
pub fn reconcile<C: ReconcilableConfig>(desired: &[C], running: &[C]) -> Vec<ReconcileAction<C>> {
    let desired_by_id: BTreeMap<&str, &C> = desired.iter().map(|c| (c.id(), c)).collect();
    let running_by_id: BTreeMap<&str, &C> = running.iter().map(|c| (c.id(), c)).collect();

    let mut stops = Vec::new();
    let mut restarts = Vec::new();
    let mut starts = Vec::new();

    for (id, run_cfg) in &running_by_id {
        match desired_by_id.get(id) {
            // Gone from config, or now disabled → stop it.
            None => stops.push(ReconcileAction::Stop((*id).to_string())),
            Some(want) if want.disabled() => stops.push(ReconcileAction::Stop((*id).to_string())),
            // Still wanted: restart only if the definition changed.
            Some(want) if want != run_cfg => {
                restarts.push(ReconcileAction::Restart((*want).clone()))
            }
            Some(_) => {}
        }
    }

    for (id, want) in &desired_by_id {
        if !want.disabled() && !running_by_id.contains_key(id) {
            starts.push(ReconcileAction::Start((*want).clone()));
        }
    }

    let mut actions = stops;
    actions.extend(restarts);
    actions.extend(starts);
    actions
}

impl ReconcilableConfig for crate::McpServerConfig {
    fn id(&self) -> &str {
        &self.id
    }
    fn disabled(&self) -> bool {
        self.disabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::McpScope;

    fn cfg(id: &str, command: &str) -> crate::McpServerConfig {
        crate::McpServerConfig {
            id: id.into(),
            command: command.into(),
            args: vec![],
            env: std::collections::BTreeMap::new(),
            disabled: false,
            scope: McpScope::Global,
            reaches_network: None,
            defer: None,
        }
    }

    #[test]
    fn no_change_yields_nothing() {
        let set = vec![cfg("a", "x"), cfg("b", "y")];
        assert!(reconcile(&set, &set).is_empty());
    }

    #[test]
    fn added_server_starts() {
        let actions = reconcile(&[cfg("a", "x")], &[]);
        assert_eq!(actions, vec![ReconcileAction::Start(cfg("a", "x"))]);
    }

    #[test]
    fn removed_server_stops() {
        let actions = reconcile::<crate::McpServerConfig>(&[], &[cfg("a", "x")]);
        assert_eq!(actions, vec![ReconcileAction::Stop("a".into())]);
    }

    #[test]
    fn changed_command_restarts() {
        let actions = reconcile(&[cfg("a", "new")], &[cfg("a", "old")]);
        assert_eq!(actions, vec![ReconcileAction::Restart(cfg("a", "new"))]);
    }

    #[test]
    fn disabling_a_running_server_stops_it() {
        let mut disabled = cfg("a", "x");
        disabled.disabled = true;
        let actions = reconcile(&[disabled], &[cfg("a", "x")]);
        assert_eq!(actions, vec![ReconcileAction::Stop("a".into())]);
    }

    #[test]
    fn disabled_desired_server_is_never_started() {
        let mut disabled = cfg("a", "x");
        disabled.disabled = true;
        assert!(reconcile(&[disabled], &[]).is_empty());
    }

    #[test]
    fn mixed_diff_orders_stop_restart_start() {
        let desired = vec![cfg("keep", "same"), cfg("change", "v2"), cfg("add", "x")];
        let running = vec![cfg("keep", "same"), cfg("change", "v1"), cfg("drop", "x")];
        let actions = reconcile(&desired, &running);
        assert_eq!(
            actions,
            vec![
                ReconcileAction::Stop("drop".into()),
                ReconcileAction::Restart(cfg("change", "v2")),
                ReconcileAction::Start(cfg("add", "x")),
            ]
        );
    }
}
