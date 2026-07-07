use super::*;

#[test]
fn strips_fenced_output() {
    assert_eq!(
        strip_code_fence("```markdown\nhello\nworld\n```"),
        "hello\nworld"
    );
    assert_eq!(strip_code_fence("```\njust text\n```"), "just text");
    assert_eq!(strip_code_fence("no fence here"), "no fence here");
}

#[test]
fn cost_scales_with_body_size() {
    let agg = SkillAggregate {
        skill: "x".to_string(),
        mean_tokens: 400.0,
        ..Default::default()
    };
    let (cur, est) = estimate_cost(Some(&agg), "aaaaaaaa", "aaaa");
    assert_eq!(cur, 400.0);
    assert_eq!(est, 200.0);
}

#[test]
fn cost_zero_without_telemetry() {
    let (cur, est) = estimate_cost(None, "aaaa", "aa");
    assert_eq!(cur, 0.0);
    assert_eq!(est, 0.0);
}

#[test]
fn messages_include_body_and_telemetry() {
    let msgs = build_messages("alpha", "do x", None, &[]);
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].role, "system");
    let user = msgs[1].content.as_deref().unwrap();
    assert!(user.contains("do x"));
    assert!(user.contains("No telemetry"));
}
