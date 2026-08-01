use ff_agent::{ApprovalOutcome, Approver, DenyReason};
use ff_scheduled::ScheduledApprover;

#[test]
fn scheduled_approver_respects_read_only_ceiling() {
    let a = ScheduledApprover::new(ff_core::SafetyCeiling::ReadOnly);
    // Read-only should be allowed
    let outcome = tokio::runtime::Runtime::new().unwrap().block_on(async {
        a.approve(
            "m",
            "c",
            "t",
            ff_tools::Safety::ReadOnly,
            &serde_json::json!({}),
        )
        .await
    });
    assert!(matches!(outcome, ApprovalOutcome::Allowed));
    // Write should be denied under read_only
    let outcome = tokio::runtime::Runtime::new().unwrap().block_on(async {
        a.approve(
            "m",
            "c",
            "t",
            ff_tools::Safety::Write,
            &serde_json::json!({}),
        )
        .await
    });
    assert!(matches!(
        outcome,
        ApprovalOutcome::Denied(DenyReason::NoInteractiveTerminal)
    ));
}

#[test]
fn scheduled_approver_allows_write_under_write_ceiling() {
    let a = ScheduledApprover::new(ff_core::SafetyCeiling::Write);
    let outcome = tokio::runtime::Runtime::new().unwrap().block_on(async {
        a.approve(
            "m",
            "c",
            "t",
            ff_tools::Safety::Write,
            &serde_json::json!({}),
        )
        .await
    });
    assert!(matches!(outcome, ApprovalOutcome::Allowed));
    // Dangerous still denied even under write
    let outcome = tokio::runtime::Runtime::new().unwrap().block_on(async {
        a.approve(
            "m",
            "c",
            "t",
            ff_tools::Safety::Dangerous,
            &serde_json::json!({}),
        )
        .await
    });
    assert!(matches!(
        outcome,
        ApprovalOutcome::Denied(DenyReason::NoInteractiveTerminal)
    ));
}
