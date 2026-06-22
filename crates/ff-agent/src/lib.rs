//! The agent turn loop.
//!
//! A turn is now multi-step: build history (advertising the tool schemas) -> stream
//! from the provider -> if the assistant only produced text, finish; if it requested
//! tool calls, execute each (subject to an approval policy), append the results, and
//! loop. The loop is capped by [`ToolContext::max_iterations`] so a misbehaving model
//! cannot spin forever.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ff_core::{Message, Mode, Role};
use ff_llm::{ChatMessage, ChatRequest, FunctionCall, LlmError, Provider, ToolCall as LlmToolCall};
use ff_session::SessionStore;
use ff_tools::{Safety, ToolRegistry};
use futures_util::StreamExt;
use serde::Serialize;

mod compaction;
mod system_prompt;
pub use compaction::{
    flush_due, CompactionContext, CompactionOutcome, CompactionStrategy, ContextPressure,
    ContextPressureEstimator, MemoryFlush, ProxyTokenEstimator, DEFAULT_CONTEXT_BUDGET_TOKENS,
    DEFAULT_FLUSH_AT_FRACTION,
};
pub use system_prompt::{build_flush_prompt, build_system_prompt, TimeOfDay, UserContext};

/// Default tool-call iteration cap for a turn when a phenotype does not override
/// it (#244 R3). A turn runs at most this many model<->tool round-trips before
/// it is forced to stop. Coding phenotypes raise this via `max_iterations` in
/// their phenotype TOML.
pub const DEFAULT_MAX_ITERATIONS: usize = 8;

/// When this many iterations (including the current one) remain before the cap,
/// the loop injects a transient "wrap up" nudge so the model produces a final
/// answer instead of being cut mid-tool-call (#244 R3).
const WRAP_UP_AT_REMAINING: usize = 1;

/// A transient provider error (connection blip, 429/5xx) is retried up to this many
/// total attempts before the turn surfaces the failure (#244 R1). Bounded so a hard
/// outage fails in seconds rather than spinning.
const MAX_PROVIDER_ATTEMPTS: usize = 3;

/// Base backoff between provider retries; attempt N waits `BASE << (N-1)` ms
/// (~250ms, 500ms), capped well under a second so retries stay snappy.
const RETRY_BACKOFF_BASE_MS: u64 = 250;

/// Sleep `ms`, but wake early (and often) if the turn is cancelled, so a retry
/// backoff never holds a cancelled turn open. `CancelToken` is a bare flag with no
/// future to await, so we poll it in small steps.
async fn cancellable_backoff(cancel: &CancelToken, ms: u64) {
    const STEP_MS: u64 = 50;
    let mut elapsed = 0;
    while elapsed < ms && !cancel.is_cancelled() {
        let step = STEP_MS.min(ms - elapsed);
        tokio::time::sleep(std::time::Duration::from_millis(step)).await;
        elapsed += step;
    }
}

/// After a model emits the identical `(tool, arguments)` call this many times in a
/// turn, inject a corrective nudge -- a context-rot stall where the model repeats a
/// call without using its result (#244 R2).
const REPEAT_NUDGE_AT: usize = 3;

/// If the identical call persists to this many repeats despite the nudge, break the
/// turn with a clear notice rather than spinning to the iteration cap.
const REPEAT_BREAK_AT: usize = 5;

/// Once a session is over the context-pressure threshold, re-run the memory flush
/// again only after the transcript has grown by this many messages, so a long
/// over-budget conversation flushes periodically rather than every single turn
/// (#244 R5). Passed to [`flush_due`] as its re-flush interval.
const DEFAULT_REFLUSH_INTERVAL_MESSAGES: u64 = 8;

/// Tool results are appended verbatim to the session history and replayed on the
/// next request, so one oversized result (a big file read, a long command dump) can
/// dominate the context budget on its own. Cap what is *persisted to history* at
/// this many bytes (#244 R8); the emitted `ToolCallFinished` event still carries the
/// full untruncated content for the UI.
const TOOL_RESULT_MAX_BYTES: usize = 8 * 1024;

/// Persisted assistant reasoning is replayed on every later tool-call turn for
/// reasoning gateways (#375 PR-2), so an unbounded chain-of-thought grows both
/// the stored row and -- compounding across turns -- the wire payload. Cap what
/// is persisted at this many bytes (#378). Larger than the tool-result cap
/// because a legitimate CoT is longer than a tool dump; the gateway accepts a
/// truncated reasoning_content (verified -- it checks presence, not integrity),
/// so a cap is safe for the round-trip.
const REASONING_MAX_BYTES: usize = 16 * 1024;

/// Events the agent emits during a turn. The host (Tauri shell or a test) decides
/// how to surface them — over IPC, to a channel, or into assertions.
#[derive(Debug, Clone, Serialize)]
pub enum AgentEvent {
    Token {
        message_id: String,
        delta: String,
    },
    Reasoning {
        message_id: String,
        delta: String,
    },
    ToolCallStarted {
        message_id: String,
        call_id: String,
        name: String,
        args: serde_json::Value,
    },
    ToolCallFinished {
        message_id: String,
        call_id: String,
        success: bool,
        result: String,
    },
    Done {
        message_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        final_message: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        turns: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        token_count: Option<u32>,
    },
    /// A silent context-pressure memory flush (#244 R5) wrote `writes` durable
    /// facts to the user's on-disk memory this turn (#283). Emitted only when
    /// `writes > 0`, so the frontend can surface provenance ("memory
    /// auto-updated"). `message_id` is the assistant message of the iteration
    /// that triggered the flush.
    MemoryFlushed {
        message_id: String,
        writes: u32,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Llm(#[from] ff_llm::LlmError),
}

/// Decides whether a non-read-only tool call may run. The host supplies this; the
/// desktop shell routes it to a UI confirmation (an async round-trip), so the call
/// is `async`. Read-only calls bypass it entirely. `message_id` + `call_id` let the
/// host correlate the request with the exact tool step it is rendering.
#[async_trait::async_trait]
pub trait Approver: Send + Sync {
    async fn approve(
        &self,
        message_id: &str,
        call_id: &str,
        name: &str,
        safety: Safety,
        args: &serde_json::Value,
    ) -> bool;

    /// Pause the turn and put a question to the user (the `ask_user` tool, #44),
    /// resuming with their answer. `args` carries the tool call's arguments (the
    /// `question` field); `message_id`/`call_id` correlate the request with the tool
    /// step the host is rendering. Returns the answer, or `None` if it was dismissed
    /// or cancelled — the loop turns `None` into a tool result, never a hang.
    ///
    /// Defaults to `None`: a host with no interactive surface simply dismisses the
    /// question rather than blocking the turn.
    async fn ask(
        &self,
        _message_id: &str,
        _call_id: &str,
        _args: &serde_json::Value,
    ) -> Option<String> {
        None
    }
}

/// Default cap on sub-agent delegation depth (#234): a top-level agent may spawn
/// children, but those children may not spawn further sub-agents. Prevents an
/// unbounded sub-agent tree.
pub const DEFAULT_MAX_DELEGATION_DEPTH: usize = 1;

/// Everything the loop needs to dispatch tools.
pub struct ToolContext<'a> {
    pub registry: &'a ToolRegistry,
    /// Per-session workspace root. File tools are jailed to it; `bash` runs in it.
    pub root: &'a Path,
    pub approve: &'a dyn Approver,
    pub max_iterations: usize,
    /// Current delegation depth: 0 at the top level, +1 per nested sub-agent (#234).
    pub depth: usize,
    /// Depth at which sub-agent spawning is refused.
    pub max_depth: usize,
    /// When `Some`, the only tool names this (sub-)agent may call or be advertised.
    /// `None` = the full registry. Used to scope a delegated subtask.
    pub allowed: Option<std::collections::HashSet<String>>,
    /// Agent autonomy mode (RFC 0011). In [`Mode::Plan`] only ReadOnly tools are
    /// advertised, so the model cannot see or call anything that mutates.
    pub mode: Mode,
}

impl<'a> ToolContext<'a> {
    /// A top-level context: full toolset, no delegation parent, default depth cap.
    pub fn new(
        registry: &'a ToolRegistry,
        root: &'a Path,
        approve: &'a dyn Approver,
        max_iterations: usize,
    ) -> Self {
        Self {
            registry,
            root,
            approve,
            max_iterations,
            depth: 0,
            max_depth: DEFAULT_MAX_DELEGATION_DEPTH,
            allowed: None,
            mode: Mode::default(),
        }
    }
}

/// Cooperative cancellation flag, shared between a running turn and `cancel`.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Head+tail truncation of an over-budget tool result on UTF-8 char boundaries
/// (#244 R8). Keeps roughly the first and last halves of the byte budget with a
/// marker between them, so both the start (often a summary/header) and the end
/// (often the conclusion/error) survive. Returns the input unchanged when it is
/// already within `TOOL_RESULT_MAX_BYTES`.
fn truncate_tool_result(content: &str) -> String {
    if content.len() <= TOOL_RESULT_MAX_BYTES {
        return content.to_string();
    }
    let marker = "\n\n[... tool result truncated to fit context ...]\n\n";
    let budget = TOOL_RESULT_MAX_BYTES.saturating_sub(marker.len());
    let head_budget = budget / 2;
    let tail_budget = budget - head_budget;

    let mut head_end = head_budget.min(content.len());
    while head_end > 0 && !content.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = content.len().saturating_sub(tail_budget);
    while tail_start < content.len() && !content.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!(
        "{}{}{}",
        &content[..head_end],
        marker,
        &content[tail_start..]
    )
}

/// Tail-biased truncation of an over-budget reasoning string on UTF-8 char
/// boundaries (#378). Unlike a tool result (head+tail), a chain-of-thought is
/// most useful at its *end* -- the final reasoning state the next turn should
/// continue from -- so keep the last REASONING_MAX_BYTES with a leading marker.
/// Returns the input unchanged when already within budget. Applied at persist
/// time, so the stored value is also the wire value and re-truncation is
/// idempotent.
fn truncate_reasoning(reasoning: &str) -> String {
    if reasoning.len() <= REASONING_MAX_BYTES {
        return reasoning.to_string();
    }
    let marker = "[... earlier reasoning truncated ...]\n\n";
    let budget = REASONING_MAX_BYTES.saturating_sub(marker.len());
    let mut tail_start = reasoning.len().saturating_sub(budget);
    while tail_start < reasoning.len() && !reasoning.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!("{}{}", marker, &reasoning[tail_start..])
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "tool",
    }
}

