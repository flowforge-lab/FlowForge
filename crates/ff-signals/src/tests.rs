use super::*;

fn completed(
    skill: &str,
    tokens: u32,
    turns: u32,
    latency_ms: u32,
    success: bool,
) -> SkillCompleted {
    SkillCompleted {
        skill: skill.to_string(),
        session_id: "s1".to_string(),
        tokens,
        latency_ms,
        turns,
        success,
    }
}

#[test]
fn activation_counts_accumulate() {
    let mut store = SignalStore::new();
    store.record_activated("alpha");
    store.record_activated("alpha");
    store.record_activated("beta");
    assert_eq!(store.aggregate("alpha").unwrap().activations, 2);
    assert_eq!(store.aggregate("beta").unwrap().activations, 1);
    assert!(store.aggregate("gamma").is_none());
}

#[test]
fn rolling_means_and_success_rate() {
    let mut store = SignalStore::new();
    store.record_completed(&completed("alpha", 100, 2, 1000, true));
    store.record_completed(&completed("alpha", 300, 4, 3000, false));
    let agg = store.aggregate("alpha").unwrap();
    assert_eq!(agg.completions, 2);
    assert_eq!(agg.successes, 1);
    assert_eq!(agg.mean_tokens, 200.0);
    assert_eq!(agg.mean_turns, 3.0);
    assert_eq!(agg.mean_latency_ms, 2000.0);
    assert_eq!(agg.success_rate, 0.5);
}

#[test]
fn ingest_dispatches_by_variant() {
    let mut store = SignalStore::new();
    store.ingest(&Signal::SkillActivated(SkillActivated {
        skill: "alpha".to_string(),
        session_id: "s1".to_string(),
    }));
    store.ingest(&Signal::SkillCompleted(completed(
        "alpha", 50, 1, 500, true,
    )));
    store.ingest(&Signal::Intention(IntentionSignal {
        session_id: "s1".to_string(),
        goal: "x".to_string(),
    }));
    let agg = store.aggregate("alpha").unwrap();
    assert_eq!(agg.activations, 1);
    assert_eq!(agg.completions, 1);
    assert_eq!(store.all().len(), 1);
}

#[test]
fn persists_and_reloads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("skill_signals.json");
    {
        let mut store = SignalStore::load(path.clone());
        store.record_activated("alpha");
        store.record_completed(&completed("alpha", 100, 2, 1000, true));
        SignalStore::persist_payload(store.snapshot_payload());
    }
    let reloaded = SignalStore::load(path);
    let agg = reloaded.aggregate("alpha").unwrap();
    assert_eq!(agg.activations, 1);
    assert_eq!(agg.completions, 1);
    assert_eq!(agg.mean_tokens, 100.0);
}

#[test]
fn missing_file_starts_empty() {
    let dir = tempfile::tempdir().unwrap();
    let store = SignalStore::load(dir.path().join("does_not_exist.json"));
    assert!(store.all().is_empty());
}
