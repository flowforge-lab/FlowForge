use super::*;
use ff_core::Role;

fn msg(role: Role, content: &str) -> Message {
    Message {
        id: String::new(),
        session_id: String::new(),
        role,
        content: content.to_string(),
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
fn classify_detects_json_code_prose() {
    assert_eq!(classify(r#"{"a": 1, "b": [1,2]}"#), ContentKind::Json);
    assert_eq!(classify("[1,2,3]"), ContentKind::Json);
    // Fenced code block is unambiguous.
    assert_eq!(classify("```rust\nfn f() {}\n```"), ContentKind::Code);
    // Strong unfenced code: function keyword + a balanced brace pair.
    assert_eq!(
        classify("fn main() {\n    println!(\"hi\");\n}\n"),
        ContentKind::Code
    );
    // Prose with an incidental brace must not be misclassified.
    assert_eq!(
        classify("Choose option {1}: it is the cleanest way."),
        ContentKind::Prose
    );
    // Malformed JSON falls back to prose, not Json.
    assert_eq!(classify("{ not really json"), ContentKind::Prose);
}

#[test]
fn cache_dedupes_identical_originals() {
    let mut cache = ReversibleCache::new();
    let k1 = cache.put("hello".to_string());
    let k2 = cache.put("hello".to_string());
    assert_eq!(k1, k2);
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.retrieve(&k1), Some("hello"));
    assert_eq!(cache.retrieve("not-a-key"), None);
}

#[test]
fn small_blobs_are_left_alone() {
    // Below `min_tokens_to_compact` the compactor must not cache or mark.
    let comp = ExtractiveCompactor::default();
    let mut cache = ReversibleCache::new();
    let small = "short message";
    let out = comp.compress(small, &mut cache);
    assert_eq!(out, small);
    assert!(cache.is_empty());
}

#[test]
fn json_compression_shrinks_and_round_trips() {
    let comp = ExtractiveCompactor::default();
    let mut cache = ReversibleCache::new();
    // A pretty-printed JSON object with a long string and a long array --
    // exactly the shape of a chatty tool output.
    let big_string = "x".repeat(400);
    let big_array: Vec<i32> = (0..50).collect();
    let original = serde_json::to_string_pretty(&serde_json::json!({
        "summary": big_string,
        "items": big_array,
        "ok": true,
    }))
    .unwrap();
    let before = proxy_tokens(&original);

    let out = comp.compress(&original, &mut cache);
    assert!(
        out.len() < original.len(),
        "JSON compression must shrink: before={} after={}",
        original.len(),
        out.len()
    );
    assert!(out.contains("[compacted; retrieve key="));
    assert_eq!(cache.len(), 1, "the original must be cached for retrieval");
    let after = proxy_tokens(&out);
    assert!(
        after < before,
        "proxy tokens must decrease: before={before} after={after}"
    );

    // The retrieve key in the marker maps back to the verbatim original.
    let key = out
        .rsplit("key=")
        .next()
        .unwrap()
        .trim_end_matches(']')
        .to_string();
    assert_eq!(cache.retrieve(&key), Some(original.as_str()));
}

#[test]
fn line_elision_keeps_head_and_tail() {
    let comp = ExtractiveCompactor {
        keep_head_lines: 2,
        keep_tail_lines: 2,
        min_lines_to_elide: 6,
        min_tokens_to_compact: 0,
        ..ExtractiveCompactor::default()
    };
    let mut cache = ReversibleCache::new();
    let original = (1..=20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = comp.compress(&original, &mut cache);
    assert!(out.contains("line 1\n"));
    assert!(out.contains("line 2\n"));
    assert!(out.contains("line 19\n"));
    assert!(out.contains("line 20"));
    assert!(out.contains("<compacted lines=\"16\"/>"));
    assert!(out.contains("[compacted; retrieve key="));
    assert_eq!(cache.len(), 1);
}

#[test]
fn compact_cold_keeps_recent_verbatim() {
    let comp = ExtractiveCompactor {
        keep_head_lines: 2,
        keep_tail_lines: 2,
        min_lines_to_elide: 6,
        min_tokens_to_compact: 0,
        ..ExtractiveCompactor::default()
    };
    let mut cache = ReversibleCache::new();
    let big = (1..=30)
        .map(|i| format!("row {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let small = "ok";
    let messages = vec![
        msg(Role::User, &big),      // cold (will compress)
        msg(Role::Assistant, &big), // cold (skipped — #813)
        msg(Role::User, small),     // recent (verbatim)
    ];

    let (out, savings) = comp.compact_cold(&messages, 1, &mut cache);

    assert_eq!(out.len(), 3);
    // Cold user message was compressed.
    assert_ne!(out[0].content, big);
    assert!(out[0].content.contains("[compacted; retrieve key="));
    // Cold assistant message is NEVER compressed (#813): markers in
    // assistant-role messages cause the model to mimic them.
    assert_eq!(out[1].content, big);
    // Recent message is byte-identical.
    assert_eq!(out[2].content, small);
    // Savings reflect only the user message being compressed.
    assert!(savings.before_tokens > savings.after_tokens);
    assert!(savings.saved() > 0);
    assert!(savings.ratio() > 0.0);
    assert_eq!(savings.originals_cached, 1);
    assert_eq!(cache.len(), 1);

    // And the retrieve key in the compressed user message resolves to the
    // verbatim original.
    let key = out[0]
        .content
        .rsplit("key=")
        .next()
        .unwrap()
        .trim_end_matches(']')
        .to_string();
    assert_eq!(cache.retrieve(&key), Some(big.as_str()));
}

#[test]
fn already_minimal_content_is_not_marked() {
    // A blob that is too tight to shrink (compact JSON, short, no arrays)
    // must come back unchanged with no cache pollution.
    let comp = ExtractiveCompactor::default();
    let mut cache = ReversibleCache::new();
    let tight = serde_json::to_string(&serde_json::json!({"ok": true, "n": 1})).unwrap();
    let out = comp.compress(&tight, &mut cache);
    assert_eq!(out, tight);
    assert!(cache.is_empty());
}

#[test]
fn savings_zero_ratio_when_no_input() {
    let s = CompactionSavings {
        before_tokens: 0,
        after_tokens: 0,
        originals_cached: 0,
    };
    assert_eq!(s.saved(), 0);
    assert_eq!(s.ratio(), 0.0);
}

#[test]
fn json_string_truncation_is_unicode_safe() {
    // A JSON value with a long multi-byte string. Truncation must split on
    // a char boundary, never a byte mid-codepoint. The payload is long
    // enough that compression amortizes the marker so the cache fires.
    let comp = ExtractiveCompactor {
        max_value_chars: 5,
        min_tokens_to_compact: 0,
        ..ExtractiveCompactor::default()
    };
    let mut cache = ReversibleCache::new();
    let long = "héllo wörld 你好 こんにちは ".repeat(20);
    let original = serde_json::to_string(&serde_json::json!({ "msg": long })).unwrap();
    let out = comp.compress(&original, &mut cache);
    // Output must be valid UTF-8 (the cargo test framework would already panic
    // otherwise, but make the contract explicit) and must not contain a raw
    // codepoint replacement marker from a bad split.
    assert!(out.is_char_boundary(out.len()));
    assert!(!out.contains('\u{FFFD}'));
    // Round-trip: the marker must point back to the verbatim original.
    let key = out
        .rsplit("key=")
        .next()
        .unwrap()
        .trim_end_matches(']')
        .to_string();
    assert_eq!(cache.retrieve(&key), Some(original.as_str()));
}

#[test]
fn compress_one_returns_key_and_original_on_shrink() {
    let comp = ExtractiveCompactor {
        min_tokens_to_compact: 0,
        ..ExtractiveCompactor::default()
    };
    let big = (1..=40)
        .map(|i| format!("log line number {i} with some filler text"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = comp.compress_one(&big);
    let (key, original) = out.original.expect("a large blob must shrink and cache");
    assert_eq!(original, big, "the cached original must be byte-identical");
    assert!(
        out.text.contains(&format!("key={key}")),
        "the marker must carry the same key the caller persists"
    );
    assert!(proxy_tokens(&out.text) < proxy_tokens(&big));
}

#[test]
fn compress_one_returns_none_below_threshold() {
    let comp = ExtractiveCompactor::default();
    let small = "a short tool result";
    let out = comp.compress_one(small);
    assert_eq!(out.text, small);
    assert!(out.original.is_none());
    assert!(!out.text.contains("[compacted"));
}

#[test]
fn compress_one_key_matches_marker() {
    let comp = ExtractiveCompactor {
        max_value_chars: 8,
        min_tokens_to_compact: 0,
        ..ExtractiveCompactor::default()
    };
    let blob = serde_json::to_string(&serde_json::json!({
        "field": "x".repeat(2000)
    }))
    .unwrap();
    let out = comp.compress_one(&blob);
    let (key, _) = out.original.clone().expect("shrinks");
    let marker_key = out
        .text
        .rsplit("key=")
        .next()
        .unwrap()
        .trim_end_matches(']')
        .to_string();
    assert_eq!(marker_key, key);
}

fn msg_with_id(id: &str, role: Role, content: &str) -> Message {
    let mut m = msg(role, content);
    m.id = id.to_string();
    m
}

#[test]
fn compact_cold_collect_keeps_recent_verbatim() {
    let comp = ExtractiveCompactor {
        keep_head_lines: 2,
        keep_tail_lines: 2,
        min_lines_to_elide: 6,
        min_tokens_to_compact: 0,
        ..ExtractiveCompactor::default()
    };
    let cold_a = (1..=30)
        .map(|i| format!("a{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let cold_b = (1..=30)
        .map(|i| format!("b{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let recent = "the exact recent state";
    let messages = vec![
        msg_with_id("m0", Role::User, &cold_a),
        msg_with_id("m1", Role::Assistant, &cold_b),
        msg_with_id("m2", Role::User, recent),
    ];

    let cold = comp.compact_cold_collect(&messages, 1);

    assert_eq!(cold.messages.len(), 3);
    // Cold user message compacted.
    assert!(cold.messages[0].content.contains(COMPACTION_MARKER_PREFIX));
    // Cold assistant message is NEVER compacted (#813).
    assert_eq!(cold.messages[1].content, cold_b);
    // Recent message is byte-identical.
    assert_eq!(cold.messages[2].content, recent);
    assert!(cold.savings.saved() > 0);
}

#[test]
fn compact_cold_collect_skips_already_compacted() {
    let comp = ExtractiveCompactor {
        min_tokens_to_compact: 0,
        ..ExtractiveCompactor::default()
    };
    // A cold message that already carries the ingest-time marker must pass
    // through untouched -- no double-compaction, no original collected.
    let already = format!("short summary\n{COMPACTION_MARKER_PREFIX}deadbeefdeadbeef]");
    let messages = vec![
        msg_with_id("m0", Role::Tool, &already),
        msg_with_id("m1", Role::User, "recent"),
    ];

    let cold = comp.compact_cold_collect(&messages, 1);

    assert_eq!(cold.messages[0].content, already);
    assert!(cold.originals.is_empty());
}

#[test]
fn compact_cold_collect_collects_originals_with_message_ids() {
    let comp = ExtractiveCompactor {
        max_value_chars: 8,
        min_tokens_to_compact: 0,
        ..ExtractiveCompactor::default()
    };
    let blob = serde_json::to_string(&serde_json::json!({
        "field": "x".repeat(2000)
    }))
    .unwrap();
    let messages = vec![
        msg_with_id("cold-id", Role::Tool, &blob),
        msg_with_id("recent-id", Role::User, "recent"),
    ];

    let cold = comp.compact_cold_collect(&messages, 1);

    assert_eq!(cold.originals.len(), 1);
    let (mid, key, original) = &cold.originals[0];
    assert_eq!(mid, "cold-id");
    assert_eq!(original, &blob);
    // The collected key matches the marker emitted on the wire content.
    assert!(cold.messages[0].content.contains(&format!("key={key}]")));
    assert_eq!(cold.savings.originals_cached, 1);
}

/// #813: assistant-role messages must NEVER be compacted — markers in prior
/// assistant replies cause the model to mimic them in its own output (the
/// root cause that PR #784's format change did not fix).
#[test]
fn assistant_messages_are_never_compacted() {
    let comp = ExtractiveCompactor {
        keep_head_lines: 2,
        keep_tail_lines: 2,
        min_lines_to_elide: 6,
        min_tokens_to_compact: 0,
        ..ExtractiveCompactor::default()
    };
    let mut cache = ReversibleCache::new();
    let big = (1..=50)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    // All messages are in the cold prefix (keep_recent=1, last is recent).
    let messages = vec![
        msg(Role::Tool, &big),      // cold tool — compacted
        msg(Role::Assistant, &big), // cold assistant — SKIPPED
        msg(Role::User, &big),      // cold user — compacted
        msg(Role::User, "recent"),  // recent — verbatim
    ];

    let (out, _) = comp.compact_cold(&messages, 1, &mut cache);

    // Tool message: compacted.
    assert!(
        out[0].content.contains(COMPACTION_MARKER_PREFIX),
        "tool should be compacted"
    );
    // Assistant message: NEVER compacted, regardless of size.
    assert_eq!(out[1].content, big, "assistant must stay verbatim (#813)");
    assert!(
        !out[1].content.contains(COMPACTION_MARKER_PREFIX),
        "assistant must not contain compaction markers"
    );
    // User message: compacted.
    assert!(
        out[2].content.contains(COMPACTION_MARKER_PREFIX),
        "user should be compacted"
    );
    // Recent: verbatim.
    assert_eq!(out[3].content, "recent");

    // Also verify compact_cold_collect (the store-agnostic variant).
    let cold = comp.compact_cold_collect(&messages, 1);
    assert_eq!(
        cold.messages[1].content, big,
        "compact_cold_collect must also skip assistant (#813)"
    );
}

// --- compact_range_collect (#933 A.2) ----------------------------------------

#[test]
fn compact_range_collect_compresses_all_messages_as_cold() {
    let long_content = "x]x]x]x]x]\n".repeat(100); // 100 lines, >64 proxy tokens
    let msgs = vec![
        msg(Role::User, &long_content),
        msg(Role::User, "short"),
        msg(Role::Assistant, &long_content), // assistant = skipped
    ];
    let result = ExtractiveCompactor::default().compact_range_collect(&msgs);
    // First message (long, user) should be compacted.
    assert!(
        result.messages[0].content.contains("<compacted"),
        "long user message should be compacted"
    );
    // Short message stays unchanged.
    assert_eq!(result.messages[1].content, "short");
    // Assistant message stays unchanged (even though cold).
    assert_eq!(result.messages[2].content, long_content);
    // Originals collected for the compacted message.
    assert_eq!(result.originals.len(), 1);
}

#[test]
fn compact_range_matches_cold_collect_for_full_cold_slice() {
    let long = "line\n".repeat(50);
    let msgs = vec![
        msg(Role::User, &long),
        msg(Role::User, &long),
        msg(Role::User, "tiny"),
    ];
    // compact_cold_collect with keep_recent=0 should equal compact_range_collect
    let full = ExtractiveCompactor::default().compact_cold_collect(&msgs, 0);
    let range = ExtractiveCompactor::default().compact_range_collect(&msgs);
    assert_eq!(full.messages.len(), range.messages.len());
    for (a, b) in full.messages.iter().zip(range.messages.iter()) {
        assert_eq!(a.content, b.content);
    }
}

#[test]
fn frozen_boundary_produces_same_prefix_as_full_compaction() {
    // Simulate the frozen-boundary pattern: compact [0..5], then on a later
    // iteration compact [5..7] separately. The combined result must equal a
    // single compact_cold_collect over [0..7] with keep_recent=3 (total=10).
    let long = "data\n".repeat(60);
    let short = "ok";
    let messages: Vec<Message> = (0..10)
        .map(|i| {
            if i < 7 {
                msg(Role::User, &long)
            } else {
                msg(Role::User, short)
            }
        })
        .collect();

    let compactor = ExtractiveCompactor::default();
    let keep_recent = 3;
    let cold_end = messages.len() - keep_recent; // 7

    // Full single-pass compaction (the baseline).
    let full = compactor.compact_cold_collect(&messages, keep_recent);

    // Frozen-boundary simulation: first compact [0..5], then [5..7].
    let first_boundary = 5;
    let first_pass = compactor.compact_range_collect(&messages[..first_boundary]);
    let second_pass = compactor.compact_range_collect(&messages[first_boundary..cold_end]);

    let mut combined = Vec::new();
    combined.extend(first_pass.messages);
    combined.extend(second_pass.messages);
    combined.extend_from_slice(&messages[cold_end..]);

    assert_eq!(combined.len(), full.messages.len());
    for (i, (a, b)) in combined.iter().zip(full.messages.iter()).enumerate() {
        assert_eq!(
            a.content, b.content,
            "message {i} content mismatch between frozen-boundary and full pass"
        );
    }
}