pub(crate) fn to_chat(messages: &[Message]) -> Vec<ChatMessage> {
    messages
        .iter()
        .map(|m| {
            let tool_calls = m.tool_calls.as_ref().map(|calls| {
                calls
                    .iter()
                    .map(|tc| LlmToolCall {
                        id: tc.id.clone(),
                        kind: "function".to_string(),
                        function: FunctionCall {
                            name: tc.name.clone(),
                            arguments: tc.arguments.clone(),
                        },
                    })
                    .collect()
            });
            // An assistant message that only carries tool calls sends `content: null`.
            let content = if m.content.is_empty() && tool_calls.is_some() {
                None
            } else {
                Some(m.content.clone())
            };
            ChatMessage {
                role: role_str(m.role).to_string(),
                content,
                tool_calls,
                tool_call_id: m.tool_call_id.clone(),
                name: None,
                attachments: m.attachments.clone().unwrap_or_default(),
                // Persisted reasoning from prior assistant turns (#375 PR-1).
                // The provider re-injects it under the gateway's field name on
                // tool-call turns; vanilla providers ignore it via the dialect.
                reasoning: m.reasoning.clone(),
            }
        })
        .collect()
}

/// Accumulates streamed tool-call fragments keyed by `index`.
#[derive(Default)]
struct CallBuf {
    id: String,
    name: String,
    arguments: String,
}

/// The set of tool names to advertise to the model this turn.
///
/// In [`Mode::Plan`] (RFC 0011) only the registry's ReadOnly tools are advertised so
/// the model cannot see — let alone call — anything that mutates; this is intersected
/// with any sub-agent allowlist (fail safe). In Act/Auto the allowlist passes through
/// unchanged (`None` = full registry).
fn advertised_tools(
    mode: Mode,
    allowed: Option<&std::collections::HashSet<String>>,
    registry: &ToolRegistry,
) -> Option<std::collections::HashSet<String>> {
    if !mode.is_plan() {
        return allowed.cloned();
    }
    let readonly = registry.readonly_tool_names();
    Some(match allowed {
        Some(set) => set.intersection(&readonly).cloned().collect(),
        None => readonly,
    })
}

/// RAII guard that guarantees every assistant `tool_use` gets a matching tool
/// result, even if the `run_turn` future is *dropped* mid-loop (window closed,
/// runtime torn down, or a new turn superseding an in-flight one). Rust async
/// drop runs no code after the current await point, so a sequential backfill
/// after the call loop is skipped on drop — leaving a dangling `tool_use` that
/// strict providers (Bedrock, OpenAI) reject on the next turn (#316).
///
/// Seeded with every requested call id after `attach_tool_calls`; each completed
/// call is removed via [`fulfilled`](Self::fulfilled). On `Drop` — whether the
/// turn ended cleanly, was cooperatively cancelled, or the future was dropped —
/// any still-pending id gets a synchronous `[cancelled]` result. The store call
/// is synchronous, so a plain `Drop` impl suffices (no async drop needed).
struct ToolResultBackfill<'a> {
    store: &'a SessionStore,
    session_id: &'a str,
    pending: HashSet<String>,
}

impl<'a> ToolResultBackfill<'a> {
    fn new(store: &'a SessionStore, session_id: &'a str) -> Self {
        Self {
            store,
            session_id,
            pending: HashSet::new(),
        }
    }

    /// Mark a call id as awaiting a result; it will be backfilled on drop unless
    /// later [`fulfilled`](Self::fulfilled).
    fn expect(&mut self, call_id: &str) {
        self.pending.insert(call_id.to_string());
    }

    /// A real tool result was persisted for this id, so it no longer needs backfill.
    fn fulfilled(&mut self, call_id: &str) {
        self.pending.remove(call_id);
    }
}

impl Drop for ToolResultBackfill<'_> {
    fn drop(&mut self) {
        for call_id in self.pending.drain() {
            self.store
                .add_tool_result_message(self.session_id, call_id, "[cancelled]".to_string());
        }
    }
}

