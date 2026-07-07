use super::*;
use ff_core::Role;

fn test_msg(content: &str) -> Message {
    Message {
        id: "test".into(),
        session_id: String::new(),
        role: Role::Assistant,
        content: content.into(),
        tool_calls: None,
        tool_call_id: None,
        attachments: None,
        reasoning: None,
        stop_reason: None,
        author_name: None,
        created_at: 0,
    }
}

#[test]
fn get_returns_none_for_empty_cache() {
    let cache = CompactionCache::new();
    assert!(cache.get("session-1").is_none());
}

#[test]
fn put_then_get_roundtrips() {
    let cache = CompactionCache::new();
    let msg = test_msg("summary of first 10 messages");
    cache.put("s1", 10, msg.clone(), 42);

    let (boundary, retrieved, count) = cache.get("s1").unwrap();
    assert_eq!(boundary, 10);
    assert_eq!(retrieved.content, msg.content);
    assert_eq!(count, 42);
}

#[test]
fn invalidate_removes_session() {
    let cache = CompactionCache::new();
    cache.put("s1", 5, test_msg("x"), 10);
    cache.put("s2", 8, test_msg("y"), 20);

    cache.invalidate("s1");
    assert!(cache.get("s1").is_none());
    assert!(cache.get("s2").is_some());
}

#[test]
fn invalidate_all_clears_everything() {
    let cache = CompactionCache::new();
    cache.put("s1", 5, test_msg("x"), 10);
    cache.put("s2", 8, test_msg("y"), 20);

    cache.invalidate_all();
    assert!(cache.get("s1").is_none());
    assert!(cache.get("s2").is_none());
}

#[test]
fn put_overwrites_existing() {
    let cache = CompactionCache::new();
    cache.put("s1", 5, test_msg("old"), 10);
    cache.put("s1", 12, test_msg("new"), 25);

    let (boundary, msg, count) = cache.get("s1").unwrap();
    assert_eq!(boundary, 12);
    assert_eq!(msg.content, "new");
    assert_eq!(count, 25);
}

#[test]
fn concurrent_access_from_two_threads() {
    use std::sync::Arc;
    use std::thread;

    let cache = Arc::new(CompactionCache::new());
    let c1 = cache.clone();
    let c2 = cache.clone();

    let t1 = thread::spawn(move || {
        for i in 0..100 {
            c1.put("s1", i, test_msg(&format!("msg-{i}")), i as u64);
        }
    });
    let t2 = thread::spawn(move || {
        for _ in 0..100 {
            // Should never panic even under concurrent writes.
            let _ = c2.get("s1");
        }
    });

    t1.join().unwrap();
    t2.join().unwrap();

    // Final state should reflect thread 1'\''s last write.
    let (boundary, _, _) = cache.get("s1").unwrap();
    assert_eq!(boundary, 99);
}
