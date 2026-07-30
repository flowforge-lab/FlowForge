use super::*;
use serde_json::json;

#[test]
fn resolve_token_does_not_panic() {
    // resolve_token() is cached in a OnceLock, so we can only test the
    // first-call behavior once per process. On a dev machine with `gh`
    // authenticated it returns Some; on CI (no gh auth) it returns None.
    // Either outcome is valid — we verify it doesn't panic.
    let _ = resolve_token();
}

#[test]
fn safety_read_for_list_actions() {
    let tool = GithubTool;
    let args = serde_json::json!({"action": "pr_list"});
    assert_eq!(tool.safety(&args), Safety::ReadOnly);
    let args = serde_json::json!({"action": "pr_checks"});
    assert_eq!(tool.safety(&args), Safety::ReadOnly);
    let args = serde_json::json!({"action": "issue_list"});
    assert_eq!(tool.safety(&args), Safety::ReadOnly);
    // #825: the single-record read actions are ReadOnly too, so gh is usable
    // in Plan mode to read one issue's / PR's body.
    let args = serde_json::json!({"action": "issue_view"});
    assert_eq!(tool.safety(&args), Safety::ReadOnly);
    let args = serde_json::json!({"action": "pr_view"});
    assert_eq!(tool.safety(&args), Safety::ReadOnly);
}

#[test]
fn render_record_shows_body_and_metadata() {
    // issue_view: title/state/author/labels/assignees + full body.
    let v = serde_json::json!({
        "number": 42,
        "title": "Fix the thing",
        "state": "OPEN",
        "author": { "login": "octocat" },
        "labels": [{ "name": "bug" }, { "name": "backend" }],
        "assignees": [{ "login": "abid" }],
        "body": "First line.\n\nSecond paragraph."
    });
    let out = render_record(&v, "issue");
    assert!(out.starts_with("#42 [OPEN] Fix the thing"));
    assert!(out.contains("author: octocat"));
    assert!(out.contains("labels: bug, backend"));
    assert!(out.contains("assignees: abid"));
    assert!(out.contains("First line.\n\nSecond paragraph."));
}

#[test]
fn render_record_pr_shows_branches_and_stats() {
    let v = serde_json::json!({
        "number": 7,
        "title": "Add feature",
        "state": "OPEN",
        "author": { "login": "dev" },
        "baseRefName": "main",
        "headRefName": "feat/x",
        "additions": 10,
        "deletions": 2,
        "changedFiles": 3,
        "body": "PR body here."
    });
    let out = render_record(&v, "pr");
    assert!(out.starts_with("PR #7 [OPEN] Add feature"));
    assert!(out.contains("feat/x → main  +10/-2 across 3 files"));
    assert!(out.contains("PR body here."));
}

#[test]
fn render_record_handles_empty_body_and_missing_fields() {
    let v = serde_json::json!({ "title": "No body", "state": "CLOSED" });
    let out = render_record(&v, "issue");
    assert!(out.contains("[CLOSED] No body"));
    assert!(out.contains("(no description)"));
    // Missing author/labels/assignees don't emit stray lines.
    assert!(!out.contains("author:"));
    assert!(!out.contains("labels:"));
}

#[test]
fn join_field_flattens_and_tolerates_absence() {
    let labels = serde_json::json!([{ "name": "a" }, { "name": "b" }]);
    assert_eq!(join_field(Some(&labels), "name"), "a, b");
    assert_eq!(join_field(None, "name"), "");
    let empty = serde_json::json!([]);
    assert_eq!(join_field(Some(&empty), "name"), "");
}

#[test]
fn safety_write_for_mutating_actions() {
    let tool = GithubTool;
    // Chatty repo writes stay Write so Auto remains usable for them.
    for action in ["issue_create", "issue_edit", "issue_comment", "pr_comment"] {
        let args = serde_json::json!({"action": action});
        assert_eq!(
            tool.safety(&args),
            Safety::Write,
            "{action} should be Write"
        );
    }
}

#[test]
fn safety_publish_for_remote_mutations() {
    // #1051: creating/merging a PR and pushing a branch write to the remote,
    // so they carry the Publish tier (Plan denies, Auto prompts, Act allows).
    let tool = GithubTool;
    for action in ["pr_create", "pr_merge", "push"] {
        let args = serde_json::json!({"action": action});
        assert_eq!(
            tool.safety(&args),
            Safety::Publish,
            "{action} should be Publish"
        );
    }
    assert_eq!(tool.max_safety(), Safety::Publish);
}

