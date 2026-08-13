//! One place that pairs `build_system_prompt` with `run_turn`.
//!
//! Every host that drives a FlowForge turn — the desktop app, the CLI's
//! interactive and single-shot paths, the scheduled task runner, the goal loop,
//! and (next) the ACP agent-side server (#1201) — repeats the same two steps:
//! build the turn's system prompt from a [`SystemPromptInputs`], then hand that
//! prompt to [`run_turn`] along with the other nine arguments. Copied nine
//! times, that pairing is a drift surface: a change to `run_turn`'s signature,
//! or a host that forgets to build the prompt, diverges silently.
//!
//! [`run_session_turn`] collapses the pairing into a single call. It owns *only*
//! that pairing — it deliberately does **not** build the [`ToolContext`] (whose
//! fields are host-specific), inject memory, or run post-turn ambient
//! reinforcement (whose placement differs per host). Those stay with the host.
//! The host builds its `ToolContext` and `SystemPromptInputs`, calls this, and
//! keeps its own pre/post-turn logic — so #1201 gets a single entry point
//! instead of a tenth hand-written `run_turn` call.

use ff_llm::Provider;
use ff_session::SessionStore;

use crate::{
    build_system_prompt, run_turn, AgentError, AgentEvent, CancelToken, Message,
    ReasoningVisibility, SystemPromptInputs, ToolContext,
};

/// Build the turn's system prompt from `prompt_inputs`, then drive one turn via
/// [`run_turn`]. Every argument other than `prompt_inputs` is forwarded to
/// `run_turn` unchanged; `prompt_inputs` is turned into the `Some(&SystemPrompt)`
/// that `run_turn` expects.
///
/// This is the single seam hosts share: it does not touch the `ToolContext`,
/// memory injection, or post-turn reinforcement — those remain the host's.
#[allow(clippy::too_many_arguments)]
pub async fn run_session_turn(
    provider: &dyn Provider,
    store: &SessionStore,
    tools: &ToolContext<'_>,
    session_id: &str,
    model: &str,
    prompt_inputs: &SystemPromptInputs<'_>,
    enable_reasoning: bool,
    reasoning_visibility: ReasoningVisibility,
    cancel: CancelToken,
    on_event: impl FnMut(AgentEvent),
) -> Result<Message, AgentError> {
    let system_prompt = build_system_prompt(prompt_inputs);
    run_turn(
        provider,
        store,
        tools,
        session_id,
        model,
        Some(&system_prompt),
        enable_reasoning,
        reasoning_visibility,
        cancel,
        on_event,
    )
    .await
}
