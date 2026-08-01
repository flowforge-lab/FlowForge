use async_trait::async_trait;
use ff_agent::{ApprovalOutcome, Approver, DenyReason};
use ff_core::Mode;
use ff_tools::Safety;

/// Approval policy for messaging transports (#911, RFC 0021 SS7).
///
/// Unlike the desktop (which prompts the user) or the CLI (which reads stdin),
/// a messaging transport has no interactive approval surface during a turn.
/// Policy:
/// - **Act mode**: auto-approve Read/Write/Sensitive; deny Dangerous (no
///   interactive surface to confirm).
/// - **Auto mode**: collapses to Act behavior. ff-core defines Auto as "prompt
///   on Sensitive", but prompting requires an interactive surface; since
///   messaging is unattended, we auto-approve Sensitive (the user opted in by
///   sending a message). This is intentional.
/// - **Plan mode**: only Read is allowed (everything else denied).
///
/// # This policy ignores the permission matrix
///
/// The decision above is derived from `mode` and `safety` alone. It never calls
/// [`ff_core::PermissionMatrix::effective_cell`], so for a messaging-triggered
/// agent the user's configured Deny cells, allowlist, and scoped rules have **no
/// effect**. A tool the user explicitly denied in the control panel still runs
/// here if this policy's coarse `mode`/`safety` check happens to allow it.
///
/// That is safe only because the policy is strictly more conservative than the
/// matrix in the tiers that matter (Plan allows nothing but reads; Publish and
/// Dangerous are always denied) — it under-approves rather than over-approves.
/// It is still a divergence, and a Deny cell that silently does nothing is the
/// kind of gap that reads as a bug when someone finds it from the other side.
///
/// #1059 replaces this for Slack with an interactive approver that runs the real
/// gate ([`ff_core::pre_prompt_decision`]) and asks the channel on an `Ask` cell.
/// Transports without an interactive surface keep using this type; anything that
/// grows one should prefer the shared gate over extending this match.
pub struct MessagingApprover {
    mode: Mode,
}

impl MessagingApprover {
    pub fn new(mode: Mode) -> Self {
        Self { mode }
    }
}

#[async_trait]
impl Approver for MessagingApprover {
    async fn approve(
        &self,
        _message_id: &str,
        _call_id: &str,
        _name: &str,
        safety: Safety,
        _args: &serde_json::Value,
    ) -> ApprovalOutcome {
        match self.mode {
            Mode::Plan => {
                if safety == Safety::ReadOnly {
                    ApprovalOutcome::Allowed
                } else {
                    ApprovalOutcome::Denied(DenyReason::Mode {
                        mode: self.mode,
                        safety,
                    })
                }
            }
            Mode::Auto | Mode::Act => match safety {
                Safety::ReadOnly => ApprovalOutcome::Allowed,
                Safety::Write => ApprovalOutcome::Allowed,
                Safety::Sensitive => ApprovalOutcome::Allowed,
                // No interactive surface to confirm a remote publish or a
                // dangerous operation, so a messaging-triggered agent must not
                // push/merge to a remote unattended (#1051).
                Safety::Publish | Safety::Dangerous => ApprovalOutcome::Denied(DenyReason::Mode {
                    mode: self.mode,
                    safety,
                }),
            },
        }
    }
}