#[test]
fn format_json_table_empty() {
    let result = format_json_table("[]", &["number", "title"]);
    assert!(result.success);
    assert_eq!(result.content, "No results.");
}

#[test]
fn format_json_table_rows() {
    let json = r#"[{"number":42,"title":"Fix bug","state":"OPEN"},{"number":43,"title":"Add feature","state":"MERGED"}]"#;
    let result = format_json_table(json, &["number", "title", "state"]);
    assert!(result.success);
    assert!(result.content.contains("42\tFix bug\tOPEN"));
    assert!(result.content.contains("43\tAdd feature\tMERGED"));
}

/// Fixture test for pr_checks output parsing — guards against requesting
/// fields that gh doesn't support (the B1 bug that broke the initial PR).
#[test]
fn pr_checks_parse_fixture() {
    // Simulates the JSON that `gh pr checks --json name,state` returns.
    let fixture = r#"[{"name":"Rust (fmt, clippy, test)","state":"SUCCESS"},{"name":"Web (typecheck, lint)","state":"SUCCESS"},{"name":"Windows (compile)","state":"FAILURE"}]"#;
    let checks: Vec<Value> = serde_json::from_str(fixture).unwrap();

    let mut lines = vec!["PR #1 checks:".to_string()];
    for check in &checks {
        let name = check.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let state = check.get("state").and_then(|v| v.as_str()).unwrap_or("?");
        let icon = match state {
            "SUCCESS" => "✓",
            "FAILURE" => "✗",
            "PENDING" | "IN_PROGRESS" => "◯",
            _ => "?",
        };
        lines.push(format!("  {icon} {name}: {state}"));
    }
    let output = lines.join("\n");

    assert!(output.contains("✓ Rust (fmt, clippy, test): SUCCESS"));
    assert!(output.contains("✗ Windows (compile): FAILURE"));
    assert!(!output.contains("conclusion"), "no conclusion field used");
}

#[test]
fn str_or_list_accepts_string_array_and_missing() {
    assert_eq!(str_or_list(&json!({"label": "bug"}), "label"), vec!["bug"]);
    assert_eq!(
        str_or_list(&json!({"label": ["bug", "frontend"]}), "label"),
        vec!["bug", "frontend"]
    );
    assert!(str_or_list(&json!({}), "label").is_empty());
    // empties and non-string items are dropped; whitespace trimmed.
    assert_eq!(
        str_or_list(&json!({"a": ["  x  ", "", 7, "y"]}), "a"),
        vec!["x", "y"]
    );
    assert!(str_or_list(&json!({"a": "   "}), "a").is_empty());
}

#[test]
fn create_flag_args_single_label_backcompat() {
    // A bare string label behaves exactly as before: one --label.
    let out = create_flag_args(&json!({"label": "bug"}), false);
    assert_eq!(out, vec!["--label", "bug"]);
}

#[test]
fn create_flag_args_multi_label_and_assignees() {
    let out = create_flag_args(
        &json!({"label": ["bug", "frontend"], "assignee": "abidkhan03"}),
        false,
    );
    assert_eq!(
        out,
        vec![
            "--label",
            "bug",
            "--label",
            "frontend",
            "--assignee",
            "abidkhan03"
        ]
    );
}

#[test]
fn create_flag_args_reviewer_only_when_included() {
    let args = json!({"assignee": ["a", "b"], "reviewer": "r"});
    // pr_create includes reviewer; issue_create does not.
    assert_eq!(
        create_flag_args(&args, true),
        vec!["--assignee", "a", "--assignee", "b", "--reviewer", "r"]
    );
    assert_eq!(
        create_flag_args(&args, false),
        vec!["--assignee", "a", "--assignee", "b"]
    );
}

#[test]
fn create_flag_args_empty_when_no_fields() {
    assert!(create_flag_args(&json!({"title": "x"}), true).is_empty());
}

#[test]
fn issue_edit_flags_includes_title() {
    let out = issue_edit_flags(&json!({"title": "Renamed"}));
    assert_eq!(out, vec!["--title", "Renamed"]);
}