/// Runs one assistant turn for `session_id`, executing any tool calls the model
/// requests until it produces a plain text answer (or the iteration cap is hit).
/// `on_event` is called synchronously as the turn progresses. The final assistant
/// message is persisted and returned.
///
/// When `enable_reasoning` is true, provider reasoning streams are requested and
/// emitted as [`AgentEvent::Reasoning`] (not persisted in message content).
#[allow(clippy::too_many_arguments)]
pub async fn run_turn(
    provider: &dyn Provider,
    store: &SessionStore,
    tools: &ToolContext<'_>,
    session_id: &str,
    model: &str,
    // Optional system prompt prepended to every request this turn (skills + persona
    // + ambient context). Built by the host via `build_system_prompt`.
    system_prompt: Option<&str>,
    enable_reasoning: bool,
    cancel: CancelToken,
    mut on_event: impl FnMut(AgentEvent),
) -> Result<Message, AgentError> {
    let allow_subagent = tools.depth < tools.max_depth;
    let advertised = advertised_tools(tools.mode, tools.allowed.as_ref(), tools.registry);
    let tool_schemas = tools
        .registry
        .openai_tools_for(advertised.as_ref(), allow_subagent);
    let mut last: Option<Message> = None;

    let max_iter = tools.max_iterations.max(1);
    let mut turn_count: u32 = 0;
    // Repeated-call / no-progress guard (#244 R2): count identical `(tool, arguments)`
    // calls across the turn; `repeat_nudge` carries a tool name to warn about on the
    // next request; `stop_reason` ends the turn with a clear notice when a stall
    // persists past the nudge.
    let mut call_counts: HashMap<(String, String), usize> = HashMap::new();
    let mut repeat_nudge: Option<String> = None;
    let mut stop_reason: Option<String> = None;
    // Context-pressure flush bookkeeping (#244 R5): the transcript length at the last
    // flush, so we re-flush on growth rather than every iteration. `None` = never
    // flushed this turn.
    let mut last_flush_count: Option<u64> = None;
    for iter in 0..max_iter {
        turn_count += 1;
        if cancel.is_cancelled() {
            break;
        }

        let history = store.get_messages(session_id);
        // Context-pressure flush (#244 R5): before sending this iteration's request,
        // estimate how full the context is and, once over the budget fraction, run a
        // silent memory flush so durable facts are persisted before any future
        // compaction summarizes them away. The flush snapshots the transcript
        // read-only and writes only to on-disk memory -- it never mutates this
        // session's messages, so `messages` below is unaffected. Best-effort: a flush
        // failure must not abort the user's turn.
        let message_count = history.len() as u64;
        let pressure = ProxyTokenEstimator::default().assess(&history, model);
        // Carries the flush's write count to the `MemoryFlushed` event below, once
        // this iteration's assistant message id exists to correlate it with.
        let mut flushed_writes: Option<u32> = None;
        if !cancel.is_cancelled()
            && flush_due(
                pressure,
                message_count,
                last_flush_count,
                DEFAULT_FLUSH_AT_FRACTION,
                DEFAULT_REFLUSH_INTERVAL_MESSAGES,
            )
        {
            // Surface provenance (#283) when the flush actually wrote durable facts;
            // a no-op / NoReply / failure stays silent (best-effort — never aborts
            // the user's turn).
            if let Ok(CompactionOutcome::Wrote { writes }) = MemoryFlush
                .compact(CompactionContext {
                    provider,
                    store,
                    registry: tools.registry,
                    root: tools.root,
                    session_id,
                    model,
                    cancel: cancel.clone(),
                })
                .await
            {
                if writes > 0 {
                    // Explicit narrowing across the usize -> u32 contract boundary;
                    // an implausible overflow degrades to "no event" rather than wrapping.
                    flushed_writes = u32::try_from(writes).ok();
                }
            }
            // Record the attempt regardless of outcome so a no-op or failing flush
            // does not re-fire every iteration.
            last_flush_count = Some(message_count);
        }

        let mut messages = Vec::new();
        if let Some(system) = system_prompt {
            // Transient: the system prompt is injected into the request only, never
            // persisted to the store, so message history stays user/assistant/tool.
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: Some(system.to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,

                attachments: Vec::new(),
                reasoning: None,
            });
        }
        messages.extend(to_chat(&history));

        // Near the iteration cap, nudge the model to stop calling tools and answer,
        // so a long turn ends with a real reply instead of "[stopped: reached
        // tool-call limit]" cut mid-tool (#244 R3). Transient: request-only.
        let remaining = max_iter - iter; // iterations left, including this one
        if remaining <= WRAP_UP_AT_REMAINING && max_iter > 1 {
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: Some(
                    "This is your final step before the tool-call limit. Do not call any \
                     more tools; summarize what you have done and give your final answer \
                     to the user now."
                        .to_string(),
                ),
                tool_calls: None,
                tool_call_id: None,
                name: None,

                attachments: Vec::new(),
                reasoning: None,
            });
        }

        // Corrective nudge for a detected repeated-call stall (#244 R2). Request-only,
        // like the wrap-up nudge above.
        if let Some(tool) = repeat_nudge.take() {
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: Some(format!(
                    "You have called `{tool}` with identical arguments {REPEAT_NUDGE_AT} times \
                     without making progress. Do not repeat that call -- read the result you \
                     already have, try a different approach or different arguments, or give \
                     your final answer now."
                )),
                tool_calls: None,
                tool_call_id: None,
                name: None,

                attachments: Vec::new(),
                reasoning: None,
            });
        }

        // Reserve the assistant message id up front so the frontend can route tokens.
        let message_id = store
            .add_message(session_id, Role::Assistant, String::new())
            .id;

        // Provenance for a flush that ran at the top of this iteration (#283): now
        // that the turn's assistant message id exists, correlate the event with it.
        if let Some(writes) = flushed_writes {
            on_event(AgentEvent::MemoryFlushed {
                message_id: message_id.clone(),
                writes,
            });
        }

        // Bounded retry for transient provider failures (#244 R1). A setup error
        // (request never started) is always safe to retry. A mid-stream error is
        // only retried when nothing has been emitted yet this attempt -- once tokens
        // or tool-call fragments have reached the frontend, replaying would duplicate
        // them, so we surface the error instead. Fatal errors (auth, 4xx, decode)
        // never retry.
        let mut acc = String::new();
        let mut reasoning_acc = String::new();
        let mut calls: BTreeMap<u32, CallBuf> = BTreeMap::new();
        let mut attempt = 0usize;
        loop {
            attempt += 1;
            acc.clear();
            reasoning_acc.clear();
            calls.clear();
            let mut emitted_any = false;

            let req = ChatRequest {
                model: model.to_string(),
                messages: messages.clone(),
                tools: tool_schemas.clone(),
                thinking: enable_reasoning,
            };

            let mut stream = match provider.chat_stream(req).await {
                Ok(s) => s,
                Err(e) => {
                    if e.is_transient() && attempt < MAX_PROVIDER_ATTEMPTS {
                        cancellable_backoff(&cancel, RETRY_BACKOFF_BASE_MS << (attempt - 1)).await;
                        // Cancelled during the backoff -> stop now instead of issuing one
                        // more wasted provider call (#244 R1 follow-up).
                        if cancel.is_cancelled() {
                            break;
                        }
                        continue;
                    }
                    on_event(AgentEvent::Error {
                        message: e.to_string(),
                    });
                    return Err(e.into());
                }
            };

            let mut stream_err: Option<LlmError> = None;
            while let Some(item) = stream.next().await {
                if cancel.is_cancelled() {
                    break;
                }
                match item {
                    Ok(chunk) => {
                        if enable_reasoning && !chunk.reasoning_delta.is_empty() {
                            emitted_any = true;
                            reasoning_acc.push_str(&chunk.reasoning_delta);
                            on_event(AgentEvent::Reasoning {
                                message_id: message_id.clone(),
                                delta: chunk.reasoning_delta,
                            });
                        }
                        if !chunk.delta.is_empty() {
                            emitted_any = true;
                            acc.push_str(&chunk.delta);
                            on_event(AgentEvent::Token {
                                message_id: message_id.clone(),
                                delta: chunk.delta,
                            });
                        }
                        for frag in chunk.tool_calls {
                            emitted_any = true;
                            let buf = calls.entry(frag.index).or_default();
                            if let Some(id) = frag.id {
                                buf.id = id;
                            }
                            if let Some(name) = frag.name {
                                buf.name = name;
                            }
                            buf.arguments.push_str(&frag.arguments);
                        }
                        if chunk.done {
                            break;
                        }
                    }
                    Err(e) => {
                        stream_err = Some(e);
                        break;
                    }
                }
            }

            match stream_err {
                Some(e) if e.is_transient() && !emitted_any && attempt < MAX_PROVIDER_ATTEMPTS => {
                    cancellable_backoff(&cancel, RETRY_BACKOFF_BASE_MS << (attempt - 1)).await;
                    // Cancelled during the backoff -> stop now instead of issuing one
                    // more wasted provider call (#244 R1 follow-up).
                    if cancel.is_cancelled() {
                        break;
                    }
                    continue;
                }
                Some(e) => {
                    on_event(AgentEvent::Error {
                        message: e.to_string(),
                    });
                    return Err(e.into());
                }
                // A clean stream that produced neither text nor a tool call is a provider
                // anomaly, not a real final answer (#244 R7). Retry (bounded, same backoff
                // as a transient error) rather than emitting a silent empty bubble; a
                // cancelled turn falls through to break and stops.
                None if acc.trim().is_empty()
                    && calls.is_empty()
                    && !cancel.is_cancelled()
                    && attempt < MAX_PROVIDER_ATTEMPTS =>
                {
                    cancellable_backoff(&cancel, RETRY_BACKOFF_BASE_MS << (attempt - 1)).await;
                    if cancel.is_cancelled() {
                        break;
                    }
                    continue;
                }
                None => break,
            }
        }

        // Persist reasoning before content so the finalized row carries both (#375
        // PR-1). Skip empty reasoning so non-reasoning turns keep a NULL column.
        // Cap it (#378): a long CoT is replayed on every later tool-call turn, so
        // truncate at write time -- the stored value is then also the wire value.
        if !reasoning_acc.trim().is_empty() {
            let reasoning = truncate_reasoning(&reasoning_acc);
            store.set_message_reasoning(&message_id, session_id, &reasoning);
        }
        let final_text = acc.clone();
        let finalized = store.set_message_content(&message_id, session_id, acc);

        // No tool calls -> this is the final text answer.
        if calls.is_empty() {
            // Empty even after the bounded R7 retries (or the turn was cancelled): don't
            // emit an empty Done bubble. Set a notice and let the post-loop finalize on
            // this same reserved message, so there is no orphan empty assistant message.
            if final_text.trim().is_empty() {
                if !cancel.is_cancelled() && stop_reason.is_none() {
                    stop_reason =
                        Some("[stopped: the model returned an empty response]".to_string());
                }
                last = Some(finalized);
                break;
            }
            // Approximate context size at completion so the frontend can show a
            // token gauge (#244 R6). The proxy estimator (chars/4) is intentionally
            // coarse; per-model tokenizers plug in via ContextPressureEstimator later.
            let token_count = Some(
                ProxyTokenEstimator::default()
                    .assess(&store.get_messages(session_id), model)
                    .estimated_tokens as u32,
            );
            on_event(AgentEvent::Done {
                message_id: message_id.clone(),
                final_message: Some(final_text),
                turns: Some(turn_count),
                token_count,
            });
            return Ok(finalized);
        }

        // Persist the tool calls on the assistant message, then execute each.
        let core_calls: Vec<ff_core::ToolCall> = calls
            .values()
            .map(|c| ff_core::ToolCall {
                id: c.id.clone(),
                name: c.name.clone(),
                arguments: c.arguments.clone(),
            })
            .collect();
        store.attach_tool_calls(&message_id, session_id, core_calls);

        // Drop-safe backfill (#316): seed a guard with every requested call id, so
        // a dropped turn future (or cooperative cancel) cannot leave a `tool_use`
        // without a matching tool result. Each call removes its id once its real
        // result is persisted; the guard's Drop backfills whatever remains.
        let mut backfill = ToolResultBackfill::new(store, session_id);
        for call in calls.values() {
            backfill.expect(&call.id);
        }
        for call in calls.values() {
            if cancel.is_cancelled() {
                break;
            }
            // Parse the model-supplied JSON arguments. On failure, return a clear,
            // actionable tool result instead of silently passing Null (#244 R4): the
            // model sees "your arguments were not valid JSON" and self-corrects, rather
            // than an opaque downstream tool error.
            let parsed_args = serde_json::from_str::<serde_json::Value>(&call.arguments);

            on_event(AgentEvent::ToolCallStarted {
                message_id: message_id.clone(),
                call_id: call.id.clone(),
                name: call.name.clone(),
                args: parsed_args
                    .as_ref()
                    .ok()
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            });

            // Interactive tools (`ask_user`, #44) don't execute against the workspace:
            // pause the turn, put the question to the host, and use the answer as the
            // tool result. A dismissed/cancelled question yields a result too, so the
            // assistant `tool_calls` message always has a matching reply (no malformed
            // history). Asking is read-only, so it never reaches the approval gate.
            // Gate dispatch on the same advertised set that gated the schema (#264).
            // `advertised` already folds together the Plan-mode read-only restriction
            // *and* any sub-agent allowlist, so a Plan-mode model that names a hidden
            // mutating tool (e.g. via prompt injection) is hard-blocked here -- before
            // the approval gate -- rather than relying on schema-hiding plus the
            // approver as the only backstop. `None` = unrestricted (Act/Auto top level).
            let permitted = advertised
                .as_ref()
                .is_none_or(|set| set.contains(&call.name));
            let outcome = match parsed_args {
                Err(e) => ff_tools::ToolOutcome::error(format!(
                    "tool `{}` arguments were not valid JSON ({e}); received: `{}`. \
                     Re-issue the call with a valid JSON object matching the tool schema.",
                    call.name, call.arguments
                )),
                Ok(args) => {
                    if tools.registry.is_interactive(&call.name) {
                        match tools.approve.ask(&message_id, &call.id, &args).await {
                            Some(answer) => ff_tools::ToolOutcome::ok(answer),
                            None => ff_tools::ToolOutcome::error("[no answer: question dismissed]"),
                        }
                    } else if !permitted {
                        // Distinguish the two reasons a tool can be hidden so the model
                        // gets an actionable result instead of a silent failure.
                        ff_tools::ToolOutcome::error(if tools.mode.is_plan() {
                            format!(
                                "tool `{}` is not available in Plan mode (read-only tools only)",
                                call.name
                            )
                        } else {
                            format!("tool `{}` is not permitted for this sub-agent", call.name)
                        })
                    } else if ff_tools::is_subagent(&call.name) {
                        // Delegation (#234): drive a child turn in a fresh ephemeral session and
                        // return only its summary. The child reuses the same provider/approver,
                        // so its tool calls hit the identical approval gate (no escalation).
                        run_subagent(
                            provider,
                            store,
                            tools,
                            model,
                            system_prompt,
                            cancel.clone(),
                            &args,
                        )
                        .await
                    } else {
                        let safety = tools.registry.safety(&call.name, &args);
                        let approved = safety == Safety::ReadOnly
                            || tools
                                .approve
                                .approve(&message_id, &call.id, &call.name, safety, &args)
                                .await;
                        if approved {
                            tools.registry.run(&call.name, args, tools.root).await
                        } else {
                            ff_tools::ToolOutcome::error(format!(
                                "call to `{}` was not approved",
                                call.name
                            ))
                        }
                    }
                }
            };

            store.add_tool_result_message(
                session_id,
                call.id.clone(),
                truncate_tool_result(&outcome.content),
            );
            backfill.fulfilled(&call.id);
            on_event(AgentEvent::ToolCallFinished {
                message_id: message_id.clone(),
                call_id: call.id.clone(),
                success: outcome.success,
                result: outcome.content,
            });

            // Count identical calls to catch a no-progress stall (#244 R2).
            let count = call_counts
                .entry((call.name.clone(), call.arguments.clone()))
                .or_insert(0);
            *count += 1;
            if *count >= REPEAT_BREAK_AT {
                stop_reason = Some(format!(
                    "[stopped: repeated the identical `{}` tool call {REPEAT_BREAK_AT} times \
                     without making progress]",
                    call.name
                ));
            } else if *count >= REPEAT_NUDGE_AT {
                // Keep nudging through the recovery window (#244 R2 follow-up): re-arm on
                // every repeat from the nudge threshold up to the break, so a model that
                // ignores the first reminder still gets one before we break the turn.
                repeat_nudge = Some(call.name.clone());
            }
        }

        // Any call without a real result (cooperative cancel mid-loop, or a turn
        // future about to be dropped) is backfilled with `[cancelled]` when
        // `backfill` drops at the end of this iteration's scope (#316). This is
        // the *same* Drop path that protects a dropped future, so the two cases
        // can't diverge into malformed history.
        drop(backfill);

        last = Some(finalized);

        // A persistent repeated-call stall ends the turn here, before burning the
        // remaining iterations (#244 R2).
        if stop_reason.is_some() {
            break;
        }
    }

    // Hit the iteration cap (or was cancelled) without a plain text answer.
    let mut msg =
        last.unwrap_or_else(|| store.add_message(session_id, Role::Assistant, String::new()));
    if msg.content.is_empty() {
        // The final assistant message only carried tool calls, so it would render as
        // an empty bubble. Replace it with a notice explaining why the turn stopped.
        let notice = if cancel.is_cancelled() {
            "[stopped]".to_string()
        } else if let Some(reason) = stop_reason {
            reason
        } else {
            "[stopped: reached tool-call limit]".to_string()
        };
        msg = store.set_message_content(&msg.id, session_id, notice);
    }
    // Same context-size estimate as the plain-text completion path (#244 R6).
    let token_count = Some(
        ProxyTokenEstimator::default()
            .assess(&store.get_messages(session_id), model)
            .estimated_tokens as u32,
    );
    on_event(AgentEvent::Done {
        message_id: msg.id.clone(),
        final_message: Some(msg.content.clone()),
        turns: Some(turn_count),
        token_count,
    });
    Ok(msg)
}

