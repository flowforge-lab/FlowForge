//! Pure desired-vs-running diff (RFC 0003 §3).
//!
//! Hot-reload compares the freshly loaded config (the *desired* set) against the set
//! the supervisor currently has running, and emits the [`ReconcileAction`]s that close
//! the gap. This module is intentionally side-effect free — the supervisor (M4.2)
//! executes the actions (spawn / kill / restart). Keeping the diff pure makes the
//! tricky cases (a server flipping `disabled`, a changed `command`) exhaustively
//! unit-testable without touching real processes.

use std::collections::BTreeMap;

use ff_core::McpServerConfig;

/// A single step toward making the running set match the desired set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileAction {
    /// Spawn a newly desired (enabled) server.
    Start(McpServerConfig),
    /// Stop a server that is no longer desired — removed from the config or flipped
    /// to `disabled: true`.
    Stop(String),
    /// Restart a server whose definition changed while running (new command, args,
    /// env, …). Carries the new config to spawn with.
    Restart(McpServerConfig),
}

/// Diff `desired` (just-loaded config) against `running` (configs the supervisor
/// currently has live) and return the actions that reconcile them.
///
/// `running` holds the configs servers were *started with*, so an unchanged entry
/// compares equal and produces no action. A `disabled: true` desired entry is treated
/// as "not wanted running": it never yields `Start`, and stops a running counterpart.
///
/// Actions are ordered Stop → Restart → Start so a caller applying them sequentially
/// frees resources before claiming new ones; within each kind, ids are sorted for
/// deterministic output.
pub fn reconcile(desired: &[McpServerConfig], running: &[McpServerConfig]) -> Vec<ReconcileAction> {
    let desired_by_id: BTreeMap<&str, &McpServerConfig> =
        desired.iter().map(|c| (c.id.as_str(), c)).collect();
    let running_by_id: BTreeMap<&str, &McpServerConfig> =
        running.iter().map(|c| (c.id.as_str(), c)).collect();

    let mut stops = Vec::new();
    let mut restarts = Vec::new();
    let mut starts = Vec::new();

    for (id, run_cfg) in &running_by_id {
        match desired_by_id.get(id) {
            // Gone from config, or now disabled → stop it.
            None => stops.push(ReconcileAction::Stop((*id).to_string())),
            Some(want) if want.disabled => stops.push(ReconcileAction::Stop((*id).to_string())),
            // Still wanted: restart only if the definition changed.
            Some(want) if want != run_cfg => {
                restarts.push(ReconcileAction::Restart((*want).clone()))
            }
            Some(_) => {}
        }
    }

    for (id, want) in &desired_by_id {
        if !want.disabled && !running_by_id.contains_key(id) {
            starts.push(ReconcileAction::Start((*want).clone()));
        }
    }

    let mut actions = stops;
    actions.extend(restarts);
    actions.extend(starts);
    actions
}

#[cfg(test)]
mod tests;