#[test]
fn issue_edit_flags_includes_body_and_labels() {
    let out = issue_edit_flags(&json!({
        "body": "New body",
        "label": ["bug", "backend"],
        "assignee": "alice"
    }));
    assert_eq!(
        out,
        vec![
            "--body",
            "New body",
            "--add-label",
            "bug",
            "--add-label",
            "backend",
            "--add-assignee",
            "alice"
        ]
    );
}

#[test]
fn issue_edit_flags_empty_when_no_fields() {
    assert!(issue_edit_flags(&json!({"number": 42})).is_empty());
}

#[test]
fn pr_request_review_flags_requires_reviewer() {
    let err = pr_request_review_flags(&json!({"number": 1})).unwrap_err();
    assert!(err.contains("requires 'reviewer'"));
}

#[test]
fn pr_request_review_flags_builds_add_reviewer() {
    let out = pr_request_review_flags(&json!({"reviewer": ["alice", "bob"]})).unwrap();
    assert_eq!(
        out,
        vec!["--add-reviewer", "alice", "--add-reviewer", "bob"]
    );
}

#[test]
fn pr_list_flags_includes_author_and_label() {
    let out = pr_list_flags(&json!({"author": "@me", "label": ["bug", "urgent"]}));
    assert_eq!(
        out,
        vec!["--author", "@me", "--label", "bug", "--label", "urgent"]
    );
}

#[test]
fn pr_list_flags_empty_when_no_fields() {
    assert!(pr_list_flags(&json!({"limit": 10})).is_empty());
}

#[test]
fn inline_review_payload_builds_comments_and_defaults_side() {
    let args = json!({
        "event": "comment",
        "body": "overall looks good",
        "comments": [
            { "path": "src/a.rs", "line": 12, "body": "nit here" },
            { "path": "src/b.rs", "line": 40, "side": "LEFT", "start_line": 38, "body": "range" }
        ]
    });
    let p = build_inline_review_payload(&args).unwrap();
    assert_eq!(p["event"], "COMMENT", "event upper-cased");
    assert_eq!(p["body"], "overall looks good");
    assert_eq!(p["comments"][0]["side"], "RIGHT", "side defaults to RIGHT");
    assert_eq!(p["comments"][0]["path"], "src/a.rs");
    assert_eq!(p["comments"][0]["line"], 12);
    assert_eq!(p["comments"][1]["side"], "LEFT");
    assert_eq!(p["comments"][1]["start_line"], 38);
    assert!(p["comments"][0].get("start_line").is_none());
}

#[test]
fn inline_review_payload_rejects_bad_event_and_empty_comments() {
    let bad_event = json!({"event": "LGTM", "comments": [{"path":"a","line":1,"body":"x"}]});
    assert!(build_inline_review_payload(&bad_event).is_err());

    let no_comments = json!({"event": "COMMENT", "comments": []});
    assert!(build_inline_review_payload(&no_comments).is_err());

    let missing = json!({"event": "COMMENT"});
    assert!(build_inline_review_payload(&missing).is_err());
}

#[test]
fn inline_review_payload_rejects_incomplete_comment() {
    // missing body
    let a = json!({"comments": [{"path": "a.rs", "line": 3}]});
    assert!(build_inline_review_payload(&a).is_err());
    // missing line
    let b = json!({"comments": [{"path": "a.rs", "body": "x"}]});
    assert!(build_inline_review_payload(&b).is_err());
    // missing path
    let c = json!({"comments": [{"line": 3, "body": "x"}]});
    assert!(build_inline_review_payload(&c).is_err());
}

#[test]
fn inline_review_comment_and_request_changes_require_body() {
    // COMMENT / REQUEST_CHANGES 422 without a top-level body, so building the
    // payload must fail up front (mirrors the pr_review guard). A blank body
    // counts as missing.
    for event in ["COMMENT", "REQUEST_CHANGES"] {
        let blank =
            json!({"event": event, "body": "   ", "comments": [{"path":"a","line":1,"body":"y"}]});
        assert!(
            build_inline_review_payload(&blank).is_err(),
            "{event} with a blank body should be rejected"
        );

        let missing = json!({"event": event, "comments": [{"path":"a","line":1,"body":"y"}]});
        assert!(
            build_inline_review_payload(&missing).is_err(),
            "{event} with no body should be rejected"
        );
    }
}

