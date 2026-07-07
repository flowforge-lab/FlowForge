use super::{ApprovalDecision, ApprovalMode, CliApprover, InputMode};
use ff_agent::Approver;
use ff_core::Mode;
use ff_tools::Safety;

#[test]
fn approval_decision_matrix_for_non_read_only_calls() {
    use ApprovalDecision::{Allow, Deny, Prompt};
    use ApprovalMode::{Deny as AutoDeny, Prompt as DefaultPrompt, Yes};
    use InputMode::{Piped, Tty};
    use Safety::{Dangerous, Write};

    let cases = [
        (Tty, Yes, Write, Allow),
        (Tty, Yes, Dangerous, Allow),
        (Piped, Yes, Write, Allow),
        (Piped, Yes, Dangerous, Allow),
        (Tty, AutoDeny, Write, Deny),
        (Tty, AutoDeny, Dangerous, Deny),
        (Piped, AutoDeny, Write, Deny),
        (Piped, AutoDeny, Dangerous, Deny),
        (Tty, DefaultPrompt, Write, Prompt),
        (Tty, DefaultPrompt, Dangerous, Prompt),
        (Piped, DefaultPrompt, Write, Deny),
        (Piped, DefaultPrompt, Dangerous, Deny),
    ];

    for (input, policy, safety, want) in cases {
        assert_eq!(
            CliApprover::decide(Mode::Act, policy, input, safety),
            want,
            "input={input:?} policy={policy:?} safety={safety:?}"
        );
    }
}

#[test]
fn auto_mode_auto_approves_write_but_still_gates_dangerous() {
    assert_eq!(
        CliApprover::decide(
            Mode::Auto,
            ApprovalMode::Prompt,
            InputMode::Tty,
            Safety::Write
        ),
        ApprovalDecision::Allow
    );
    assert_eq!(
        CliApprover::decide(
            Mode::Auto,
            ApprovalMode::Prompt,
            InputMode::Piped,
            Safety::Write
        ),
        ApprovalDecision::Allow
    );
    assert_eq!(
        CliApprover::decide(
            Mode::Auto,
            ApprovalMode::Prompt,
            InputMode::Tty,
            Safety::Dangerous
        ),
        ApprovalDecision::Prompt
    );
    assert_eq!(
        CliApprover::decide(
            Mode::Auto,
            ApprovalMode::Prompt,
            InputMode::Piped,
            Safety::Dangerous
        ),
        ApprovalDecision::Deny
    );
}

#[test]
fn auto_mode_treats_sensitive_like_write() {
    assert_eq!(
        CliApprover::decide(
            Mode::Auto,
            ApprovalMode::Prompt,
            InputMode::Tty,
            Safety::Sensitive
        ),
        ApprovalDecision::Allow
    );
    assert_eq!(
        CliApprover::decide(
            Mode::Act,
            ApprovalMode::Prompt,
            InputMode::Tty,
            Safety::Sensitive
        ),
        ApprovalDecision::Prompt
    );
}

#[test]
fn explicit_policy_wins_over_auto_write_carve_out() {
    use InputMode::{Piped, Tty};
    assert_eq!(
        CliApprover::decide(Mode::Auto, ApprovalMode::Deny, Tty, Safety::Write),
        ApprovalDecision::Deny
    );
    assert_eq!(
        CliApprover::decide(Mode::Auto, ApprovalMode::Deny, Piped, Safety::Write),
        ApprovalDecision::Deny
    );
    assert_eq!(
        CliApprover::decide(Mode::Auto, ApprovalMode::Yes, Piped, Safety::Write),
        ApprovalDecision::Allow
    );
}

#[test]
fn act_mode_does_not_auto_approve_write() {
    assert_eq!(
        CliApprover::decide(
            Mode::Act,
            ApprovalMode::Prompt,
            InputMode::Tty,
            Safety::Write
        ),
        ApprovalDecision::Prompt
    );
}

#[test]
fn read_only_is_allowed_even_if_the_approver_is_called() {
    for input in [InputMode::Tty, InputMode::Piped] {
        for policy in [ApprovalMode::Prompt, ApprovalMode::Yes, ApprovalMode::Deny] {
            for agent_mode in [Mode::Plan, Mode::Act, Mode::Auto] {
                assert_eq!(
                    CliApprover::decide(agent_mode, policy, input, Safety::ReadOnly),
                    ApprovalDecision::Allow
                );
            }
        }
    }
}

#[test]
fn dangerous_calls_are_never_auto_allowed_without_an_explicit_flag() {
    for agent_mode in [Mode::Plan, Mode::Act, Mode::Auto] {
        assert_ne!(
            CliApprover::decide(
                agent_mode,
                ApprovalMode::Prompt,
                InputMode::Tty,
                Safety::Dangerous
            ),
            ApprovalDecision::Allow
        );
        assert_eq!(
            CliApprover::decide(
                agent_mode,
                ApprovalMode::Prompt,
                InputMode::Piped,
                Safety::Dangerous
            ),
            ApprovalDecision::Deny
        );
    }
}

#[tokio::test]
async fn was_denied_is_set_when_a_call_is_denied_by_policy() {
    let approver = CliApprover::new(ApprovalMode::Deny, Mode::Act);
    let approved = approver
        .approve(
            "msg",
            "call",
            "test_tool",
            Safety::Dangerous,
            &serde_json::json!({}),
        )
        .await;
    assert!(!approved, "a dangerous call under --deny should be denied");
    assert!(
        approver.was_denied(),
        "was_denied() must be true after a --deny denial"
    );
}

#[tokio::test]
async fn was_denied_stays_false_when_a_call_is_allowed_by_yes() {
    let approver = CliApprover::new(ApprovalMode::Yes, Mode::Act);
    let approved = approver
        .approve(
            "msg",
            "call",
            "test_tool",
            Safety::Dangerous,
            &serde_json::json!({}),
        )
        .await;
    assert!(approved, "a dangerous call under --yes should be allowed");
    assert!(
        !approver.was_denied(),
        "was_denied() must stay false when the call is allowed"
    );
}
