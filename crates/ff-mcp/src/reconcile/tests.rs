use super::*;
use ff_core::McpScope;
use std::collections::BTreeMap;

fn cfg(id: &str, command: &str) -> McpServerConfig {
    McpServerConfig {
        id: id.into(),
        command: command.into(),
        args: vec![],
        env: BTreeMap::new(),
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
    let actions = reconcile(&[], &[cfg("a", "x")]);
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
