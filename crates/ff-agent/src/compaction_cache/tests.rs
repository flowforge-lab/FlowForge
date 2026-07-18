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

#[test]
fn evict_when_over_capacity() {
    let cache = CompactionCache::new();
    let msg = test_msg("summary");

    // Beyond capacity, survivors get evicted in LRU order.
    for i in 0..200 {
        cache.put(&format!("s{i}"), i, msg.clone(), i as u64);
    }
    // All entries outside the last 128 should be gone — including s0.
    assert!(cache.get("s0").is_none(), "s0 must be evicted by LRU cap");
    assert!(cache.get("s199").is_some(), "s199 must survive as MRU");
}

#[test]
fn get_promotes_to_most_recently_used() {
    // Fill to capacity, touch the oldest, insert one more: the touched entry
    // survives and the next-oldest gets evicted instead.
    let cache = CompactionCache::new();
    let msg = test_msg("summary");

    for i in 0..128 {
        cache.put(&format!("s{i}"), i, msg.clone(), i as u64);
    }
    // Touch s0 — this promotes it to MRU.
    let _ = cache.get("s0").unwrap();

    cache.put("s-new", 999, test_msg("new"), 999);

    // s0 was promoted, so it survives.
    assert!(
        cache.get("s0").is_some(),
        "touched s0 must survive eviction"
    );
    // s-new is present.
    assert!(cache.get("s-new").is_some());
    // s1 — the new least-recently-used entry after s0's promotion — is gone.
    assert!(
        cache.get("s1").is_none(),
        "s1 is the new LRU and must be evicted"
    );
}

// --- Tier-1 cross-turn cache (#933 A.2 step 2) ---

#[test]
fn tier1_get_returns_none_for_empty_cache() {
    let cache = CompactionCache::new();
    assert!(cache.get_tier1("s1").is_none());
}

#[test]
fn tier1_put_then_get_roundtrips() {
    let cache = CompactionCache::new();
    let prefix = vec![test_msg("compacted-a"), test_msg("compacted-b")];
    cache.put_tier1("s1", 5, prefix.clone(), 10, 0);

    let (boundary, retrieved, _count, _level) = cache.get_tier1("s1").unwrap();
    assert_eq!(boundary, 5);
    assert_eq!(retrieved.len(), 2);
    assert_eq!(retrieved[0].content, "compacted-a");
    assert_eq!(retrieved[1].content, "compacted-b");
}

#[test]
fn tier1_survives_tier2_put() {
    let cache = CompactionCache::new();
    let prefix = vec![test_msg("frozen-prefix")];
    cache.put_tier1("s1", 3, prefix, 10, 0);
    // Tier-2 put should not clobber tier-1.
    cache.put("s1", 10, test_msg("summary"), 50);

    let (b, msgs, _count, _level) = cache.get_tier1("s1").unwrap();
    assert_eq!(b, 3);
    assert_eq!(msgs[0].content, "frozen-prefix");
    // Tier-2 still there too.
    let (b2, msg, count) = cache.get("s1").unwrap();
    assert_eq!(b2, 10);
    assert_eq!(msg.content, "summary");
    assert_eq!(count, 50);
}

#[test]
fn tier2_survives_tier1_put() {
    let cache = CompactionCache::new();
    cache.put("s1", 7, test_msg("summary"), 30);
    cache.put_tier1("s1", 4, vec![test_msg("prefix")], 10, 0);

    // Both present.
    assert!(cache.get("s1").is_some());
    assert!(cache.get_tier1("s1").is_some());
}

#[test]
fn invalidate_clears_both_tiers() {
    let cache = CompactionCache::new();
    cache.put("s1", 5, test_msg("sum"), 10);
    cache.put_tier1("s1", 3, vec![test_msg("pfx")], 10, 0);

    cache.invalidate("s1");
    assert!(cache.get("s1").is_none());
    assert!(cache.get_tier1("s1").is_none());
}

#[test]
fn invalidate_all_clears_tier1_too() {
    let cache = CompactionCache::new();
    cache.put_tier1("s1", 5, vec![test_msg("a")], 10, 0);
    cache.put_tier1("s2", 8, vec![test_msg("b")], 10, 0);

    cache.invalidate_all();
    assert!(cache.get_tier1("s1").is_none());
    assert!(cache.get_tier1("s2").is_none());
}

#[test]
fn tier1_put_overwrites_existing() {
    let cache = CompactionCache::new();
    cache.put_tier1("s1", 3, vec![test_msg("old")], 10, 0);
    cache.put_tier1("s1", 7, vec![test_msg("new-a"), test_msg("new-b")], 10, 0);

    let (boundary, msgs, _count, _level) = cache.get_tier1("s1").unwrap();
    assert_eq!(boundary, 7);
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].content, "new-a");
}

#[test]
fn tier1_get_returns_message_count_for_staleness_check() {
    let cache = CompactionCache::new();
    cache.put_tier1("s1", 5, vec![test_msg("pfx")], 42, 0);

    let (boundary, _prefix, count, _level) = cache.get_tier1("s1").unwrap();
    assert_eq!(boundary, 5);
    assert_eq!(count, 42);
}
