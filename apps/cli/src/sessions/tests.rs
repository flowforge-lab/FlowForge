use super::*;
use ff_core::{Session, SessionStatus};
use std::io::Cursor;

/// Build a minimal `Session` for rendering tests — only the fields `render_list`
/// and `resolve_label` read are populated.
fn test_session(id: &str, title: Option<&str>, goal: Option<&str>, updated_at: i64) -> Session {
    Session {
        id: id.to_string(),
        goal: goal.map(str::to_string),
        title: title.map(str::to_string),
        summary: None,
        status: SessionStatus::Active,
        created_at: updated_at,
        updated_at,
        phenotype: None,
        mode: None,
        workspace: None,
        model: None,
        mcp_servers: None,
    }
}

// -- resolve_label -------------------------------------------------------

#[test]
fn resolve_label_uses_title_when_present() {
    let s = test_session("s1", Some("Refactor auth"), None, 0);
    assert_eq!(resolve_label(&s), "Refactor auth");
}

#[test]
fn resolve_label_falls_back_to_goal() {
    let s = test_session("s1", None, Some("ship the feature"), 0);
    assert_eq!(resolve_label(&s), "ship the feature");
}

#[test]
fn resolve_label_falls_back_to_default() {
    let s = test_session("s1", None, None, 0);
    assert_eq!(resolve_label(&s), "New session");
}

#[test]
fn resolve_label_title_preferred_over_goal() {
    let s = test_session("s1", Some("the title"), Some("the goal"), 0);
    assert_eq!(resolve_label(&s), "the title");
}

#[test]
fn resolve_label_treats_empty_title_as_absent() {
    // The desktop TS `if (session.title)` is falsy for ""; mirror that so an
    // empty-string title (which the store never produces but could exist in
    // edge cases) falls through to goal.
    let s = test_session("s1", Some(""), Some("the goal"), 0);
    assert_eq!(resolve_label(&s), "the goal");
}

// -- strip_fork_suffix ---------------------------------------------------

#[test]
fn strip_fork_suffix_strips_a_trailing_fork_n() {
    assert_eq!(strip_fork_suffix("Refactor auth (Fork 2)"), "Refactor auth");
}

#[test]
fn strip_fork_suffix_leaves_non_fork_titles_unchanged() {
    assert_eq!(strip_fork_suffix("Refactor auth"), "Refactor auth");
    assert_eq!(strip_fork_suffix("Untitled session"), "Untitled session");
}

#[test]
fn strip_fork_suffix_requires_digits() {
    // " (Fork x)" is not a valid suffix — no digits — so the title is unchanged.
    assert_eq!(
        strip_fork_suffix("Refactor auth (Fork x)"),
        "Refactor auth (Fork x)"
    );
}

#[test]
fn strip_fork_suffix_requires_closing_paren() {
    assert_eq!(
        strip_fork_suffix("Refactor auth (Fork 1"),
        "Refactor auth (Fork 1"
    );
}

#[test]
fn strip_fork_suffix_strips_only_the_last_suffix() {
    // When a base title legitimately contains " (Fork " earlier, only the
    // trailing well-formed suffix is stripped — matching the desktop regex `$/`.
    assert_eq!(
        strip_fork_suffix("Fix (Fork 1) bug (Fork 2)"),
        "Fix (Fork 1) bug"
    );
}

// -- next_fork_title -----------------------------------------------------
// These mirror the desktop's `sessions.test.ts` cases verbatim (#1069 parity).

#[test]
fn next_fork_starts_at_1_when_no_forks_exist() {
    assert_eq!(
        next_fork_title("Refactor auth", &[]),
        "Refactor auth (Fork 1)"
    );
}

#[test]
fn next_fork_starts_at_1_when_no_matching_base() {
    let existing = [Some("Refactor auth"), Some("Other")];
    assert_eq!(
        next_fork_title("New feature", existing.as_slice()),
        "New feature (Fork 1)",
    );
}

#[test]
fn next_fork_increments_past_highest_existing() {
    let existing = [Some("Refactor auth"), Some("Refactor auth (Fork 1)")];
    assert_eq!(
        next_fork_title("Refactor auth", existing.as_slice()),
        "Refactor auth (Fork 2)",
    );
}

#[test]
fn next_fork_forking_a_fork_renumbers_from_base() {
    // Forking "Refactor auth (Fork 2)" itself strips the suffix to get the base
    // "Refactor auth", then increments past the highest existing (2) → 3.
    let existing = [
        Some("Refactor auth (Fork 1)"),
        Some("Refactor auth (Fork 2)"),
    ];
    assert_eq!(
        next_fork_title("Refactor auth (Fork 2)", existing.as_slice()),
        "Refactor auth (Fork 3)",
    );
}

#[test]
fn next_fork_ignores_unrelated_fork_suffixes() {
    let existing: [Option<&str>; 3] = [
        None,
        Some("Unrelated (Fork 5)"),
        Some("Refactor auth (Fork 1)"),
    ];
    assert_eq!(
        next_fork_title("Refactor auth", &existing),
        "Refactor auth (Fork 2)",
    );
}

#[test]
fn next_fork_handles_regex_special_chars_in_base() {
    // The desktop TS escapes the base for regex matching; the Rust port uses
    // literal `starts_with` so special chars need no escaping — same result.
    let existing = [
        Some("Fix (a+b)*c [urgent]"),
        Some("Fix (a+b)*c [urgent] (Fork 1)"),
    ];
    assert_eq!(
        next_fork_title("Fix (a+b)*c [urgent]", existing.as_slice()),
        "Fix (a+b)*c [urgent] (Fork 2)",
    );
}

#[test]
fn next_fork_ignores_null_titles() {
    let existing: [Option<&str>; 2] = [None, Some("Refactor auth (Fork 1)")];
    assert_eq!(
        next_fork_title("Refactor auth", &existing),
        "Refactor auth (Fork 2)",
    );
}

// -- format_ts -----------------------------------------------------------

#[test]
fn format_ts_renders_a_readable_date() {
    // 2025-01-01 00:00:00 UTC = epoch millis 1735689600000.
    let s = format_ts(1735689600000);
    assert!(s.contains("2025-01-01"), "expected date in {s}");
}

#[test]
fn format_ts_falls_back_to_millis_for_garbage() {
    // Out-of-range negative — chrono returns None, so the raw millis survive.
    let s = format_ts(i64::MIN);
    assert_eq!(s, i64::MIN.to_string());
}

// -- render_list ---------------------------------------------------------

#[test]
fn render_list_prints_header_then_rows() {
    let sessions = vec![
        test_session("s1", Some("First"), None, 1000),
        test_session("s2", None, Some("goal two"), 2000),
    ];
    let mut out = Cursor::new(Vec::new());
    render_list(&sessions, &mut out).unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "header + 2 rows");
    assert_eq!(lines[0], "id\tlabel\tstatus\tupdated");
    assert!(lines[1].starts_with("s1\tFirst\tactive\t"));
    assert!(lines[2].starts_with("s2\tgoal two\tactive\t"));
}

#[test]
fn render_list_prints_only_header_when_empty() {
    let mut out = Cursor::new(Vec::new());
    render_list(&[], &mut out).unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert_eq!(text, "id\tlabel\tstatus\tupdated\n");
}
