use ff_agent::Approver;
use ff_scheduled::ScheduledApprover;

#[test]
fn scheduled_approver_respects_read_only_ceiling() {
    let a = ScheduledApprover::new(ff_core::SafetyCeiling::ReadOnly);
    // Read-only should be allowed
    assert!(tokio::runtime::Runtime::new().unwrap().block_on(async {
        a.approve(
            "m",
            "c",
            "t",
            ff_tools::Safety::ReadOnly,
            &serde_json::json!({}),
        )
        .await
    }));
    // Write should be denied under read_only
    assert!(!tokio::runtime::Runtime::new().unwrap().block_on(async {
        a.approve(
            "m",
            "c",
            "t",
            ff_tools::Safety::Write,
            &serde_json::json!({}),
        )
        .await
    }));
}

#[test]
fn scheduled_approver_allows_write_under_write_ceiling() {
    let a = ScheduledApprover::new(ff_core::SafetyCeiling::Write);
    assert!(tokio::runtime::Runtime::new().unwrap().block_on(async {
        a.approve(
            "m",
            "c",
            "t",
            ff_tools::Safety::Write,
            &serde_json::json!({}),
        )
        .await
    }));
    // Dangerous still denied even under write
    assert!(!tokio::runtime::Runtime::new().unwrap().block_on(async {
        a.approve(
            "m",
            "c",
            "t",
            ff_tools::Safety::Dangerous,
            &serde_json::json!({}),
        )
        .await
    }));
}