#[test]
fn inline_review_approve_may_omit_body() {
    // APPROVE is the one event GitHub accepts without a top-level body.
    let args = json!({"event":"APPROVE","comments":[{"path":"a","line":1,"body":"y"}]});
    let p = build_inline_review_payload(&args).unwrap();
    assert_eq!(p["event"], "APPROVE");
    assert!(p.get("body").is_none(), "APPROVE omits an absent body");
}

// -----------------------------------------------------------------------
// #853 — pr_reviews / pr_review_comments rendering & grouping
// -----------------------------------------------------------------------

#[test]
fn safety_read_for_pr_review_read_actions() {
    // Both new actions are pure reads (#853): they inherit Plan-mode
    // availability from #846 without a gating change.
    let tool = GithubTool;
    for action in ["pr_reviews", "pr_review_comments"] {
        let args = serde_json::json!({"action": action});
        assert_eq!(
            tool.safety(&args),
            Safety::ReadOnly,
            "{action} should be ReadOnly"
        );
    }
}

#[test]
fn trim_diff_hunk_passes_through_under_limit() {
    let hunk = "@@ fn login() { @@\n-   a\n+   b\n c\n";
    assert_eq!(trim_diff_hunk(hunk), hunk.trim_end_matches('\n'));
}

#[test]
fn trim_diff_hunk_truncates_long_hunks() {
    let lines: Vec<String> = (0..50).map(|i| format!("  line {i}")).collect();
    let hunk = lines.join("\n");
    let out = trim_diff_hunk(&hunk);
    assert!(out.contains("…(diff hunk trimmed)"));
    // 6 lines + the marker, not 50.
    assert!(out.lines().count() <= 7);
}

#[test]
fn trim_diff_hunk_caps_by_char_count() {
    // 5 lines, each 200 chars = 1000 chars total → must truncate.
    let lines: Vec<String> = (0..5)
        .map(|i| format!("  line {i} {}", "x".repeat(200)))
        .collect();
    let hunk = lines.join("\n");
    let out = trim_diff_hunk(&hunk);
    assert!(out.contains("…(diff hunk trimmed)"));
    assert!(out.chars().count() < 600);
}

#[test]
fn render_reviews_empty_friendly_message() {
    assert_eq!(render_reviews(42, &[]), "PR #42 has no reviews yet.");
}

#[test]
fn render_reviews_orders_newest_first_and_handles_missing_fields() {
    let reviews = vec![
        json!({
            "id": 1,
            "state": "APPROVED",
            "user": { "login": "alice" },
            "submittedAt": "2025-01-10T10:00:00Z",
            "body": "LGTM."
        }),
        json!({
            "id": 2,
            "state": "CHANGES_REQUESTED",
            "user": { "login": "bob" },
            "submittedAt": "2025-01-12T10:00:00Z"
            // body absent
        }),
        json!({
            "id": 3,
            "state": "COMMENTED",
            "user": { "login": "carol" },
            "submittedAt": "2025-01-11T10:00:00Z",
            "body": "  trimmed me  "
        }),
    ];
    let out = render_reviews(7, &reviews);
    assert!(out.starts_with("Reviews on PR #7 (3)"));
    // Newest first: bob (Jan 12), carol (Jan 11), alice (Jan 10).
    let bob = out.find("[CHANGES_REQUESTED] bob").expect("bob present");
    let carol = out.find("[COMMENTED] carol").expect("carol present");
    let alice = out.find("[APPROVED] alice").expect("alice present");
    assert!(bob < carol);
    assert!(carol < alice);
    // Missing body becomes "(no description)".
    assert!(out.contains("(no description)"));
    // Body is trimmed of surrounding whitespace.
    assert!(out.contains("trimmed me"));
    assert!(!out.contains("  trimmed me  \n"));
}

#[test]
fn render_reviews_tolerates_unknown_state_and_missing_author() {
    let reviews = vec![json!({
        "id": 1,
        "state": "PENDING_DEFINITELY_NOT_A_REAL_STATE",
        "submittedAt": "2025-01-10T10:00:00Z",
        "body": "hi"
    })];
    let out = render_reviews(9, &reviews);
    assert!(out.contains("[PENDING_DEFINITELY_NOT_A_REAL_STATE] ?"));
}

#[test]
fn group_review_comments_empty_input() {
    assert!(group_review_comments(&[]).is_empty());
}

