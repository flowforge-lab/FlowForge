use super::*;
use std::fs;

fn sample_entry(id: &str) -> GoalLedgerEntry {
    GoalLedgerEntry {
        id: id.into(),
        status: StepStatus::Pending,
        claim: "the build passes".into(),
        action: None,
        evidence: Vec::new(),
        verdict: None,
        next: None,
        created_ms: 0,
        updated_ms: 0,
    }
}

#[test]
fn default_budget_caps_iterations_at_40() {
    let b = GoalBudget::default();
    assert_eq!(b.max_iterations, DEFAULT_MAX_ITERATIONS);
    assert_eq!(b.max_iterations, 40);
    assert!(b.max_tokens.is_none());
    assert!(b.max_wall_ms.is_none());
}

#[test]
fn goal_round_trips_through_json() {
    let mut g = Goal::new("sess-1", "ship goal mode", 1_000);
    g.append_entry(sample_entry("step-1"), 1_100);
    g.update_entry("step-1", 1_200, |e| {
        e.status = StepStatus::Done;
        e.action = Some("ran cargo test".into());
        e.evidence.push("crates/ff-core: 42 passed".into());
        e.verdict = Some(Verdict::Match);
        e.next = Some(NextAction::Resume);
    });
    let json = serde_json::to_string(&g).unwrap();
    let back: Goal = serde_json::from_str(&json).unwrap();
    assert_eq!(g, back);
}

#[test]
fn unverifiable_verdict_round_trips_as_snake_case() {
    let mut e = sample_entry("s");
    e.verdict = Some(Verdict::Unverifiable);
    e.next = Some(NextAction::AskUser);
    let json = serde_json::to_string(&e).unwrap();
    assert!(json.contains(r#""verdict":"unverifiable""#), "got: {json}");
    assert!(json.contains(r#""next":"ask_user""#), "got: {json}");
    let back: GoalLedgerEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(e, back);
}

#[test]
fn partial_file_loads_via_serde_defaults() {
    // Only the three truly-required fields present; everything else defaults.
    let raw = r#"{"sessionId":"s","objective":"do the thing","status":"active"}"#;
    let g: Goal = serde_json::from_str(raw).unwrap();
    assert_eq!(g.iteration, 0);
    assert_eq!(g.budget.max_iterations, 40);
    assert_eq!(g.spent.tokens, 0);
    assert!(g.ledger.is_empty());
    assert!(g.pending_steer.is_none());
}

#[test]
fn append_and_update_entry_touch_timestamps() {
    let mut g = Goal::new("s", "obj", 1);
    g.append_entry(sample_entry("a"), 10);
    assert_eq!(g.ledger[0].created_ms, 10);
    assert_eq!(g.ledger[0].updated_ms, 10);
    assert_eq!(g.updated_ms, 10);

    let updated = g.update_entry("a", 20, |e| e.status = StepStatus::Active);
    assert!(updated);
    assert_eq!(g.ledger[0].status, StepStatus::Active);
    assert_eq!(g.ledger[0].updated_ms, 20);
    assert_eq!(g.ledger[0].created_ms, 10, "created is not clobbered");
    assert_eq!(g.updated_ms, 20);

    assert!(!g.update_entry("missing", 30, |_| {}), "no such id");
}

#[test]
fn checkpoint_accumulates_spend_and_bumps_iteration() {
    let mut g = Goal::new("s", "obj", 0);
    g.budget.max_tokens = Some(100);
    g.checkpoint(30, 500, 10);
    g.checkpoint(40, 600, 20);
    assert_eq!(g.iteration, 2);
    assert_eq!(g.spent.tokens, 70);
    assert_eq!(g.spent.wall_ms, 1_100);
    assert_eq!(g.updated_ms, 20);
    assert!(!g.budget_exhausted(), "70 < 100 tokens, 2 < 25 iters");
    g.checkpoint(40, 0, 30);
    assert!(g.budget_exhausted(), "110 >= 100 tokens");
}

#[test]
fn budget_exhausted_on_iteration_ceiling() {
    let mut g = Goal::new("s", "obj", 0);
    g.budget.max_iterations = 2;
    assert!(!g.budget_exhausted());
    g.checkpoint(0, 0, 1);
    g.checkpoint(0, 0, 2);
    assert!(g.budget_exhausted(), "2 >= 2");
}

#[test]
fn store_save_load_delete_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = GoalStore::new(dir.path().join("goals"));
    assert!(store.load("s").unwrap().is_none(), "absent before save");

    let g = Goal::new("s", "obj", 42);
    store.save(&g).unwrap();
    let loaded = store.load("s").unwrap().expect("present after save");
    assert_eq!(loaded, g);

    store.delete("s").unwrap();
    assert!(store.load("s").unwrap().is_none(), "gone after delete");
    store.delete("s").unwrap(); // idempotent
}

#[test]
fn store_save_leaves_no_tmp_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("goals");
    let store = GoalStore::new(&root);
    store.save(&Goal::new("s", "obj", 1)).unwrap();
    let entries: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec!["s.json".to_string()], "only the final file");
}

#[test]
fn store_corrupt_file_surfaces_error_not_silent_none() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("goals");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("s.json"), "{ not valid json").unwrap();
    let store = GoalStore::new(&root);
    assert!(store.load("s").is_err(), "corrupt file is an error");
}

#[test]
fn list_active_returns_only_active_goals_and_tolerates_junk() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("goals");
    let store = GoalStore::new(&root);

    let save_with = |sid: &str, status: GoalStatus| {
        let mut g = Goal::new(sid, "obj", 1);
        g.status = status;
        store.save(&g).unwrap();
    };
    save_with("live-a", GoalStatus::Active);
    save_with("live-b", GoalStatus::Active);
    save_with("paused", GoalStatus::Paused);
    save_with("done", GoalStatus::Completed);
    save_with("failed", GoalStatus::Failed);
    save_with("spent", GoalStatus::Exhausted);
    // A corrupt checkpoint and an atomic-write leftover must not derail the
    // scan, and a non-checkpoint file is ignored.
    fs::write(root.join("garbage.json"), "{ not valid json").unwrap();
    fs::write(root.join("half.json.tmp"), "{}").unwrap();
    fs::write(root.join("README.txt"), "notes").unwrap();

    // list_active returns a deterministic (session-id-sorted) order, so no
    // sort is needed here — assert the order directly.
    let active: Vec<String> = store
        .list_active()
        .into_iter()
        .map(|g| g.session_id)
        .collect();
    assert_eq!(active, vec!["live-a".to_string(), "live-b".to_string()]);
}

#[test]
fn list_active_on_missing_dir_is_empty_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    // Point at a subdir that was never created (no goal ever saved).
    let store = GoalStore::new(dir.path().join("never-created"));
    assert!(store.list_active().is_empty());
}

/// Both the desktop and the CLI must resolve the same goals directory so a
/// goal created in one surface is visible to the other (RFC 0020 §5).
#[test]
fn goal_store_dir_matches_desktop_convention() {
    let expected = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("flowforge")
        .join("goals");
    assert_eq!(goal_store_dir(), expected);
}
