//! `SlackApprover` — the interactive approval gate over Slack (#1059 T4,
//! RFC 0021 §5.2).
//!
//! Approvals reuse the shared [`PermissionMatrix`] via
//! [`ff_core::pre_prompt_decision`], exactly as the desktop `UiApprover` does, so
//! Slack does not fork the policy. On top of that it applies one hard override:
//!
//! **A channel button never authorizes a [`Safety::Publish`] or
//! [`Safety::Dangerous`] call.**
//!
//! That override is the reason this type exists rather than reusing an existing
//! approver. A Slack button is a *shared* authorization surface — every member of
//! the channel can click it, and a mis-click is indistinguishable from intent —
//! so it is strictly weaker than the desktop owner's click. The default matrix
//! leaves `Act/Publish` at `Allow` and `Act/Dangerous` at `Ask`; both are clamped
//! to Deny here, in every mode.
//!
//! Interactions arrive on the dedicated channel T3 set up
//! (`SlackTransport::take_interaction_rx`), never through a Router turn, so
//! draining them here cannot steal a user message from the Router.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ff_agent::Approver;
use ff_core::{Mode, PermissionMatrix, Safety};
use ff_transport::ChannelId;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, warn};

use crate::api::SlackApi;
use crate::envelope::SlackInteraction;

/// `action_id` of the approve button.
pub const ACTION_APPROVE: &str = "ff_approve";
/// `action_id` of the deny button.
pub const ACTION_DENY: &str = "ff_deny";

/// How long to wait for a click before failing closed.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// Interactive approver backed by Slack Block Kit buttons.
pub struct SlackApprover {
    api: SlackApi,
    channel: ChannelId,
    /// Agent mode. [`Approver::approve`] does not receive it, so it is bound at
    /// construction — one approver per session, which is also how `UiApprover`
    /// holds it.
    mode: Mode,
    matrix: PermissionMatrix,
    /// Interactions from T3's demux. Behind a `Mutex` because `approve` takes
    /// `&self`; it also serialises prompts, which is what we want — two
    /// concurrent prompts in one channel could not be told apart by the user.
    interactions: Mutex<mpsc::Receiver<SlackInteraction>>,
    timeout: Duration,
    /// Monotonic counter making each prompt's correlation token unique even when
    /// the same tool call is retried.
    seq: AtomicU64,
}

impl SlackApprover {
    /// Build an approver posting to `channel` and draining `interactions`.
    pub fn new(
        api: SlackApi,
        channel: ChannelId,
        mode: Mode,
        matrix: PermissionMatrix,
        interactions: mpsc::Receiver<SlackInteraction>,
    ) -> Self {
        Self {
            api,
            channel,
            mode,
            matrix,
            interactions: Mutex::new(interactions),
            timeout: DEFAULT_TIMEOUT,
            seq: AtomicU64::new(0),
        }
    }

    /// Override the click timeout (tests use a short one).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Whether `safety` is off-limits to a shared channel button regardless of
    /// what the matrix says. See the module docs for why this is not redundant
    /// with the matrix.
    ///
    /// Do not be tempted to drop this on the grounds that
    /// [`ff_core::pre_prompt_decision`] also takes a `Safety`: that parameter only
    /// suppresses a *scoped-rule* auto-allow, it never turns a cell into `Deny`.
    /// Verified against the default matrix — `Act/Publish` is `Allow` and
    /// `Act/Dangerous` is `Ask`, so this check is the only thing standing between
    /// a channel button and a remote publish. `the_override_holds_in_every_mode`
    /// fails if it is weakened.
    fn button_can_never_authorize(safety: Safety) -> bool {
        matches!(safety, Safety::Publish | Safety::Dangerous)
    }

    /// Correlation token for one prompt. Tying it to `call_id` *and* a counter
    /// means a click on an earlier prompt cannot answer a later one even if the
    /// model retries the identical call.
    fn next_token(&self, call_id: &str) -> String {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        format!("{call_id}#{n}")
    }

    fn prompt_blocks(tool: &str, safety: Safety, token: &str) -> serde_json::Value {
        serde_json::json!([
            {
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!("*Approval needed*\n`{tool}` — {safety:?}")
                }
            },
            {
                "type": "actions",
                "elements": [
                    {
                        "type": "button",
                        "action_id": ACTION_APPROVE,
                        "style": "primary",
                        "text": { "type": "plain_text", "text": "Approve" },
                        "value": token
                    },
                    {
                        "type": "button",
                        "action_id": ACTION_DENY,
                        "style": "danger",
                        "text": { "type": "plain_text", "text": "Deny" },
                        "value": token
                    }
                ]
            }
        ])
    }

    /// Await the click carrying `token`, discarding anything else.
    ///
    /// Discarding rather than accepting is a safety property: a click on a
    /// previous prompt must not answer the current one. Returns `None` on
    /// timeout or if the interaction channel closed (transport disconnected) —
    /// both fail closed at the call site.
    async fn await_decision(&self, token: &str) -> Option<bool> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let mut rx = self.interactions.lock().await;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Err(_) => return None,
                Ok(None) => {
                    warn!("slack interaction channel closed while awaiting approval");
                    return None;
                }
                Ok(Some(interaction)) => {
                    if interaction.value.as_deref() != Some(token) {
                        debug!(
                            action_id = %interaction.action_id,
                            value = ?interaction.value,
                            "discarding stale slack interaction: not the prompt we are awaiting"
                        );
                        continue;
                    }
                    match interaction.action_id.as_str() {
                        ACTION_APPROVE => return Some(true),
                        ACTION_DENY => return Some(false),
                        other => {
                            debug!(action_id = %other, "unknown slack action_id; ignoring");
                            continue;
                        }
                    }
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl Approver for SlackApprover {
    async fn approve(
        &self,
        _message_id: &str,
        call_id: &str,
        name: &str,
        safety: Safety,
        _args: &serde_json::Value,
    ) -> bool {
        if Self::button_can_never_authorize(safety) {
            warn!(
                tool = %name,
                ?safety,
                "denied over slack: a shared channel button may not authorize this"
            );
            return false;
        }

        let cell = self.matrix.effective_cell(name, self.mode, safety);
        match ff_core::pre_prompt_decision(cell, false, None, safety) {
            ff_core::PrePromptDecision::Deny => false,
            ff_core::PrePromptDecision::Allow => true,
            ff_core::PrePromptDecision::Prompt => {
                let token = self.next_token(call_id);
                let text = format!("Approval needed: `{name}` ({safety:?})");
                if let Err(e) = self
                    .api
                    .post_blocks(
                        &self.channel.platform_id,
                        &text,
                        Self::prompt_blocks(name, safety, &token),
                    )
                    .await
                {
                    warn!(tool = %name, error = %e, "failed to post slack approval prompt; denying");
                    return false;
                }
                match self.await_decision(&token).await {
                    Some(decision) => decision,
                    None => {
                        warn!(
                            tool = %name,
                            timeout_secs = self.timeout.as_secs(),
                            "no slack approval within the timeout; denying"
                        );
                        false
                    }
                }
            }
        }
    }
}