#[test]
fn group_review_comments_buckets_by_path_and_threads_by_in_reply_to_id() {
    let comments = vec![
        // Two roots on the same file, each with one reply.
        json!({
            "id": 10, "path": "src/a.rs", "line": 5, "side": "RIGHT",
            "user": { "login": "alice" },
            "createdAt": "2025-01-10T10:00:00Z",
            "body": "first thread root"
        }),
        json!({
            "id": 11, "path": "src/a.rs", "line": 20, "side": "RIGHT",
            "user": { "login": "bob" },
            "createdAt": "2025-01-10T11:00:00Z",
            "in_reply_to_id": 10,
            "body": "first thread reply"
        }),
        json!({
            "id": 20, "path": "src/a.rs", "line": 100, "side": "RIGHT",
            "user": { "login": "carol" },
            "createdAt": "2025-01-11T10:00:00Z",
            "body": "second thread root"
        }),
        json!({
            "id": 21, "path": "src/a.rs", "line": 100, "side": "RIGHT",
            "user": { "login": "dan" },
            "createdAt": "2025-01-11T11:00:00Z",
            "in_reply_to_id": 20,
            "body": "second thread reply"
        }),
        // A separate file: surfaces as a second bucket in insertion order.
        json!({
            "id": 30, "path": "src/b.rs", "line": 1, "side": "RIGHT",
            "user": { "login": "eve" },
            "createdAt": "2025-01-09T10:00:00Z",
            "body": "b thread"
        }),
    ];
    let grouped = group_review_comments(&comments);
    assert_eq!(grouped.len(), 2);
    // First bucket is the file that appeared first in the input.
    assert_eq!(grouped[0].0, "src/a.rs");
    assert_eq!(grouped[1].0, "src/b.rs");
    // a.rs has two threads.
    assert_eq!(grouped[0].1.len(), 2);
    // Each thread is [root, reply] in id order.
    assert_eq!(grouped[0].1[0][0]["id"], 10);
    assert_eq!(grouped[0].1[0][1]["id"], 11);
    assert_eq!(grouped[0].1[1][0]["id"], 20);
    assert_eq!(grouped[0].1[1][1]["id"], 21);
    // b.rs has one thread.
    assert_eq!(grouped[1].1.len(), 1);
    assert_eq!(grouped[1].1[0][0]["id"], 30);
}

#[test]
fn group_review_comments_keeps_orphan_replies_visible() {
    // Reply whose parent isn't in the set (filtered or never loaded) —
    // should still surface as its own thread so it isn't silently lost.
    let comments = vec![json!({
        "id": 99,
        "path": "src/c.rs",
        "line": 3,
        "user": { "login": "ghost" },
        "createdAt": "2025-01-10T10:00:00Z",
        "in_reply_to_id": 1234,
        "body": "lost parent"
    })];
    let grouped = group_review_comments(&comments);
    assert_eq!(grouped.len(), 1);
    assert_eq!(grouped[0].0, "src/c.rs");
    assert_eq!(grouped[0].1.len(), 1);
    assert_eq!(grouped[0].1[0][0]["id"], 99);
}

#[test]
fn group_review_comments_depth_three_chain_stays_visible() {
    // Document the depth-3 contract. GitHub's inline-review API flattens
    // reply chains so every reply's `in_reply_to_id` points at the thread
    // root, not the previous comment — real chains are depth-2 relative to
    // the root and attach via `thread_for` in one pass. If a depth-3 chain
    // ever shows up (custom tooling, a future API change), the reply whose
    // parent is itself a reply never gets inserted as a root and the
    // grandchild falls into the orphan sweep. That sweep groups orphans by
    // their own `in_reply_to_id`, so a chain of *siblings* still collapses
    // into one thread — but a chain of *descendants* (each pointing at the
    // previous one) ends up split: A+B stay together, C surfaces as its own
    // synthetic thread. Either way nothing is silently dropped, which is
    // the property the orphan sweep actually guarantees.
    let comments = vec![
        json!({
            "id": 1, "path": "src/a.rs", "line": 5, "side": "RIGHT",
            "user": { "login": "alice" },
            "createdAt": "2025-01-10T10:00:00Z",
            "body": "A: root"
        }),
        json!({
            "id": 2, "path": "src/a.rs", "line": 5, "side": "RIGHT",
            "user": { "login": "bob" },
            "createdAt": "2025-01-10T10:01:00Z",
            "in_reply_to_id": 1,
            "body": "B: reply to A"
        }),
        json!({
            "id": 3, "path": "src/a.rs", "line": 5, "side": "RIGHT",
            "user": { "login": "carol" },
            "createdAt": "2025-01-10T10:02:00Z",
            "in_reply_to_id": 2,
            "body": "C: reply to B (depth-3)"
        }),
    ];
    let grouped = group_review_comments(&comments);
    assert_eq!(grouped.len(), 1);
    assert_eq!(grouped[0].0, "src/a.rs");
    // A+B attach via thread_for; C falls into the orphan sweep and becomes
    // its own synthetic thread. Both threads surface — no comment is lost.
    assert_eq!(grouped[0].1.len(), 2);
    assert_eq!(grouped[0].1[0][0]["id"], 1);
    assert_eq!(grouped[0].1[0][1]["id"], 2);
    assert_eq!(grouped[0].1[1][0]["id"], 3);
}

