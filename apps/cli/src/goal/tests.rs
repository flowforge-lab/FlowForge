use ff_core::{Goal, GoalStatus, GoalStore};
use tempfile::TempDir;

#[test]
fn goal_store_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let store = GoalStore::new(tmp.path());

    let g = Goal {
        session_id: "sess-test".into(),
        objective: "write tests".into(),
        status: GoalStatus::Paused,
        iteration: 2,
        budget: ff_core::GoalBudget {
            max_iterations: 10,
            max_tokens: None,
            max_wall_ms: None,
        },
        spent: ff_core::GoalSpend {
            tokens: 100,
            wall_ms: 500,
        },
        ledger: vec![],
        pending_steer: None,
        verify_cmd: None,
        created_ms: 1,
        updated_ms: 2,
    };

    store.save(&g).unwrap();
    let loaded = store.load("sess-test").unwrap().expect("goal exists");
    assert_eq!(loaded, g);
}

#[test]
fn goal_store_list_loads_all_goals() {
    let tmp = TempDir::new().unwrap();
    let store = GoalStore::new(tmp.path());

    let active = Goal {
        session_id: "sess-a".into(),
        objective: "ship it".into(),
        status: GoalStatus::Active,
        ..Default::default()
    };
    let paused = Goal {
        session_id: "sess-p".into(),
        objective: "pause it".into(),
        status: GoalStatus::Paused,
        ..Default::default()
    };

    store.save(&active).unwrap();
    store.save(&paused).unwrap();

    let active_list = store.list_active();
    assert_eq!(active_list.len(), 1);
    assert_eq!(active_list[0].session_id, "sess-a");
}