/// Spawns a scoped child agent for an `agent` tool call (#234): a fresh ephemeral
/// session seeded with the `task`, run to completion against the same workspace and
/// approver, returning only the child's final message as the tool result. The child
/// session is deleted afterward — the parent never inherits its transcript.
#[allow(clippy::too_many_arguments)]
async fn run_subagent(
    provider: &dyn Provider,
    store: &SessionStore,
    parent: &ToolContext<'_>,
    model: &str,
    system_prompt: Option<&str>,
    cancel: CancelToken,
    args: &serde_json::Value,
) -> ff_tools::ToolOutcome {
    if parent.depth >= parent.max_depth {
        return ff_tools::ToolOutcome::error(
            "sub-agents cannot spawn further sub-agents (max delegation depth reached)",
        );
    }

    let task = match args.get("task").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => {
            return ff_tools::ToolOutcome::error(
                "agent: `task` is required and must be a non-empty string",
            )
        }
    };

    // Clamp the child's iteration budget to a safe ceiling.
    const SUBAGENT_ITER_CAP: usize = 16;
    let max_iterations = args
        .get("max_iterations")
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize).clamp(1, SUBAGENT_ITER_CAP))
        .unwrap_or(SUBAGENT_ITER_CAP);

    // Optional tool allowlist for the subtask (e.g. a read-only audit).
    let allowed: Option<std::collections::HashSet<String>> =
        args.get("tools").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        });

    let child = store.create_session(Some(task.clone()));
    store.add_message(&child.id, Role::User, task);

    let child_ctx = ToolContext {
        registry: parent.registry,
        root: parent.root,
        approve: parent.approve,
        max_iterations,
        depth: parent.depth + 1,
        max_depth: parent.max_depth,
        allowed,
        mode: parent.mode,
    };

    // Child events are swallowed: the parent receives only the summary, never the
    // child's token/tool stream — the whole point of fresh-context delegation.
    let result = Box::pin(run_turn(
        provider,
        store,
        &child_ctx,
        &child.id,
        model,
        system_prompt,
        false,
        cancel,
        |_event| {},
    ))
    .await;

    store.delete_session(&child.id);

    match result {
        Ok(msg) if !msg.content.trim().is_empty() => ff_tools::ToolOutcome::ok(msg.content),
        Ok(_) => ff_tools::ToolOutcome::ok("[sub-agent finished without a summary]"),
        Err(e) => ff_tools::ToolOutcome::error(format!("sub-agent failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ff_llm::{Chunk, ChunkStream, LlmError, ToolCallDelta};
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    #[test]
    fn to_chat_carries_reasoning_from_persisted_message() {
        // #375 PR-2: ff-agent must lift Message.reasoning into ChatMessage.reasoning
        // so the OpenAI-compatible provider can re-inject it under the gateway's
        // field name on the next tool-call turn.
        let msg = ff_core::Message {
            id: "m1".into(),
            session_id: "s1".into(),
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(vec![ff_core::ToolCall {
                id: "call_1".into(),
                name: "search".into(),
                arguments: "{}".into(),
            }]),
            tool_call_id: None,
            attachments: None,
            reasoning: Some("because A then B".into()),
            created_at: 0,
        };
        let out = to_chat(std::slice::from_ref(&msg));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].reasoning.as_deref(), Some("because A then B"));
    }

    #[test]
    fn plan_mode_advertises_only_readonly_tools() {
        let reg = ToolRegistry::with_defaults();
        let advertised = advertised_tools(Mode::Plan, None, &reg).expect("Plan restricts");
        for name in ["view", "grep", "glob", "tree", "todo", "ask_user"] {
            assert!(advertised.contains(name), "Plan should advertise {name}");
        }
        for name in [
            "bash",
            "python",
            "edit",
            "write",
            "apply_patch",
            "web_fetch",
            "agent",
        ] {
            assert!(!advertised.contains(name), "Plan must hide {name}");
        }
    }

    #[test]
    fn plan_mode_intersects_with_subagent_allowlist() {
        let reg = ToolRegistry::with_defaults();
        // A sub-agent scoped to {view, edit}: Plan further drops the mutating `edit`.
        let allowed: std::collections::HashSet<String> =
            ["view", "edit"].iter().map(|s| s.to_string()).collect();
        let advertised = advertised_tools(Mode::Plan, Some(&allowed), &reg).unwrap();
        assert_eq!(advertised, ["view".to_string()].into_iter().collect());
    }

    #[test]
    fn act_and_auto_pass_the_allowlist_through_unchanged() {
        let reg = ToolRegistry::with_defaults();
        assert_eq!(advertised_tools(Mode::Act, None, &reg), None);
        assert_eq!(advertised_tools(Mode::Auto, None, &reg), None);
        let allowed: std::collections::HashSet<String> =
            ["view", "edit"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            advertised_tools(Mode::Auto, Some(&allowed), &reg),
            Some(allowed)
        );
    }

    /// Records whether its approval gate was ever consulted. A Plan-mode hard block
    /// must reject before this is reached (#264 review).
    struct RecordingApprover {
        consulted: Arc<AtomicBool>,
    }
    #[async_trait]
    impl Approver for RecordingApprover {
        async fn approve(
            &self,
            _message_id: &str,
            _call_id: &str,
            _name: &str,
            _safety: Safety,
            _args: &serde_json::Value,
        ) -> bool {
            self.consulted.store(true, Ordering::SeqCst);
            true
        }
    }

    #[tokio::test]
    async fn plan_mode_hard_blocks_dispatch_of_a_hidden_tool() {
        // A Plan-mode model that names a hidden mutating tool (`bash`) -- e.g. via
        // prompt injection -- must be hard-blocked at dispatch, *before* the approval
        // gate, not merely hidden from the schema (#264 review blocker).
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "do it".into());
        let registry = ToolRegistry::with_defaults();
        let root = dir.path().to_path_buf();
        let consulted = Arc::new(AtomicBool::new(false));
        let approve = RecordingApprover {
            consulted: consulted.clone(),
        };
        let provider = ToolThenText {
            calls: AtomicUsize::new(0),
        };

        let plan = ToolContext {
            registry: &registry,
            root: &root,
            approve: &approve,
            max_iterations: 8,
            depth: 0,
            max_depth: 1,
            allowed: None,
            mode: Mode::Plan,
        };

        run_turn(
            &provider,
            &store,
            &plan,
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        // The approver was never consulted -- the block is structural, independent of
        // model or approver behaviour.
        assert!(
            !consulted.load(Ordering::SeqCst),
            "Plan-mode dispatch must hard-block before the approval gate"
        );

        // The tool never ran; the model gets a clear, actionable Plan-mode error.
        let history = store.get_messages(&s.id);
        let tool_result = history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("the blocked call still produces a tool result");
        assert!(
            tool_result.content.contains("not available in Plan mode"),
            "{}",
            tool_result.content
        );
        assert!(
            !tool_result.content.contains("wired"),
            "bash must not have executed: {}",
            tool_result.content
        );
    }

    struct AlwaysApprove;
    #[async_trait]
    impl Approver for AlwaysApprove {
        async fn approve(
            &self,
            _message_id: &str,
            _call_id: &str,
            _name: &str,
            _safety: Safety,
            _args: &serde_json::Value,
        ) -> bool {
            true
        }
    }

    struct AlwaysDeny;
    #[async_trait]
    impl Approver for AlwaysDeny {
        async fn approve(
            &self,
            _message_id: &str,
            _call_id: &str,
            _name: &str,
            _safety: Safety,
            _args: &serde_json::Value,
        ) -> bool {
            false
        }
    }

    /// Approves, but cancels the turn first — to exercise the cancel-mid-loop path.
    struct CancelOnApprove(CancelToken);
    #[async_trait]
    impl Approver for CancelOnApprove {
        async fn approve(
            &self,
            _message_id: &str,
            _call_id: &str,
            _name: &str,
            _safety: Safety,
            _args: &serde_json::Value,
        ) -> bool {
            self.0.cancel();
            true
        }
    }

    /// Yields once before approving, proving the loop actually awaits the decision.
    struct YieldThenApprove;
    #[async_trait]
    impl Approver for YieldThenApprove {
        async fn approve(
            &self,
            _message_id: &str,
            _call_id: &str,
            _name: &str,
            _safety: Safety,
            _args: &serde_json::Value,
        ) -> bool {
            tokio::task::yield_now().await;
            true
        }
    }

    fn ctx<'a>(
        registry: &'a ToolRegistry,
        root: &'a Path,
        approve: &'a dyn Approver,
    ) -> ToolContext<'a> {
        ToolContext::new(registry, root, approve, 8)
    }

    struct TextProvider;

    #[async_trait]
    impl Provider for TextProvider {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let chunks = vec![
                Ok(Chunk {
                    delta: "Hel".into(),
                    ..Chunk::default()
                }),
                Ok(Chunk {
                    delta: "lo".into(),
                    done: true,
                    ..Chunk::default()
                }),
            ];
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    /// Emits a reasoning stream then a text answer, to verify run_turn persists
    /// the accumulated CoT onto the assistant message (#375 PR-1).
    struct ReasoningThenText;

    #[async_trait]
    impl Provider for ReasoningThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let chunks = vec![
                Ok(Chunk {
                    reasoning_delta: "let me ".into(),
                    ..Chunk::default()
                }),
                Ok(Chunk {
                    reasoning_delta: "think".into(),
                    ..Chunk::default()
                }),
                Ok(Chunk {
                    delta: "42".into(),
                    done: true,
                    ..Chunk::default()
                }),
            ];
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    /// First call requests a `bash` tool call; second call returns plain text.
    struct ToolThenText {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for ToolThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("call_1".into()),
                        name: Some("bash".into()),
                        arguments: r#"{"command":"echo wired"}"#.into(),
                    }],
                    done: true,
                    ..Chunk::default()
                })]
            } else {
                vec![Ok(Chunk {
                    delta: "done: wired".into(),
                    done: true,
                    ..Chunk::default()
                })]
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    /// First call requests an `ask_user` tool call; second call returns plain text.
    struct AskThenText {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for AskThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("ask_1".into()),
                        name: Some("ask_user".into()),
                        arguments: r#"{"question":"Which file?"}"#.into(),
                    }],
                    done: true,
                    ..Chunk::default()
                })]
            } else {
                vec![Ok(Chunk {
                    delta: "using main.rs".into(),
                    done: true,
                    ..Chunk::default()
                })]
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    /// First call requests an `agent` (sub-agent) call; the child's call returns a
    /// summary; the parent's final call returns plain text. One shared counter drives
    /// parent and child turns through the same provider instance.
    struct AgentThenText {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for AgentThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = match n {
                0 => vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("agent_1".into()),
                        name: Some("agent".into()),
                        arguments: r#"{"task":"audit the foo module"}"#.into(),
                    }],
                    done: true,
                    ..Chunk::default()
                })],
                1 => vec![Ok(Chunk {
                    delta: "child: audit complete, 0 issues".into(),
                    done: true,
                    ..Chunk::default()
                })],
                _ => vec![Ok(Chunk {
                    delta: "parent: delegated and done".into(),
                    done: true,
                    ..Chunk::default()
                })],
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    /// Answers an interactive `ask`; denies everything that needs approval (it should
    /// never be asked to approve an interactive tool).
    struct CannedAnswer(&'static str);
    #[async_trait]
    impl Approver for CannedAnswer {
        async fn approve(
            &self,
            _message_id: &str,
            _call_id: &str,
            _name: &str,
            _safety: Safety,
            _args: &serde_json::Value,
        ) -> bool {
            false
        }
        async fn ask(
            &self,
            _message_id: &str,
            _call_id: &str,
            args: &serde_json::Value,
        ) -> Option<String> {
            // The host receives the tool args and reads the `question` field.
            assert_eq!(args["question"], "Which file?");
            Some(self.0.to_string())
        }
    }

    #[tokio::test]
    async fn streams_and_persists_text_turn() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "hi".into());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;

        let mut tokens = String::new();
        let mut done = false;
        let msg = run_turn(
            &TextProvider,
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| match ev {
                AgentEvent::Token { delta, .. } => tokens.push_str(&delta),
                AgentEvent::Done { .. } => done = true,
                AgentEvent::Error { .. } => panic!("unexpected error"),
                _ => {}
            },
        )
        .await
        .unwrap();

        assert_eq!(tokens, "Hello");
        assert!(done);
        assert_eq!(msg.content, "Hello");
    }

    #[tokio::test]
    async fn persists_reasoning_onto_assistant_message() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "what is the answer?".into());
        let registry = ToolRegistry::with_defaults();
        let approve = AlwaysApprove;
        let provider = ReasoningThenText;

        let mut reasoning_seen = String::new();
        let msg = run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            true,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::Reasoning { delta, .. } = ev {
                    reasoning_seen.push_str(&delta);
                }
            },
        )
        .await
        .unwrap();

        // Still streamed to the FE...
        assert_eq!(reasoning_seen, "let me think");
        assert_eq!(msg.content, "42");
        // ...and now also persisted on the message for later round-tripping.
        assert_eq!(msg.reasoning.as_deref(), Some("let me think"));
        let history = store.get_messages(&s.id);
        assert_eq!(
            history.last().unwrap().reasoning.as_deref(),
            Some("let me think")
        );
    }

    #[tokio::test]
    async fn no_reasoning_leaves_column_null() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "hi".into());
        let registry = ToolRegistry::with_defaults();
        let approve = AlwaysApprove;
        // TextProvider emits no reasoning; even with reasoning enabled the column
        // must stay NULL (skip-empty guard).
        let msg = run_turn(
            &TextProvider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            true,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();
        assert!(msg.reasoning.is_none());
    }

    #[tokio::test]
    async fn executes_tool_then_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "run echo".into());
        let registry = ToolRegistry::with_defaults();
        let approve = AlwaysApprove;
        let provider = ToolThenText {
            calls: AtomicUsize::new(0),
        };

        let mut started = 0;
        let mut finished_ok = false;
        let mut final_text = String::new();
        let msg = run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| match ev {
                AgentEvent::ToolCallStarted { name, .. } => {
                    assert_eq!(name, "bash");
                    started += 1;
                }
                AgentEvent::ToolCallFinished {
                    success, result, ..
                } => {
                    finished_ok = success;
                    assert!(result.contains("wired"));
                }
                AgentEvent::Token { delta, .. } => final_text.push_str(&delta),
                AgentEvent::Reasoning { .. } => {}
                AgentEvent::Error { message } => panic!("error: {message}"),
                AgentEvent::Done { .. } => {}
                AgentEvent::MemoryFlushed { .. } => {}
            },
        )
        .await
        .unwrap();

        assert_eq!(started, 1);
        assert!(finished_ok);
        assert_eq!(final_text, "done: wired");
        assert_eq!(msg.content, "done: wired");

        // History should be: user, assistant(tool_calls), tool(result), assistant(final).
        let history = store.get_messages(&s.id);
        assert_eq!(history.len(), 4);
        assert_eq!(history[1].role, Role::Assistant);
        assert!(history[1].tool_calls.is_some());
        assert_eq!(history[2].role, Role::Tool);
        assert_eq!(history[2].tool_call_id.as_deref(), Some("call_1"));
    }

    /// #44: an `ask_user` call routes to `Approver::ask`; the answer becomes the tool
    /// result and the turn resumes, with well-formed history.
    #[tokio::test]
    async fn ask_user_round_trips_answer_as_tool_result() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "edit the file".into());
        let registry = ToolRegistry::with_defaults();
        let approve = CannedAnswer("main.rs");
        let provider = AskThenText {
            calls: AtomicUsize::new(0),
        };

        let mut started_name = String::new();
        let mut result = String::new();
        let mut ok = false;
        let mut final_text = String::new();
        let msg = run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| match ev {
                AgentEvent::ToolCallStarted { name, .. } => started_name = name,
                AgentEvent::ToolCallFinished {
                    success, result: r, ..
                } => {
                    ok = success;
                    result = r;
                }
                AgentEvent::Token { delta, .. } => final_text.push_str(&delta),
                AgentEvent::Reasoning { .. } => {}
                AgentEvent::Error { message } => panic!("error: {message}"),
                AgentEvent::Done { .. } => {}
                AgentEvent::MemoryFlushed { .. } => {}
            },
        )
        .await
        .unwrap();

        assert_eq!(started_name, "ask_user");
        assert!(ok, "an answered question is a successful tool result");
        assert_eq!(result, "main.rs");
        assert_eq!(final_text, "using main.rs");
        assert_eq!(msg.content, "using main.rs");

        // History: user, assistant(tool_calls), tool(answer), assistant(final).
        let history = store.get_messages(&s.id);
        assert_eq!(history.len(), 4);
        assert_eq!(history[2].role, Role::Tool);
        assert_eq!(history[2].tool_call_id.as_deref(), Some("ask_1"));
        assert_eq!(history[2].content, "main.rs");
    }

    /// #44: a dismissed question (the default `ask` returns `None`) still emits a
    /// matching tool result, so history never goes malformed.
    #[tokio::test]
    async fn dismissed_ask_emits_tool_result_not_hang() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "edit the file".into());
        let registry = ToolRegistry::with_defaults();
        // AlwaysDeny uses the default `ask` (returns None) -> dismissed.
        let approve = AlwaysDeny;
        let provider = AskThenText {
            calls: AtomicUsize::new(0),
        };

        let mut result = String::new();
        let msg = run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::ToolCallFinished { result: r, .. } = ev {
                    result = r;
                }
            },
        )
        .await
        .unwrap();

        assert!(result.contains("no answer"));
        let history = store.get_messages(&s.id);
        assert_eq!(history[2].role, Role::Tool);
        assert_eq!(history[2].tool_call_id.as_deref(), Some("ask_1"));
        // Turn still completed with the follow-up assistant text.
        assert_eq!(msg.content, "using main.rs");
    }

    /// Cancelling mid-execution must still leave a matching tool result for every
    /// requested call, so the next turn's history stays well-formed.
    #[tokio::test]
    async fn cancel_mid_loop_backfills_tool_results() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "do two things".into());
        let registry = ToolRegistry::with_defaults();

        struct TwoCalls;
        #[async_trait]
        impl Provider for TwoCalls {
            async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
                let chunks = vec![Ok(Chunk {
                    tool_calls: vec![
                        ToolCallDelta {
                            index: 0,
                            id: Some("call_a".into()),
                            name: Some("bash".into()),
                            arguments: r#"{"command":"touch a"}"#.into(),
                        },
                        ToolCallDelta {
                            index: 1,
                            id: Some("call_b".into()),
                            name: Some("bash".into()),
                            arguments: r#"{"command":"touch b"}"#.into(),
                        },
                    ],
                    done: true,
                    ..Chunk::default()
                })];
                Ok(futures_util::stream::iter(chunks).boxed())
            }
        }

        // Approving the first (write) call cancels the turn, so the second is skipped.
        let cancel = CancelToken::new();
        let approve = CancelOnApprove(cancel.clone());

        let msg = run_turn(
            &TwoCalls,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            cancel,
            |_| {},
        )
        .await
        .unwrap();

        // Every requested tool call must have a matching Role::Tool reply.
        let history = store.get_messages(&s.id);
        let assistant = history
            .iter()
            .find(|m| m.tool_calls.is_some())
            .expect("assistant tool-call message");
        let requested: Vec<&str> = assistant
            .tool_calls
            .as_ref()
            .unwrap()
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        let replied: Vec<String> = history
            .iter()
            .filter(|m| m.role == Role::Tool)
            .filter_map(|m| m.tool_call_id.clone())
            .collect();
        for id in &requested {
            assert!(
                replied.iter().any(|r| r == id),
                "missing tool result for {id}"
            );
        }
        // The skipped call is recorded as cancelled.
        assert!(history
            .iter()
            .any(|m| m.role == Role::Tool && m.content == "[cancelled]"));
        // The final bubble is never empty.
        assert!(!msg.content.is_empty());
    }

    /// Dropping the `run_turn` future mid tool-loop (window closed, runtime torn
    /// down, or a superseding turn) must NOT leave an assistant `tool_use` without
    /// a matching tool result — strict providers reject that on the next turn
    /// (#316). The cooperative-cancel backfill (`cancel_mid_loop_backfills_tool_results`)
    /// only fires if execution reaches it; a dropped future skips it. The RAII
    /// guard closes that gap.
    #[tokio::test]
    async fn dropped_future_backfills_tool_results() {
        use std::future::Future;
        use std::task::{Context, Poll};

        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "do two things".into());
        let registry = ToolRegistry::with_defaults();

        // Two write (approval-gated) calls; the loop parks on the first call's
        // approval, which is exactly the window between `attach_tool_calls` and the
        // first tool result.
        struct TwoWrites;
        #[async_trait]
        impl Provider for TwoWrites {
            async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
                let chunks = vec![Ok(Chunk {
                    tool_calls: vec![
                        ToolCallDelta {
                            index: 0,
                            id: Some("call_a".into()),
                            name: Some("bash".into()),
                            arguments: r#"{"command":"touch a"}"#.into(),
                        },
                        ToolCallDelta {
                            index: 1,
                            id: Some("call_b".into()),
                            name: Some("bash".into()),
                            arguments: r#"{"command":"touch b"}"#.into(),
                        },
                    ],
                    done: true,
                    ..Chunk::default()
                })];
                Ok(futures_util::stream::iter(chunks).boxed())
            }
        }

        // Never resolves: the turn future parks forever awaiting approval, so we
        // can drop it while a `tool_use` is persisted but un-resulted.
        struct NeverApprove;
        #[async_trait]
        impl Approver for NeverApprove {
            async fn approve(
                &self,
                _message_id: &str,
                _call_id: &str,
                _name: &str,
                _safety: Safety,
                _args: &serde_json::Value,
            ) -> bool {
                std::future::pending::<()>().await;
                unreachable!("pending() never resolves")
            }
        }

        let approve = NeverApprove;
        let tool_ctx = ctx(&registry, dir.path(), &approve);
        let fut = run_turn(
            &TwoWrites,
            &store,
            &tool_ctx,
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |_| {},
        );

        // Poll the turn future a bounded number of times so it reaches and parks on
        // the first approval, then drop it — simulating the host abandoning the turn.
        let mut fut = Box::pin(fut);
        let waker = futures_util::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        for _ in 0..256 {
            match fut.as_mut().poll(&mut cx) {
                Poll::Pending => {}
                Poll::Ready(_) => panic!("turn should park on NeverApprove, not complete"),
            }
        }
        drop(fut);

        // Every requested tool call has a matching Role::Tool reply despite the drop.
        let history = store.get_messages(&s.id);
        let assistant = history
            .iter()
            .find(|m| m.tool_calls.is_some())
            .expect("assistant tool-call message persisted before the drop");
        let requested: Vec<&str> = assistant
            .tool_calls
            .as_ref()
            .unwrap()
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        let replied: Vec<String> = history
            .iter()
            .filter(|m| m.role == Role::Tool)
            .filter_map(|m| m.tool_call_id.clone())
            .collect();
        for id in &requested {
            assert!(
                replied.iter().any(|r| r == id),
                "dropped turn left tool_use {id} without a result"
            );
        }
    }

    #[tokio::test]
    async fn denied_write_tool_reports_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "old\n").unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "edit it".into());
        let registry = ToolRegistry::with_defaults();
        // Deny everything that needs approval.
        let deny = AlwaysDeny;

        struct EditProvider {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl Provider for EditProvider {
            async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                let chunks = if n == 0 {
                    vec![Ok(Chunk {
                        tool_calls: vec![ToolCallDelta {
                            index: 0,
                            id: Some("call_e".into()),
                            name: Some("edit".into()),
                            arguments: r#"{"path":"f.txt","old_str":"old","new_str":"new"}"#.into(),
                        }],
                        done: true,
                        ..Chunk::default()
                    })]
                } else {
                    vec![Ok(Chunk {
                        delta: "ok".into(),
                        done: true,
                        ..Chunk::default()
                    })]
                };
                Ok(futures_util::stream::iter(chunks).boxed())
            }
        }

        let mut denied_reported = false;
        run_turn(
            &EditProvider {
                calls: AtomicUsize::new(0),
            },
            &store,
            &ctx(&registry, dir.path(), &deny),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::ToolCallFinished {
                    success, result, ..
                } = ev
                {
                    if !success && result.contains("not approved") {
                        denied_reported = true;
                    }
                }
            },
        )
        .await
        .unwrap();

        assert!(denied_reported);
        // The file must be untouched because the edit was denied.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "old\n"
        );
    }

    /// The loop must await an async approval decision before running the tool.
    #[tokio::test]
    async fn awaits_async_approval() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "run echo".into());
        let registry = ToolRegistry::with_defaults();
        let approve = YieldThenApprove;
        let provider = ToolThenText {
            calls: AtomicUsize::new(0),
        };

        let mut finished_ok = false;
        run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::ToolCallFinished { success, .. } = ev {
                    finished_ok = success;
                }
            },
        )
        .await
        .unwrap();

        assert!(finished_ok, "tool should run after async approval resolves");
    }

    /// Captures the `ChatRequest` it receives so a test can assert what reached the
    /// provider (the system prompt is transient — never stored in history).
    struct RecordingProvider {
        seen: Arc<std::sync::Mutex<Vec<ChatMessage>>>,
    }

    #[async_trait]
    impl Provider for RecordingProvider {
        async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
            *self.seen.lock().unwrap() = req.messages;
            Ok(futures_util::stream::iter(vec![Ok(Chunk {
                delta: "ok".into(),
                done: true,
                ..Chunk::default()
            })])
            .boxed())
        }
    }

    #[tokio::test]
    async fn system_prompt_is_injected_into_request_not_history() {
        use ff_skills::SkillRegistry;

        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("rust-debug");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: rust-debug\ndescription: Systematic Rust debugging\nversion: 0.1.0\n---\nBisect with bash.\n",
        )
        .unwrap();
        let (skills, errs) = SkillRegistry::load_dir(dir.path());
        assert!(errs.is_empty());

        let user = UserContext {
            local_date: "2026-06-13".into(),
            timezone: "America/Chicago".into(),
            time_of_day: TimeOfDay::Morning,
        };
        let system = build_system_prompt(None, &skills, &[], &user, None, Mode::default());

        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "hi".into());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = RecordingProvider { seen: seen.clone() };

        run_turn(
            &provider,
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            Some(&system),
            false,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let msgs = seen.lock().unwrap();
        assert_eq!(msgs[0].role, "system");
        let sys = msgs[0].content.as_deref().unwrap();
        assert!(
            sys.contains("- rust-debug: Systematic Rust debugging"),
            "{sys}"
        );
        assert!(sys.contains("Current: 2026-06-13, morning (America/Chicago)."));
        assert_eq!(msgs[1].role, "user");

        // The system prompt must not be persisted: history is just [user, assistant].
        let history = store.get_messages(&s.id);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, Role::User);
        assert_eq!(history[1].role, Role::Assistant);
    }

    #[tokio::test]
    async fn subagent_delegates_and_returns_summary() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "delegate an audit".into());
        let registry = ToolRegistry::with_defaults();
        let root = dir.path().to_path_buf();
        let approve = AlwaysApprove;
        let provider = AgentThenText {
            calls: AtomicUsize::new(0),
        };

        let msg = run_turn(
            &provider,
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        // Parent finished with its own answer, having delegated mid-turn.
        assert_eq!(msg.content, "parent: delegated and done");

        // The child's summary came back as the parent's tool result.
        let history = store.get_messages(&s.id);
        let tool_result = history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("parent should have a tool result for the agent call");
        assert_eq!(tool_result.content, "child: audit complete, 0 issues");

        // The ephemeral child session was deleted — only the parent remains.
        assert_eq!(store.list_sessions().len(), 1);
    }

    #[tokio::test]
    async fn subagent_depth_guard_refuses_nested_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "try to delegate from a child".into());
        let registry = ToolRegistry::with_defaults();
        let root = dir.path().to_path_buf();
        let approve = AlwaysApprove;
        let provider = AgentThenText {
            calls: AtomicUsize::new(0),
        };

        // Simulate an agent already at the depth cap.
        let at_cap = ToolContext {
            registry: &registry,
            root: &root,
            approve: &approve,
            max_iterations: 8,
            depth: 1,
            max_depth: 1,
            allowed: None,
            mode: Mode::default(),
        };

        run_turn(
            &provider,
            &store,
            &at_cap,
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let history = store.get_messages(&s.id);
        let tool_result = history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("the refused spawn still produces a tool result");
        assert!(
            tool_result.content.contains("max delegation depth"),
            "{}",
            tool_result.content
        );
        // No child session was ever created.
        assert_eq!(store.list_sessions().len(), 1);
    }

    #[tokio::test]
    async fn subagent_allowlist_blocks_disallowed_tool() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "scoped read-only run".into());
        let registry = ToolRegistry::with_defaults();
        let root = dir.path().to_path_buf();
        let approve = AlwaysApprove;
        let provider = ToolThenText {
            calls: AtomicUsize::new(0),
        };

        // A sub-agent scoped to read-only tools tries to call `bash`.
        let scoped = ToolContext {
            registry: &registry,
            root: &root,
            approve: &approve,
            max_iterations: 8,
            depth: 1,
            max_depth: 1,
            allowed: Some(["view".to_string()].into_iter().collect()),
            mode: Mode::default(),
        };

        run_turn(
            &provider,
            &store,
            &scoped,
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let history = store.get_messages(&s.id);
        let tool_result = history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("the disallowed call still produces a tool result");
        assert!(
            tool_result.content.contains("not permitted"),
            "{}",
            tool_result.content
        );
    }

    /// Always requests a tool call (never finishes on its own), and records, per
    /// request, whether the wrap-up nudge system message was present. Lets a test
    /// drive the loop to its iteration cap and assert when the nudge fires.
    struct RecordingToolLooper {
        nudge_seen: Arc<std::sync::Mutex<Vec<bool>>>,
    }

    #[async_trait]
    impl Provider for RecordingToolLooper {
        async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let saw = req.messages.iter().any(|m| {
                m.role == "system"
                    && m.content
                        .as_deref()
                        .is_some_and(|c| c.contains("final step before the tool-call limit"))
            });
            self.nudge_seen.lock().unwrap().push(saw);
            Ok(futures_util::stream::iter(vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_1".into()),
                    name: Some("bash".into()),
                    arguments: r#"{"command":"echo wired"}"#.into(),
                }],
                done: true,
                ..Chunk::default()
            })])
            .boxed())
        }
    }

    #[tokio::test]
    async fn wrap_up_nudge_fires_only_on_final_iteration() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "keep going".into());
        let registry = ToolRegistry::with_defaults();
        let approve = AlwaysApprove;
        let nudge_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = RecordingToolLooper {
            nudge_seen: nudge_seen.clone(),
        };
        let tools = ToolContext::new(&registry, dir.path(), &approve, 3);

        run_turn(
            &provider,
            &store,
            &tools,
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let seen = nudge_seen.lock().unwrap();
        // The provider is hit once per iteration, up to the cap.
        assert_eq!(seen.len(), 3, "loop should run to the iteration cap");
        // The nudge is injected only on the final iteration (remaining == 1).
        assert_eq!(seen.as_slice(), &[false, false, true]);
    }

    #[tokio::test]
    async fn no_wrap_up_nudge_when_cap_is_one() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "keep going".into());
        let registry = ToolRegistry::with_defaults();
        let approve = AlwaysApprove;
        let nudge_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = RecordingToolLooper {
            nudge_seen: nudge_seen.clone(),
        };
        let tools = ToolContext::new(&registry, dir.path(), &approve, 1);

        run_turn(
            &provider,
            &store,
            &tools,
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let seen = nudge_seen.lock().unwrap();
        // With a single-iteration cap there is no "next step" to wrap up toward.
        assert_eq!(seen.as_slice(), &[false]);
    }

    // ----- #244 R4: tool-argument parse feedback -----

    /// First call emits a tool call with malformed JSON arguments; the second returns
    /// plain text (the model "self-correcting" after seeing the parse error).
    struct BadArgsThenText {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Provider for BadArgsThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("call_bad".into()),
                        name: Some("bash".into()),
                        arguments: "{not valid json".into(),
                    }],
                    done: true,
                    ..Chunk::default()
                })]
            } else {
                vec![Ok(Chunk {
                    delta: "fixed and done".into(),
                    done: true,
                    ..Chunk::default()
                })]
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    #[tokio::test]
    async fn invalid_tool_args_return_parse_error_and_loop_continues() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "go".into());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;
        let calls = Arc::new(AtomicUsize::new(0));

        let mut finished_success = true;
        let msg = run_turn(
            &BadArgsThenText {
                calls: calls.clone(),
            },
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::ToolCallFinished { success, .. } = ev {
                    finished_success = success;
                }
            },
        )
        .await
        .unwrap();

        // The model got a second turn and produced a real answer.
        assert_eq!(msg.content, "fixed and done");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // The bad call surfaced as a failed tool result, not a silent Null.
        assert!(!finished_success);

        // History integrity: the assistant tool_calls message has a matching tool
        // reply, and that reply tells the model its JSON was invalid.
        let history = store.get_messages(&s.id);
        let tool_reply = history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("a tool result must exist for the bad call");
        assert!(
            tool_reply.content.contains("not valid JSON"),
            "tool reply should explain the parse failure, got: {}",
            tool_reply.content
        );
    }

    // ----- #244 R1: transient-error retry with backoff -----

    /// Returns a transient setup error for the first `fails` calls, then a text turn.
    struct FlakySetup {
        fails: usize,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Provider for FlakySetup {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fails {
                return Err(LlmError::Transport("connection refused".into()));
            }
            let chunks = vec![Ok(Chunk {
                delta: "recovered".into(),
                done: true,
                ..Chunk::default()
            })];
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    /// Always fails the request setup with a fatal (client) error.
    struct FatalSetup {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Provider for FatalSetup {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(LlmError::Api {
                status: 401,
                message: "unauthorized".into(),
            })
        }
    }

    /// First call yields a transient error mid-stream; `emit_first` controls whether a
    /// token is emitted before the error. Later calls return a text turn.
    struct MidStreamErr {
        calls: Arc<AtomicUsize>,
        emit_first: bool,
    }
    #[async_trait]
    impl Provider for MidStreamErr {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                let mut chunks: Vec<Result<Chunk, LlmError>> = Vec::new();
                if self.emit_first {
                    chunks.push(Ok(Chunk {
                        delta: "partial".into(),
                        ..Chunk::default()
                    }));
                }
                chunks.push(Err(LlmError::Transport("reset".into())));
                return Ok(futures_util::stream::iter(chunks).boxed());
            }
            let chunks = vec![Ok(Chunk {
                delta: "recovered".into(),
                done: true,
                ..Chunk::default()
            })];
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    async fn run_text_turn(provider: &dyn Provider) -> (Result<Message, AgentError>, bool) {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "hi".into());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;
        let mut errored = false;
        let res = run_turn(
            provider,
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::Error { .. } = ev {
                    errored = true;
                }
            },
        )
        .await;
        (res, errored)
    }

    #[tokio::test(start_paused = true)]
    async fn transient_setup_error_retries_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = FlakySetup {
            fails: 2,
            calls: calls.clone(),
        };
        let (res, errored) = run_text_turn(&provider).await;
        assert_eq!(res.unwrap().content, "recovered");
        assert!(!errored, "recovered turn should not surface an error");
        assert_eq!(calls.load(Ordering::SeqCst), 3, "two retries then success");
    }

    #[tokio::test(start_paused = true)]
    async fn fatal_setup_error_surfaces_without_retry() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = FatalSetup {
            calls: calls.clone(),
        };
        let (res, errored) = run_text_turn(&provider).await;
        assert!(res.is_err(), "fatal error must surface");
        assert!(errored, "an Error event should fire");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "fatal errors are not retried"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn mid_stream_error_before_emit_retries() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = MidStreamErr {
            calls: calls.clone(),
            emit_first: false,
        };
        let (res, errored) = run_text_turn(&provider).await;
        assert_eq!(res.unwrap().content, "recovered");
        assert!(!errored);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "retried after pre-emit blip"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn mid_stream_error_after_emit_surfaces() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = MidStreamErr {
            calls: calls.clone(),
            emit_first: true,
        };
        let (res, errored) = run_text_turn(&provider).await;
        assert!(
            res.is_err(),
            "error after streamed output must surface, not replay"
        );
        assert!(errored);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no retry once tokens reached the UI"
        );
    }

    // ----- #244 R2: repeated-call / no-progress guard -----

    /// Always emits the identical `bash` tool call, recording per-request whether the
    /// repeat-nudge system message was present -- a model stuck in a no-progress loop.
    struct RepeatProvider {
        calls: Arc<AtomicUsize>,
        saw_nudge: Arc<std::sync::Mutex<Vec<bool>>>,
    }
    #[async_trait]
    impl Provider for RepeatProvider {
        async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let saw = req.messages.iter().any(|m| {
                m.role == "system"
                    && m.content
                        .as_deref()
                        .is_some_and(|c| c.contains("without making progress"))
            });
            self.saw_nudge.lock().unwrap().push(saw);
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some(format!("call_{n}")),
                    name: Some("bash".into()),
                    arguments: r#"{"command":"echo loop"}"#.into(),
                }],
                done: true,
                ..Chunk::default()
            })];
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    #[tokio::test]
    async fn repeated_identical_calls_nudge_then_break() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "go".into());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;
        let calls = Arc::new(AtomicUsize::new(0));
        let saw_nudge = Arc::new(std::sync::Mutex::new(Vec::new()));

        // A generous cap so the *guard* (not the cap) is what stops the turn.
        let tools = ToolContext::new(&registry, &root, &approve, 20);
        let msg = run_turn(
            &RepeatProvider {
                calls: calls.clone(),
                saw_nudge: saw_nudge.clone(),
            },
            &store,
            &tools,
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        // Broke at REPEAT_BREAK_AT (5), well before the cap of 20.
        assert_eq!(calls.load(Ordering::SeqCst), REPEAT_BREAK_AT);
        // The corrective nudge was injected at least once before the break.
        assert!(
            saw_nudge.lock().unwrap().iter().any(|&b| b),
            "the repeat nudge should have been sent"
        );
        // The turn ends with a clear repeated-call notice, not the generic cap notice.
        assert!(
            msg.content.contains("repeated the identical"),
            "got: {}",
            msg.content
        );
    }

    // ----- #244 R7 + R1/R2 follow-up nits: loop polish -----

    /// Cancels the turn, then returns a transient setup error -- so the retry backoff
    /// runs with the token already cancelled. Counts how many times the provider is hit.
    struct CancelDuringBackoff {
        calls: Arc<AtomicUsize>,
        cancel: CancelToken,
    }
    #[async_trait]
    impl Provider for CancelDuringBackoff {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.cancel.cancel();
            Err(LlmError::Transport("reset".into()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn no_extra_chat_stream_after_cancel_during_backoff() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "hi".into());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;
        let calls = Arc::new(AtomicUsize::new(0));
        let cancel = CancelToken::new();
        let provider = CancelDuringBackoff {
            calls: calls.clone(),
            cancel: cancel.clone(),
        };

        let _ = run_turn(
            &provider,
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            cancel,
            |_| {},
        )
        .await;

        // Without the cancel-after-backoff check the loop would issue two more wasted
        // calls before surfacing; the fix stops it dead after the first (#244 R1 nit).
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "cancel during backoff must not issue another provider call"
        );
    }

    /// Returns an empty (no text, no tool call) but successful stream for the first
    /// `empties` calls, then a real text turn -- a provider hiccup (#244 R7).
    struct EmptyThenText {
        empties: usize,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Provider for EmptyThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.empties {
                return Ok(futures_util::stream::iter(vec![Ok(Chunk {
                    done: true,
                    ..Chunk::default()
                })])
                .boxed());
            }
            Ok(futures_util::stream::iter(vec![Ok(Chunk {
                delta: "recovered".into(),
                done: true,
                ..Chunk::default()
            })])
            .boxed())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn empty_response_retries_then_recovers() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = EmptyThenText {
            empties: 1,
            calls: calls.clone(),
        };
        let (res, errored) = run_text_turn(&provider).await;
        assert_eq!(res.unwrap().content, "recovered");
        assert!(
            !errored,
            "an anomaly that recovers should not surface an error"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "one empty response retried, then success"
        );
    }

    /// Always returns an empty successful stream -- a persistent anomaly.
    struct AlwaysEmpty {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Provider for AlwaysEmpty {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(futures_util::stream::iter(vec![Ok(Chunk {
                done: true,
                ..Chunk::default()
            })])
            .boxed())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn empty_response_exhausts_to_notice() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "go".into());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;
        let calls = Arc::new(AtomicUsize::new(0));

        let msg = run_turn(
            &AlwaysEmpty {
                calls: calls.clone(),
            },
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        // The empty-response retry is bounded by the provider-attempt cap within one
        // iteration -- not an infinite spin.
        assert_eq!(calls.load(Ordering::SeqCst), MAX_PROVIDER_ATTEMPTS);
        // ...and the turn ends with a clear notice, never a silent empty bubble.
        assert!(
            msg.content.contains("empty response"),
            "got: {}",
            msg.content
        );
    }

    #[tokio::test]
    async fn repeat_nudge_persists_through_the_recovery_window() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "go".into());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;
        let calls = Arc::new(AtomicUsize::new(0));
        let saw_nudge = Arc::new(std::sync::Mutex::new(Vec::new()));

        let tools = ToolContext::new(&registry, &root, &approve, 20);
        run_turn(
            &RepeatProvider {
                calls: calls.clone(),
                saw_nudge: saw_nudge.clone(),
            },
            &store,
            &tools,
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        // Five requests fire before the break at REPEAT_BREAK_AT (5). The nudge is
        // re-armed across the whole window, so both the count-4 request (index 3) and
        // the count-5 request (index 4) carry it -- not just the first (#244 R2 nit).
        let seen = saw_nudge.lock().unwrap();
        assert_eq!(
            seen.as_slice(),
            &[false, false, false, true, true],
            "{seen:?}"
        );
    }

    #[tokio::test]
    async fn done_event_reports_estimated_token_count() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "hi".into());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;

        let seen = std::sync::Mutex::new(None);
        run_turn(
            &TextProvider,
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::Done { token_count, .. } = ev {
                    *seen.lock().unwrap() = Some(token_count);
                }
            },
        )
        .await
        .unwrap();

        // The Done event must carry a populated, non-zero estimate (#244 R6) rather
        // than the previous hardcoded None.
        let tc = seen.lock().unwrap().expect("Done event was emitted");
        let tc = tc.expect("token_count must be populated, not None");
        assert!(tc > 0, "estimated token count should be positive, got {tc}");
    }

    // ----- #244 R8: oversized tool-result history truncation -----

    #[test]
    fn truncate_tool_result_passes_through_small_input() {
        let small = "ok";
        assert_eq!(truncate_tool_result(small), small);
        let exact = "x".repeat(TOOL_RESULT_MAX_BYTES);
        assert_eq!(truncate_tool_result(&exact), exact);
    }

    #[test]
    fn truncate_tool_result_caps_and_keeps_head_and_tail() {
        let big = format!("HEAD{}TAIL", "x".repeat(TOOL_RESULT_MAX_BYTES * 2));
        let out = truncate_tool_result(&big);
        assert!(
            out.len() <= TOOL_RESULT_MAX_BYTES,
            "truncated to {} bytes, cap {}",
            out.len(),
            TOOL_RESULT_MAX_BYTES
        );
        assert!(out.starts_with("HEAD"), "head slice must survive");
        assert!(out.ends_with("TAIL"), "tail slice must survive");
        assert!(out.contains("truncated"), "marker must be present");
    }

    #[test]
    fn truncate_tool_result_respects_utf8_boundaries() {
        // A grinning-face emoji is 4 bytes; a naive byte slice mid-codepoint would
        // panic. The output must stay valid UTF-8 and within the cap.
        let big = "😀".repeat(TOOL_RESULT_MAX_BYTES);
        let out = truncate_tool_result(&big);
        assert!(out.len() <= TOOL_RESULT_MAX_BYTES);
        assert!(out.chars().count() > 0);
    }

    // ----- #378: persisted reasoning sizing -----

    #[test]
    fn truncate_reasoning_passes_through_small_input() {
        let small = "thought briefly";
        assert_eq!(truncate_reasoning(small), small);
        let exact = "x".repeat(REASONING_MAX_BYTES);
        assert_eq!(truncate_reasoning(&exact), exact);
    }

    #[test]
    fn truncate_reasoning_caps_and_keeps_tail() {
        // A chain-of-thought is most useful at its end, so unlike a tool result the
        // truncation keeps the TAIL and drops the head.
        let big = format!("HEAD{}TAIL", "x".repeat(REASONING_MAX_BYTES * 2));
        let out = truncate_reasoning(&big);
        assert!(
            out.len() <= REASONING_MAX_BYTES,
            "truncated to {} bytes, cap {}",
            out.len(),
            REASONING_MAX_BYTES
        );
        assert!(out.ends_with("TAIL"), "tail slice must survive");
        assert!(!out.contains("HEAD"), "head must be dropped (tail-biased)");
        assert!(out.contains("truncated"), "marker must be present");
    }

    #[test]
    fn truncate_reasoning_respects_utf8_boundaries() {
        // Tail-biased slicing must land on a char boundary, not mid-codepoint.
        let big = "😀".repeat(REASONING_MAX_BYTES);
        let out = truncate_reasoning(&big);
        assert!(out.len() <= REASONING_MAX_BYTES);
        assert!(out.chars().count() > 0);
    }

    /// A tool whose result is far larger than the history cap.
    struct BigResultTool {
        bytes: usize,
    }
    #[async_trait]
    impl ff_tools::Tool for BigResultTool {
        fn name(&self) -> &str {
            "big"
        }
        fn description(&self) -> &str {
            "returns a large blob"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": {} })
        }
        fn safety(&self, _args: &serde_json::Value) -> Safety {
            Safety::ReadOnly
        }
        async fn run(&self, _args: serde_json::Value, _root: &Path) -> ff_tools::ToolOutcome {
            ff_tools::ToolOutcome::ok("B".repeat(self.bytes))
        }
    }

    /// First call invokes `big`; second call returns plain text.
    struct BigToolThenText {
        calls: AtomicUsize,
    }
    #[async_trait]
    impl Provider for BigToolThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("call_1".into()),
                        name: Some("big".into()),
                        arguments: "{}".into(),
                    }],
                    done: true,
                    ..Chunk::default()
                })]
            } else {
                vec![Ok(Chunk {
                    delta: "done".into(),
                    done: true,
                    ..Chunk::default()
                })]
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    #[tokio::test]
    async fn oversized_tool_result_is_truncated_in_history_but_full_in_event() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "go".into());
        let mut registry = ToolRegistry::new();
        let full_len = TOOL_RESULT_MAX_BYTES * 3;
        registry.register(Box::new(BigResultTool { bytes: full_len }));
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;

        let mut event_result_len = 0usize;
        run_turn(
            &BigToolThenText {
                calls: AtomicUsize::new(0),
            },
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::ToolCallFinished { result, .. } = ev {
                    event_result_len = result.len();
                }
            },
        )
        .await
        .unwrap();

        // The UI event keeps the full, untruncated result.
        assert_eq!(event_result_len, full_len, "event must carry full content");

        // History (replayed to the model) is capped.
        let history = store.get_messages(&s.id);
        let tool_msg = history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("a tool result message");
        assert!(
            tool_msg.content.len() <= TOOL_RESULT_MAX_BYTES,
            "history result {} exceeds cap {}",
            tool_msg.content.len(),
            TOOL_RESULT_MAX_BYTES
        );
        assert!(
            tool_msg.content.contains("truncated"),
            "history result should carry the truncation marker"
        );
    }

    // ----- #244 R5: in-turn context-pressure flush -----

    /// Always returns the same short text turn, counting how many times it is hit.
    /// Used to observe whether a flush fired (the flush issues an extra provider
    /// call before the main turn).
    struct CountingText {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Provider for CountingText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(futures_util::stream::iter(vec![Ok(Chunk {
                delta: "ok".into(),
                done: true,
                ..Chunk::default()
            })])
            .boxed())
        }
    }

    #[tokio::test]
    async fn context_pressure_under_budget_skips_flush() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "hi".into());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;
        let calls = Arc::new(AtomicUsize::new(0));

        run_turn(
            &CountingText {
                calls: calls.clone(),
            },
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        // A tiny transcript is well under the budget fraction -> no flush, so the
        // provider is hit exactly once (the main turn).
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "under-budget turn must not trigger a flush"
        );
    }

    #[tokio::test]
    async fn context_pressure_over_budget_triggers_flush() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        // Push the proxy estimate (chars/4) over 0.75 * DEFAULT_CONTEXT_BUDGET_TOKENS:
        // 0.75 * 24_000 = 18_000 tokens -> 72_000 chars. 100k chars clears it.
        let huge = "x".repeat(100_000);
        store.add_message(&s.id, Role::User, huge);
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;
        let calls = Arc::new(AtomicUsize::new(0));

        // Sanity: the seeded transcript really is over budget.
        let pressure = ProxyTokenEstimator::default().assess(&store.get_messages(&s.id), "mock");
        assert!(
            pressure.is_over(DEFAULT_FLUSH_AT_FRACTION),
            "test precondition: transcript must exceed the flush threshold"
        );

        // A NoReply flush writes nothing, so no provenance event must fire (#283).
        let mut flush_events = 0usize;
        run_turn(
            &CountingText {
                calls: calls.clone(),
            },
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| {
                if matches!(ev, AgentEvent::MemoryFlushed { .. }) {
                    flush_events += 1;
                }
            },
        )
        .await
        .unwrap();

        // Over budget on the first iteration -> a silent flush fires (one extra
        // provider call that returns no tool calls -> NoReply) before the main turn.
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "over-budget turn must run exactly one flush before the main turn"
        );
        assert_eq!(
            flush_events, 0,
            "a flush that writes nothing (NoReply) must not emit MemoryFlushed"
        );
        // The flush is silent: it must not add any message to the visible transcript
        // (memory writes go to disk, not the session). Still just the one user msg
        // plus the single assistant reply from the main turn.
        let history = store.get_messages(&s.id);
        assert_eq!(history.len(), 2, "flush must not mutate the transcript");
    }

    /// A `memory_write` tool that records how many times it ran, so an over-budget
    /// flush can actually persist a durable fact during the test (#283).
    struct CountingMemoryWrite {
        writes: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl ff_tools::Tool for CountingMemoryWrite {
        fn name(&self) -> &str {
            "memory_write"
        }
        fn description(&self) -> &str {
            "persists a durable fact"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } }
            })
        }
        fn safety(&self, _args: &serde_json::Value) -> Safety {
            Safety::Write
        }
        async fn run(&self, _args: serde_json::Value, _root: &Path) -> ff_tools::ToolOutcome {
            self.writes.fetch_add(1, Ordering::SeqCst);
            ff_tools::ToolOutcome::ok("saved")
        }
    }

    /// Calls `memory_write` once (the flush's first request), then answers with
    /// plain text — which both terminates the flush loop and finishes the main turn.
    struct FlushWriteThenText {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Provider for FlushWriteThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("w1".into()),
                        name: Some("memory_write".into()),
                        arguments: r#"{"text":"user prefers dark mode"}"#.into(),
                    }],
                    done: true,
                    ..Chunk::default()
                })]
            } else {
                vec![Ok(Chunk {
                    delta: "ok".into(),
                    done: true,
                    ..Chunk::default()
                })]
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    #[tokio::test]
    async fn over_budget_flush_that_writes_emits_memory_flushed_event() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "x".repeat(100_000));
        let writes = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(CountingMemoryWrite {
            writes: writes.clone(),
        }));
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;

        let mut flushed: Vec<u32> = Vec::new();
        run_turn(
            &FlushWriteThenText {
                calls: Arc::new(AtomicUsize::new(0)),
            },
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::MemoryFlushed { writes, .. } = ev {
                    flushed.push(writes);
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(
            flushed,
            vec![1],
            "a flush that wrote one fact emits one MemoryFlushed carrying writes=1"
        );
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "the flush ran memory_write exactly once"
        );
        // Provenance, not mutation: the visible transcript stays user + assistant.
        assert_eq!(store.get_messages(&s.id).len(), 2);
    }
}