#[test]
fn render_review_comments_empty_friendly_message() {
    assert_eq!(
        render_review_comments(42, &[]),
        "PR #42 has no review comments."
    );
}

#[test]
fn render_review_comments_renders_thread_with_diff_hunk_and_reply() {
    let comments = vec![
        json!({
            "id": 10,
            "path": "src/auth.rs",
            "line": 42,
            "side": "RIGHT",
            "diff_hunk": "@@ fn login() { @@\n-   a\n+   b\n",
            "user": { "login": "alice" },
            "createdAt": "2025-01-10T10:00:00Z",
            "body": "Consider validating the token."
        }),
        json!({
            "id": 11,
            "path": "src/auth.rs",
            "line": 42,
            "side": "RIGHT",
            "user": { "login": "bob" },
            "createdAt": "2025-01-10T11:00:00Z",
            "in_reply_to_id": 10,
            "body": "Fixed in abc1234."
        }),
    ];
    let out = render_review_comments(779, &comments);
    assert!(out.starts_with("Review comments on PR #779 (2 comments in 1 threads)"));
    assert!(out.contains("── src/auth.rs ──"));
    assert!(out.contains("• alice @ line 42 (RIGHT)"));
    assert!(out.contains("```diff"));
    assert!(out.contains("Consider validating the token."));
    assert!(out.contains("↳ bob (reply)"));
    assert!(out.contains("Fixed in abc1234."));
}

#[test]
fn render_review_comments_tolerates_missing_diff_hunk_and_body() {
    let comments = vec![json!({
        "id": 1,
        "path": "src/x.rs",
        "line": 1,
        "side": "RIGHT",
        "user": { "login": "alice" },
        "createdAt": "2025-01-10T10:00:00Z"
    })];
    let out = render_review_comments(1, &comments);
    assert!(out.contains("• alice @ line 1 (RIGHT)"));
    // No diff_hunk → no code fence.
    assert!(!out.contains("```diff"));
    // Missing body → "(no description)".
    assert!(out.contains("(no description)"));
}

#[test]
fn render_review_comments_falls_back_to_original_line() {
    // A stale review comment anchored to the original diff side has no
    // `line` but has `original_line` — render that gracefully.
    let comments = vec![json!({
        "id": 1,
        "path": "src/x.rs",
        "original_line": 17,
        "side": "LEFT",
        "user": { "login": "alice" },
        "createdAt": "2025-01-10T10:00:00Z",
        "body": "stale anchor"
    })];
    let out = render_review_comments(1, &comments);
    assert!(out.contains("original line 17 (LEFT)"));
}

#[test]
fn github_action_params_coherent_with_schema() {
    // RFC 0024 Phase 2B (#1162): the declaration must match the schema it prunes.
    // Adding an action to the enum without declaring its parameters fails here.
    crate::registry::assert_action_params_coherent(&GithubTool);
}

