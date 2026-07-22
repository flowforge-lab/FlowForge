use async_trait::async_trait;
use ff_agent::Approver;
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
    ) -> bool {
        match self.mode {
            Mode::Plan => false, // Only read-only passes (bypasses Approver entirely).
            Mode::Auto | Mode::Act => match safety {
                Safety::ReadOnly => true,
                Safety::Write => true,
                Safety::Sensitive => true,
                // No interactive surface to confirm a remote publish or a
                // dangerous operation, so a messaging-triggered agent must not
                // push/merge to a remote unattended (#1051).
                Safety::Publish | Safety::Dangerous => false,
            },
        }
    }
}
