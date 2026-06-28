//! Headless-safe approval gate for scheduled fires (RFC 0017 §3, §3.1). A fire
//! has no interactive surface, so the policy is static: read-only always runs,
//! a write runs only when the task's ceiling permits it, and a dangerous call
//! is never auto-approved. An `ask_user` call cannot be answered, so instead of
//! continuing on a dismissed (`None`) answer the approver latches a flag the
//! runner reads to record [`RunStatus::NeedsAttention`](ff_core::RunStatus).

use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use ff_agent::Approver;
use ff_core::SafetyCeiling;
use ff_tools::Safety;

/// Approves tool calls for a single scheduled fire under a fixed safety ceiling.
pub struct ScheduledApprover {
    ceiling: SafetyCeiling,
    /// Latches `true` the first time the turn calls `ask_user`. The runner reads
    /// it after the turn to distinguish a fire that stopped to surface a question
    /// from one that completed.
    needs_attention: AtomicBool,
}

impl ScheduledApprover {
    pub fn new(ceiling: SafetyCeiling) -> Self {
        Self {
            ceiling,
            needs_attention: AtomicBool::new(false),
        }
    }

    /// `true` if the turn called `ask_user` (which a headless fire cannot answer).
    pub fn needs_attention(&self) -> bool {
        self.needs_attention.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Approver for ScheduledApprover {
    async fn approve(
        &self,
        _message_id: &str,
        _call_id: &str,
        _name: &str,
        safety: Safety,
        _args: &serde_json::Value,
    ) -> bool {
        match safety {
            // The loop short-circuits read-only calls before consulting the
            // approver; allow defensively in case a future caller does not.
            Safety::ReadOnly => true,
            // A write runs only when the task opted into the write ceiling.
            Safety::Write => self.ceiling == SafetyCeiling::Write,
            // A dangerous call is never auto-approved in a headless fire,
            // regardless of the ceiling.
            Safety::Dangerous => false,
        }
    }

    async fn ask(
        &self,
        _message_id: &str,
        _call_id: &str,
        _args: &serde_json::Value,
    ) -> Option<String> {
        // No interactive surface: latch the flag and dismiss the question so the
        // loop unwinds to a tool result rather than hanging. The runner maps the
        // latched flag to `RunStatus::NeedsAttention`.
        self.needs_attention.store(true, Ordering::SeqCst);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> serde_json::Value {
        serde_json::json!({})
    }

    #[tokio::test]
    async fn read_only_ceiling_allows_read_only_only() {
        let a = ScheduledApprover::new(SafetyCeiling::ReadOnly);
        assert!(a.approve("m", "c", "t", Safety::ReadOnly, &args()).await);
        assert!(!a.approve("m", "c", "t", Safety::Write, &args()).await);
        assert!(!a.approve("m", "c", "t", Safety::Dangerous, &args()).await);
    }

    #[tokio::test]
    async fn write_ceiling_allows_write_but_not_dangerous() {
        let a = ScheduledApprover::new(SafetyCeiling::Write);
        assert!(a.approve("m", "c", "t", Safety::ReadOnly, &args()).await);
        assert!(a.approve("m", "c", "t", Safety::Write, &args()).await);
        assert!(!a.approve("m", "c", "t", Safety::Dangerous, &args()).await);
    }

    #[tokio::test]
    async fn dangerous_is_never_approved_at_any_ceiling() {
        for ceiling in [SafetyCeiling::ReadOnly, SafetyCeiling::Write] {
            let a = ScheduledApprover::new(ceiling);
            assert!(!a.approve("m", "c", "t", Safety::Dangerous, &args()).await);
        }
    }

    #[tokio::test]
    async fn ask_dismisses_and_latches_needs_attention() {
        let a = ScheduledApprover::new(SafetyCeiling::ReadOnly);
        assert!(!a.needs_attention());
        assert_eq!(a.ask("m", "c", &args()).await, None);
        assert!(a.needs_attention());
    }

    #[tokio::test]
    async fn approving_a_call_does_not_set_needs_attention() {
        let a = ScheduledApprover::new(SafetyCeiling::Write);
        let _ = a.approve("m", "c", "t", Safety::Write, &args()).await;
        assert!(!a.needs_attention());
    }
}