#[test]
fn github_action_params_cover_known_dispatch_reads() {
    // Closes the gap `assert_action_params_coherent` cannot see: a property
    // omitted from one action while another still claims it. The orphan check
    // stays silent there, because the ground truth is in the dispatch code.
    //
    // Each pair below is a parameter the dispatch path provably reads — verified
    // against the code, not against the property descriptions. Three of these are
    // cases where the descriptions are wrong today (#1161), so a future author who
    // "fixes" the declaration to match the prose breaks this test instead of
    // silently deleting a capability.
    let declared = GithubTool
        .action_params()
        .expect("github declares action_params");
    let required: &[(&str, &str)] = &[
        // issue_edit reads `title` at github.rs:630 and passes --title, but the
        // description for `title` names only (pr_create, issue_create). #1161.
        ("issue_edit", "title"),
        // pr_request_review reads `reviewer` at :402 and errors without it; the
        // description names only (pr_create). #1161.
        ("pr_request_review", "reviewer"),
        // pr_merge reads `delete_branch` at :264-271 via a multi-line .get(),
        // which single-line source scanning misses. #1161.
        ("pr_merge", "delete_branch"),
        // Posting a comment forwards to comment_on, which reads `body`. An early
        // single-level probe reported pr_comment as needing no parameters at all.
        ("pr_comment", "body"),
        ("issue_comment", "body"),
        // pr_review_inline forwards to build_inline_review_payload, which reads
        // `comments` — a second-level forward.
        ("pr_review_inline", "comments"),
        ("pr_review_inline", "event"),
        // create_flag_args(args, true) supplies reviewer only for pr_create.
        ("pr_create", "reviewer"),
        ("pr_create", "label"),
        ("issue_create", "label"),
        // Every numbered action needs the number it acts on.
        ("pr_view", "number"),
        ("pr_checks", "number"),
        ("push", "force"),
    ];
    for (action, param) in required {
        let params = declared
            .get(action)
            .unwrap_or_else(|| panic!("action {action:?} missing from action_params"));
        assert!(
            params.contains(param),
            "action {action:?} reads {param:?} in its dispatch path but does not declare it — \
             pruning would remove it from the schema and the capability would vanish silently"
        );
    }
}

#[test]
fn github_action_params_match_the_dispatch_code_exactly() {
    // The sampled test above only asserts that specific pairs are *present*, so it
    // cannot see a parameter a handler starts reading later. #1163 did exactly that:
    // `pr_list` gained `--label` (bringing it in line with `issue_list`, whose
    // description had always claimed the filter) while this declaration still said
    // `["author", "limit"]`. Pruning would then have dropped `label` from the
    // advertised schema and the filter would have become unreachable — no error, the
    // model simply never passes it.
    //
    // `assert_action_params_coherent` is structurally blind to that: it checks
    // declared-⊆-schema and no-orphans, and `label` stays non-orphaned via four other
    // actions. So the only defence is an exact set per action, transcribed from the
    // dispatch code. Adding a read without updating this fails here.
    let declared = GithubTool
        .action_params()
        .expect("github declares action_params");
    let expected: &[(&str, &[&str])] = &[
        (
            "pr_create",
            &[
                "title", "body", "base", "head", "label", "assignee", "reviewer",
            ],
        ),
        ("pr_list", &["author", "label", "limit"]),
        ("pr_view", &["number", "diff"]),
        ("pr_reviews", &["number"]),
        ("pr_review_comments", &["number"]),
        ("pr_merge", &["number", "squash", "delete_branch"]),
        ("pr_checks", &["number"]),
        ("pr_review", &["number", "body", "event"]),
        ("pr_comment", &["number", "body"]),
        ("pr_request_review", &["number", "reviewer"]),
        ("pr_review_inline", &["number", "body", "event", "comments"]),
        ("issue_create", &["title", "body", "label", "assignee"]),
        (
            "issue_edit",
            &["number", "title", "body", "label", "assignee"],
        ),
        ("issue_list", &["label", "limit"]),
        ("issue_view", &["number"]),
        ("issue_comment", &["number", "body"]),
        ("push", &["force"]),
    ];

    let mut declared_names: Vec<&str> = declared.keys().copied().collect();
    let mut expected_names: Vec<&str> = expected.iter().map(|(a, _)| *a).collect();
    declared_names.sort_unstable();
    expected_names.sort_unstable();
    assert_eq!(
        declared_names, expected_names,
        "the set of declared actions changed; transcribe the new action's reads from \
         its dispatch code rather than from the parameter descriptions (#1161)"
    );

    for (action, want) in expected {
        let mut got: Vec<&str> = declared
            .get(action)
            .unwrap_or_else(|| panic!("action {action:?} missing from action_params"))
            .to_vec();
        let mut want = want.to_vec();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(
            got, want,
            "action {action:?} declares a different parameter set than its dispatch \
             code reads; a missing entry is pruned away silently, an extra one wastes \
             the bytes this phase exists to save"
        );
    }
}
