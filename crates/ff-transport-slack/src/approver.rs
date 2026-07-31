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
//!
//! # Stated properties of this surface
//!
//! These are deliberate for T4, not oversights (#1168 review, findings 3 and 5):
//!
//! - **Any channel member may answer any prompt.** There is no allowlist and no
//!   check that the clicker owns the session; whose session a channel belongs to
//!   is a T5/#1060 question. The `{Publish, Dangerous}` clamp above is justified
//!   by exactly this weakness. The answering `user_id` *is* logged on every
//!   resolved decision, so a shared approval is at least attributable.
//! - **Prompts are answered one at a time.** See [`SlackApprover::await_decision`]
//!   for why concurrent prompts are unreachable today and what would have to
//!   change to make them safe.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ff_agent::Approver;
use ff_core::{Mode, PermissionMatrix, Safety};
use ff_transport::ChannelId;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

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

    /// Truncate a resolved arg for display. Slack's `mrkdwn` section caps at
    /// 3000 chars; this is far tighter because an approver skims a channel, and
    /// a wall of text is its own kind of blind approval.
    ///
    /// Backticks are neutralised via [`Self::inert`], not just newlines: the caller
    /// wraps this in a ``` fence, which an arg carrying its own ``` would close early.
    pub(crate) fn arg_preview(arg: &str) -> String {
        const MAX: usize = 300;
        let one_line = Self::inert(&arg.replace('\n', " ⏎ "));
        match one_line.char_indices().nth(MAX) {
            None => one_line,
            Some((cut, _)) => format!("{}…", &one_line[..cut]),
        }
    }

    /// Strip the one character that lets model-controlled text escape its container.
    ///
    /// Every string this module interpolates into a `mrkdwn` block sits inside a
    /// backtick container — a code span for the tool name, a ``` fence for the arg —
    /// and both the tool name (`call.name`, straight off the model) and the arg are
    /// model-controlled. A backtick closes that container early, after which the rest
    /// renders as markup: enough to draw a second "*Approval needed*" card naming a
    /// read-only tool while the real call is something else.
    ///
    /// The tool name is the worse of the two, because it forges *inside block 0* —
    /// there is no untouched genuine header above it to compare against. An unknown
    /// name does not get filtered out on the way here either: `Registry::safety`
    /// returns `Dangerous` for a name it does not recognise, precisely so an unknown
    /// tool cannot slip the gate, which means arbitrary model text reaches this
    /// function by design.
    ///
    /// Substituting rather than deleting keeps the text honest. Backticks are ordinary
    /// in shell arguments, and `echo \`date\`` silently becoming `echo date` changes
    /// what the approver is agreeing to — a quieter failure than the one being fixed.
    fn inert(s: &str) -> String {
        s.replace('`', "'")
    }

    /// `arg` is the *resolved* argument — the same string the scoped rules match
    /// on. Showing it is not cosmetic (#1168 review, finding 2): without it a
    /// channel member approving `bash — Write` cannot tell `cargo test` from
    /// `rm -rf ~`, which is blind approval on a surface where the PR's own
    /// framing says a mis-click is indistinguishable from intent.
    fn prompt_blocks(
        tool: &str,
        safety: Safety,
        arg: Option<&str>,
        token: &str,
    ) -> serde_json::Value {
        let mut blocks = vec![serde_json::json!({
            "type": "section",
            "text": {
                "type": "mrkdwn",
                "text": format!("*Approval needed*\n`{}` — {safety:?}", Self::inert(tool))
            }
        })];
        if let Some(arg) = arg {
            blocks.push(serde_json::json!({
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!("```{}```", Self::arg_preview(arg))
                }
            }));
        }
        blocks.push(serde_json::json!(
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
        ));
        serde_json::Value::Array(blocks)
    }

    /// Await the click carrying `token`, discarding anything else.
    ///
    /// Discarding rather than accepting is a safety property: a click on a
    /// previous prompt must not answer the current one. Returns `None` on
    /// timeout or if the interaction channel closed (transport disconnected) —
    /// both fail closed at the call site.
    /// Wait for the click that carries `token`, returning the verdict **and who
    /// clicked it**.
    ///
    /// Any channel member may answer any prompt — that is a deliberate property
    /// of a shared surface, and it is the whole reason `{Publish, Dangerous}` is
    /// clamped to Deny rather than merely prompted (#1168 review, finding 3).
    /// T4 does not gate on identity (whose session a channel belongs to is a T5
    /// / #1060 question), but it does *record* it: an unattributable approval is
    /// the difference between an audit trail and none.
    ///
    /// # Why holding the receiver lock across the timeout is safe today
    ///
    /// This holds the `Mutex` for the full timeout and *discards* non-matching
    /// interactions. Those compose badly if two prompts are ever outstanding at
    /// once: a click for the second would be eaten by the first's loop, and both
    /// would then time out. It is unreachable today, and both reasons live in
    /// *other* files — hence this note (#1168 review, finding 5):
    ///
    /// 1. `ff-agent` runs its parallel batch for `Safety::ReadOnly` only, so
    ///    every call that reaches an approver is on the serial path — `approve`
    ///    is strictly sequential per turn.
    /// 2. `SlackTransport::take_interaction_rx` hands out the receiver via
    ///    `Option::take`, so at most one approver can exist per transport.
    ///
    /// If either changes, this needs a per-token registry (a `HashMap<String,
    /// oneshot::Sender<_>>` fed by one demux task) rather than a shared lock.
    async fn await_decision(&self, token: &str) -> Option<(bool, String)> {
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
                        ACTION_APPROVE => return Some((true, interaction.user_id)),
                        ACTION_DENY => return Some((false, interaction.user_id)),
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
        args: &serde_json::Value,
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
        // `resolved_arg` must be computed the same way every approver computes
        // it (#1168 review, finding 1). Feeding `evaluate_rules` a `None` it did
        // not earn makes it return early, so **every** scoped rule is skipped —
        // including `Deny`. That is fail-open, and it is the same hole #768
        // warned about for a *wrong* key; T4 shipped it by passing `None`
        // outright, which is why `resolve_tool_arg` is shared code now.
        //
        // `is_allowlisted` stays `false`: the allowlist is session state that a
        // shared Slack channel has no equivalent of, and `false` is the
        // conservative direction (it can only add a prompt, never skip one).
        let resolved_arg = ff_core::resolve_tool_arg(name, args);
        let scoped_effect = self
            .matrix
            .evaluate_rules(name, resolved_arg.as_deref(), self.mode);
        match ff_core::pre_prompt_decision(cell, false, scoped_effect, safety) {
            ff_core::PrePromptDecision::Deny => false,
            ff_core::PrePromptDecision::Allow => true,
            ff_core::PrePromptDecision::Prompt => {
                let token = self.next_token(call_id);
                // The fallback text is what a push notification shows, so the
                // arg belongs here too, not just in the blocks (finding 2).
                let text = match resolved_arg.as_deref() {
                    Some(arg) => format!(
                        "Approval needed: `{name}` ({safety:?}) — {}",
                        Self::arg_preview(arg)
                    ),
                    None => format!("Approval needed: `{name}` ({safety:?})"),
                };
                let ts = match self
                    .api
                    .post_blocks(
                        &self.channel.platform_id,
                        &text,
                        Self::prompt_blocks(name, safety, resolved_arg.as_deref(), &token),
                    )
                    .await
                {
                    Ok(ts) => ts,
                    Err(e) => {
                        warn!(tool = %name, error = %e, "failed to post slack approval prompt; denying");
                        return false;
                    }
                };
                let outcome = self.await_decision(&token).await;

                // Retire the prompt so the channel does not keep live buttons on
                // a settled request (#1168 review, finding 4). The `ts` from the
                // post is the only handle for this, which is why it is no longer
                // discarded. Best-effort: the decision is already made, so a
                // failed edit must not change it.
                let epilogue = match &outcome {
                    Some((true, user)) => format!("✅ `{name}` approved by <@{user}>"),
                    Some((false, user)) => format!("🚫 `{name}` denied by <@{user}>"),
                    None => format!(
                        "⏱️ `{name}` timed out after {}s — denied",
                        self.timeout.as_secs()
                    ),
                };
                if let Err(e) = self
                    .api
                    .update_message(&self.channel.platform_id, &ts, &epilogue)
                    .await
                {
                    debug!(tool = %name, error = %e, "could not retire the slack prompt");
                }

                match outcome {
                    // Record who answered. T4 does not gate on identity, but an
                    // approval nobody can be attributed to is not an audit trail
                    // (#1168 review, finding 3).
                    Some((decision, user_id)) => {
                        info!(
                            tool = %name,
                            ?safety,
                            decision,
                            slack_user = %user_id,
                            "slack approval answered"
                        );
                        decision
                    }
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
