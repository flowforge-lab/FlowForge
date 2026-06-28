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

use ff_core::{Message, Mode, ReasoningVisibility, Role};
use ff_llm::{ChatMessage, ChatRequest, FunctionCall, LlmError, Provider, ToolCall as LlmToolCall};
use ff_session::SessionStore;
use ff_tools::{Safety, ToolRegistry, COMPACTION_RETRIEVE_TOOL};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

mod compaction;
mod compaction_abstractive;
mod compaction_extractive;
mod system_prompt;
pub use compaction::{
    flush_due, CompactionContext, CompactionOutcome, CompactionStrategy, ContextPressure,
    ContextPressureEstimator, MemoryFlush, ProxyTokenEstimator, DEFAULT_CONTEXT_BUDGET_TOKENS,
    DEFAULT_FLUSH_AT_FRACTION,
};
pub use compaction_abstractive::{
    build_summary_prompt, summary_due, AbstractiveConfig, AbstractiveSummarizer, SummaryResult,
};
pub use compaction_extractive::{
    classify, proxy_tokens, ColdCompaction, CompactionSavings, CompressOutcome, ContentKind,
    ExtractiveCompactor, ReversibleCache, COMPACTION_MARKER_PREFIX,
};
pub use system_prompt::{build_flush_prompt, build_system_prompt, TimeOfDay, UserContext};

/// Default tool-call iteration cap for a turn when a phenotype does not override
/// it (#244 R3). A turn runs at most this many model<->tool round-trips before
/// it is forced to stop. Coding phenotypes raise this via `max_iterations` in
/// their phenotype TOML.
pub const DEFAULT_MAX_ITERATIONS: usize = 8;

/// When this many iterations (including the current one) remain before the cap,
/// the loop injects a transient "wrap up" nudge so the model has runway to
/// converge on a final answer instead of being cut mid-tool-call (#244 R3). The
/// nudge copy graduates: while still in the window it is a soft "approaching the
/// limit, start converging" message with tools still advertised; on the very last
/// iteration (`remaining == 1`) it hardens to "do not call any more tools" *and*
/// the tool schema is withheld, so the model *must* emit text rather than another
/// tool call that would be lost to the cap -- the instruction matching the
/// mechanism (RC3, #454).
const WRAP_UP_AT_REMAINING: usize = 3;

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

/// Number of most-recent messages kept byte-identical on the wire when the
/// cold-prefix extractive compaction runs. The model needs exact recent state;
/// only the cold prefix is eligible for lossy-but-reversible compaction (RFC
/// 0016 M7.1b).
const KEEP_RECENT_VERBATIM: usize = 6;

/// Context-pressure fraction at which the cold-prefix extractive compaction
/// engages as a deterministic, pre-send wire transform. Set at the same budget
/// fraction as the memory flush so the two pressure responses move together
/// (RFC 0016 M7.1b).
const EXTRACTIVE_COMPACT_AT_FRACTION: f64 = 0.75;

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

/// How many of the most-recent assistant tool-call turns keep their persisted
/// reasoning when building the wire request. Reasoning gateways (#375) replay
/// `reasoning_content` on *every* prior tool-call turn, so an N-turn task resends
/// every earlier CoT on every call -- O(n^2) prefill growth. Only the most recent
/// turns' reasoning meaningfully aids continuation, so older CoT is dropped from
/// the wire (the store keeps the verbatim original untouched). Latency win is
/// model-agnostic; correctness is unaffected because the dialect simply omits
/// absent reasoning.
const REASONING_REPLAY_KEEP: usize = 2;

/// Fraction of a model's real context window used as the compaction budget. The
/// headroom (the remaining ~20%) absorbs the model's own response and the
/// coarseness of the chars/4 proxy estimate, so compaction engages before the
/// true window is hit rather than after. Combined with the per-model window from
/// `Provider::context_window`, this stops a large-window model from being
/// force-compacted at a small fixed ceiling (#B1).
const CONTEXT_BUDGET_SAFETY: f64 = 0.8;

/// Events the agent emits during a turn. The host (Tauri shell or a test) decides
/// how to surface them — over IPC, to a channel, or into assertions.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        /// F1b (#441): the projected prefill-token estimate of every round-trip's
        /// outgoing request (post-compaction wire), in iteration order. Lets the
        /// perf epic (#426) see whether the budget / reasoning-replay-cap work
        /// actually shrank the per-iteration request size. `None` only for events
        /// that did not originate from `run_turn`.
        #[serde(skip_serializing_if = "Option::is_none")]
        prefill_estimates: Option<Vec<u32>>,
        /// F1b (#441): how many iterations this turn engaged the Tier-1 extractive
        /// cold-prefix compaction pass (RFC 0016 M7.1b).
        #[serde(skip_serializing_if = "Option::is_none")]
        tier1_fires: Option<u32>,
        /// F1b (#441): how many iterations this turn engaged the Tier-2 abstractive
        /// cold-tail summary (RFC 0016 M7.0).
        #[serde(skip_serializing_if = "Option::is_none")]
        tier2_fires: Option<u32>,
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
    /// One or more attachments on the user's turn were dropped before the request
    /// because the active model lacks vision (#338). The per-provider capability
    /// strip is otherwise silent; this turns the drop into a visible notice. Emitted
    /// once per turn (first iteration only), keyed to that turn's assistant message.
    AttachmentsDropped {
        message_id: String,
        count: u32,
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
    /// Tier-2 abstractive cold-tail summary config (RFC 0016 M7.0). Default-off;
    /// the host enables/tunes it. Sub-agents inherit the parent's setting.
    pub abstractive: AbstractiveConfig,
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
            abstractive: AbstractiveConfig::default(),
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
    /// True when both handles share the same underlying flag (i.e. clones of one
    /// token), as opposed to two independently-created tokens. Lets the host
    /// remove a session's registered token only while it is still the one a
    /// given turn owns, so a successor turn's token is never clobbered.
    pub fn ptr_eq(&self, other: &CancelToken) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
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
    let mut chat: Vec<ChatMessage> = messages
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
        .collect();
    repair_empty_tool_call_ids(&mut chat);
    cap_reasoning_replay(&mut chat, REASONING_REPLAY_KEEP);
    chat
}

/// Repair tool-call ids that a gateway persisted empty because it omitted the id
/// on its streaming delta (SiliconFlow, #512). An empty id on the wire makes the
/// gateway reject the replayed turn with a 400, so a session recorded before the
/// capture-site fix would stay wedged. Mint a stable id for each empty assistant
/// tool_call and bind the following tool result(s) to it in order: results are
/// persisted in the same index order as their calls, so a FIFO match restores
/// the pairing. Messages that already carry a non-empty id are left untouched.
///
/// The FIFO match assumes each empty-id assistant call is followed by its own
/// result in order. That holds for any well-formed transcript; the one degenerate
/// input is a *dangling* empty-id call (a turn cancelled after the assistant
/// message persisted but before its result), which leaves a stale id in `pending`
/// and could misbind a later result. Such a transcript -- an assistant `tool_call`
/// with no matching result -- is already a 400-class malformation on its own, so
/// this heuristic does not make a recoverable session worse.
fn repair_empty_tool_call_ids(messages: &mut [ChatMessage]) {
    let mut pending: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut counter = 0usize;
    for m in messages.iter_mut() {
        if let Some(calls) = m.tool_calls.as_mut() {
            for call in calls.iter_mut() {
                if call.id.is_empty() {
                    let id = format!("call_repair_{counter}");
                    counter += 1;
                    call.id = id.clone();
                    pending.push_back(id);
                }
            }
        } else if m.role == "tool" {
            if let Some(tcid) = m.tool_call_id.as_mut() {
                if tcid.is_empty() {
                    if let Some(id) = pending.pop_front() {
                        *tcid = id;
                    }
                }
            }
        }
    }
}

/// Drop replayed reasoning from all but the most-recent `keep` assistant
/// tool-call turns (#375 follow-up; see [`REASONING_REPLAY_KEEP`]). Walks the
/// wire messages newest-first; once `keep` reasoning-bearing tool-call turns have
/// been seen, every older one has its `reasoning` cleared so it is omitted from
/// the wire. Only assistant turns that actually carry tool calls are counted --
/// those are the only ones a reasoning gateway replays. Plain assistant answers
/// are untouched (the dialect never replays their reasoning anyway).
fn cap_reasoning_replay(messages: &mut [ChatMessage], keep: usize) {
    let mut seen = 0usize;
    for m in messages.iter_mut().rev() {
        let is_tool_call_turn = m.role == "assistant" && m.tool_calls.is_some();
        if !is_tool_call_turn || m.reasoning.is_none() {
            continue;
        }
        if seen < keep {
            seen += 1;
        } else {
            m.reasoning = None;
        }
    }
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

/// Whether a given loop iteration should request model reasoning (#D1, widened
/// in #549). Driven by [`ReasoningVisibility`]:
/// - [`WrapUp`](ReasoningVisibility::WrapUp): the **first** iteration (initial
///   planning, before any tool result) and the **cap-forced wrap-up** step
///   (`remaining <= WRAP_UP_AT_REMAINING`) only. Mid-loop tool-dispatch steps
///   skip reasoning — pure latency on a slow model (#449).
/// - [`All`](ReasoningVisibility::All): every step, so a turn that finishes
///   naturally *before* the cap (the common case) carries reasoning on its final
///   answer. We can't target only the synthesis step a-priori (the model decides
///   by whether it returns tool calls), so `All` requests on every step; the
///   persisted reasoning still ends up being the final step's, since the turn
///   shares one assistant message id and each step overwrites the row.
///
/// `iter` is 0-based; `remaining` counts iterations left including the current
/// one (so `remaining == 1` is the last step).
fn should_reason(iter: usize, remaining: usize, visibility: ReasoningVisibility) -> bool {
    match visibility {
        ReasoningVisibility::WrapUp => iter == 0 || remaining <= WRAP_UP_AT_REMAINING,
        ReasoningVisibility::All => true,
    }
}

/// Cheap content hash for the per-turn read-dedupe (#458 RC5). Uses the same
/// `DefaultHasher` idiom as `compaction_extractive`, so no new dependency: this is
/// a same-process collision check ("did this exact content already appear this
/// turn"), not a security or cross-run digest.
fn content_hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Runs one assistant turn for `session_id`, executing any tool calls the model
/// requests until it produces a plain text answer (or the iteration cap is hit).
/// `on_event` is called synchronously as the turn progresses. The final assistant
/// message is persisted and returned.
///
/// When `enable_reasoning` is true, provider reasoning streams are requested on
/// the steps selected by `reasoning_visibility` and emitted as
/// [`AgentEvent::Reasoning`]; the final step's reasoning is persisted via
/// `set_message_reasoning` (not in message content).
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
    // Which loop steps request reasoning when `enable_reasoning` is true (#549).
    reasoning_visibility: ReasoningVisibility,
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
    // Size the compaction budget to THIS model's real context window (#B1) so a
    // large-window model isn't force-compacted at a small fixed ceiling. Built
    // once per turn and reused for every pressure check below.
    let estimator = ProxyTokenEstimator {
        budget_tokens: ((provider.context_window(model) as f64) * CONTEXT_BUDGET_SAFETY) as u64,
    };
    let mut turn_count: u32 = 0;
    // Repeated-call / no-progress guard (#244 R2): count identical `(tool, arguments)`
    // calls across the turn; `repeat_nudge` carries a tool name to warn about on the
    // next request; `stop_reason` ends the turn with a clear notice when a stall
    // persists past the nudge.
    let mut call_counts: HashMap<(String, String), usize> = HashMap::new();
    let mut repeat_nudge: Option<String> = None;
    let mut stop_reason: Option<String> = None;
    // Per-turn semantic read-dedupe (#458 RC5): read key (e.g. a file path) -> the
    // step it was first read at + a hash of that content. A later re-read whose
    // content is unchanged is collapsed to a sentinel instead of re-injecting the
    // bytes. Complements the byte-identical repeat-breaker above, which only catches
    // identical `(tool, args)` calls -- this fires on identical *content* regardless
    // of how the read was phrased (e.g. a different line range).
    let mut read_cache: HashMap<String, (u32, u64)> = HashMap::new();
    // Context-pressure flush bookkeeping (#244 R5): the transcript length at the last
    // flush, so we re-flush on growth rather than every iteration. `None` = never
    // flushed this turn.
    let mut last_flush_count: Option<u64> = None;
    // Tier-2 abstractive summary cache (RFC 0016 M7.0): the summary covering the
    // cold prefix and the boundary it covers, plus the transcript length when it
    // was produced. Reused across iterations until the transcript grows by the
    // re-flush interval, so a long turn pays for at most one summarizer call per
    // window instead of one per tool round.
    let mut last_summary: Option<(usize, Message)> = None;
    let mut last_summary_count: Option<u64> = None;
    // F1b (#441) telemetry: the projected prefill estimate of each round-trip's
    // outgoing wire, plus how often each compaction tier engaged this turn. Folded
    // into the `Done` event so the desktop's `turn:stats` can report them. Purely
    // observational -- never gates behavior.
    let mut prefill_estimates: Vec<u32> = Vec::new();
    let mut tier1_fires: u32 = 0;
    let mut tier2_fires: u32 = 0;
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
        let pressure = estimator.assess(&history, model);
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
        // Cold-prefix extractive compaction (RFC 0016 M7.1b): once over the budget
        // fraction, compact the cold prefix of the transcript into a reversible,
        // marker-tagged form *for this request only*. The store keeps the full
        // verbatim transcript -- this is a deterministic pre-send wire transform,
        // never a mutation of session state. Recent messages stay byte-identical;
        // any blob that shrank has its verbatim original persisted so the
        // `compaction_retrieve` tool can fetch it back. Messages already compacted
        // at ingest (M7.1a tool results) are skipped to avoid double-compaction.
        let wire = if pressure.is_over(EXTRACTIVE_COMPACT_AT_FRACTION) {
            tier1_fires += 1;
            let cold =
                ExtractiveCompactor::default().compact_cold_collect(&history, KEEP_RECENT_VERBATIM);
            for (mid, key, original) in &cold.originals {
                store.put_compaction_original(session_id, mid, key, original);
            }
            cold.messages
        } else {
            history.clone()
        };

        // Tier-2 abstractive cold-tail summary (RFC 0016 M7.0): the fallback when
        // the mechanical, free Tier-1 pass above cannot relieve enough pressure.
        // Re-estimate pressure on the post-extractive wire and, once over the
        // (higher) Tier-2 fraction, condense the cold prefix into a single summary
        // message via the session LLM (or a configured override), keeping the
        // recent tail verbatim. Request-only, like Tier 1: the store keeps the full
        // transcript and the collapsed block's original is persisted so
        // `compaction_retrieve` can fetch it back. Best-effort -- a failed or
        // cancelled summary falls back to `wire` and never aborts the user's turn.
        //
        // Tier 1 (above) assesses raw `history`; Tier 2 must gate on the projected
        // request size *after* extractive compression, i.e. the post-Tier-1 `wire`.
        // It reuses the same model-aware `estimator` built at the top of the turn
        // (#B1 / RFC 0016 6), so a large-window model is no longer force-summarized
        // at the old fixed 24k ceiling -- only when `wire` genuinely nears its real
        // window. The differing input (`wire`, not `history`) is the intended
        // distinction between the two tiers, not the budget.
        let wire = if tools.abstractive.enabled
            && estimator
                .assess(&wire, model)
                .is_over(tools.abstractive.fire_at_fraction)
        {
            tier2_fires += 1;
            let reuse = match last_summary.as_ref() {
                Some((boundary, msg))
                    if *boundary <= wire.len()
                        && !summary_due(
                            message_count,
                            last_summary_count,
                            DEFAULT_REFLUSH_INTERVAL_MESSAGES,
                        ) =>
                {
                    // Reuse the cached summary: prepend it and keep everything after
                    // its fixed boundary verbatim, so messages appended since the
                    // summary was produced are preserved exactly.
                    let mut out = Vec::with_capacity(wire.len() - boundary + 1);
                    out.push(msg.clone());
                    out.extend_from_slice(&wire[*boundary..]);
                    Some(out)
                }
                _ => None,
            };
            match reuse {
                Some(out) => out,
                None => {
                    match AbstractiveSummarizer::new(tools.abstractive.clone())
                        .summarize_cold(provider, model, &wire, KEEP_RECENT_VERBATIM, &cancel)
                        .await
                    {
                        Ok(Some(result)) => {
                            if let Some((mid, key, original)) = &result.original {
                                store.put_compaction_original(session_id, mid, key, original);
                            }
                            last_summary = Some((result.boundary, result.messages[0].clone()));
                            last_summary_count = Some(message_count);
                            result.messages
                        }
                        _ => wire,
                    }
                }
            }
        } else {
            wire
        };
        messages.extend(to_chat(&wire));
        // F1b (#441): record the projected prefill of the actual outgoing wire
        // (post Tier-1/Tier-2), so the metric reflects what compaction left to send.
        prefill_estimates.push(
            u32::try_from(estimator.assess(&wire, model).estimated_tokens).unwrap_or(u32::MAX),
        );

        // Near the iteration cap, nudge the model toward a final answer so a long
        // turn ends with a real reply instead of "[stopped: reached tool-call
        // limit]" cut mid-tool (#244 R3). The copy graduates across the window: a
        // soft convergence nudge while tools are still advertised, hardening to
        // "do not call tools" only on the final iteration -- where the tool schema
        // is also withheld below, so the instruction matches the mechanism rather
        // than crying "final step" with steps to spare. Transient: request-only.
        let remaining = max_iter - iter; // iterations left, including this one

        // Adaptive per-step reasoning (#D1): with the master switch on, reason only
        // on the planning step (first iteration) and the cap-forced wrap-up step;
        // the mid-loop tool-dispatch steps skip reasoning, which is pure latency on
        // a slow model. A turn that finishes naturally before the cap therefore
        // answers with reasoning off (see `should_reason` for why the synthesis step
        // can't be targeted a priori). Effort, where reasoning IS on, stays whatever
        // the connection set (Medium by default); this gates only *whether* a step
        // reasons.
        let step_thinking =
            enable_reasoning && should_reason(iter, remaining, reasoning_visibility);
        if remaining <= WRAP_UP_AT_REMAINING && max_iter > 1 {
            let content = if remaining <= 1 {
                // Final iteration: tools are withheld below, so the model must
                // answer now -- this hard copy is both true and matched by the
                // mechanism.
                "This is your final step before the tool-call limit. Do not call any \
                 more tools; summarize what you have done and give your final answer \
                 to the user now."
                    .to_string()
            } else {
                // Earlier in the wrap-up window: tools are still advertised, so
                // nudge toward convergence without falsely claiming this is the
                // last step (`remaining` is >= 2 here, so "steps" is always plural).
                format!(
                    "You're approaching the tool-call limit -- about {remaining} steps \
                     left. Start converging: finish any essential tool calls now and \
                     prepare to give your final answer soon."
                )
            };
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: Some(content),
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

        // Graceful-degradation notice (#338): when the active model can't see images,
        // the per-provider capability strip silently drops attachments before they
        // reach the wire. Surface that once per turn so the drop isn't invisible.
        // Count the attachments on the turn's triggering user message (the last user
        // message in history); gating on the first iteration avoids re-notifying on
        // each tool-loop iteration, and counting only the last user message avoids
        // re-firing on later turns whose own input carries nothing.
        if iter == 0 && !provider.supports_vision() {
            let dropped = history
                .iter()
                .rev()
                .find(|m| m.role == Role::User)
                .and_then(|m| m.attachments.as_ref())
                .map_or(0, |a| a.len());
            if let Ok(count) = u32::try_from(dropped) {
                if count > 0 {
                    on_event(AgentEvent::AttachmentsDropped {
                        message_id: message_id.clone(),
                        count,
                    });
                }
            }
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
        // The provider stopped on its output-token cap before the tool-call JSON
        // finished (#528). Tracked across the retry loop so the post-stream parse
        // failure can report truncation instead of a misleading "invalid JSON".
        let mut output_truncated = false;
        let mut attempt = 0usize;
        loop {
            attempt += 1;
            acc.clear();
            reasoning_acc.clear();
            calls.clear();
            let mut emitted_any = false;

            // On the very last iteration, withhold tools entirely so the model
            // emits a final answer instead of another tool call that would be cut
            // off as "[stopped: reached tool-call limit]" (RC3, #454). The widened
            // wrap-up nudge above gives advance warning; this is the hard stop.
            let withhold_tools = max_iter > 1 && remaining <= 1;
            let req = ChatRequest {
                model: model.to_string(),
                messages: messages.clone(),
                tools: if withhold_tools {
                    Vec::new()
                } else {
                    tool_schemas.clone()
                },
                thinking: step_thinking,
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
                        // OR-accumulate (not last-write): once a chunk reports the
                        // output cap was hit, a later non-truncated chunk -- e.g. a
                        // provider's trailing terminal frame -- must not silently reset
                        // it and re-mislabel the cut-off tool call as invalid JSON (#528).
                        output_truncated |= chunk.truncated;
                        if step_thinking && !chunk.reasoning_delta.is_empty() {
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
                            // Some gateways (SiliconFlow GLM-5.2, #374) stream the name in
                            // the first tool_call fragment and then send name: "" (an empty
                            // string, not null) on every continuation fragment. A blind
                            // overwrite would clobber the real name to "", which later
                            // dispatches as `unknown tool:`. Only adopt a non-empty name.
                            if let Some(name) = frag.name {
                                if !name.is_empty() {
                                    buf.name = name;
                                }
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
                estimator
                    .assess(&store.get_messages(session_id), model)
                    .estimated_tokens as u32,
            );
            on_event(AgentEvent::Done {
                message_id: message_id.clone(),
                final_message: Some(final_text),
                turns: Some(turn_count),
                token_count,
                prefill_estimates: Some(prefill_estimates.clone()),
                tier1_fires: Some(tier1_fires),
                tier2_fires: Some(tier2_fires),
            });
            return Ok(finalized);
        }

        // Some gateways (SiliconFlow streaming, #512) never emit a tool_call id in
        // the delta, leaving the accumulated buffer id empty. Persisting "" makes
        // the assistant tool_call and its tool result both carry an empty id, which
        // the gateway then rejects with a 400 when the turn is replayed. Mint a
        // stable per-index id so the request side and the result side stay matched.
        for (index, buf) in calls.iter_mut() {
            if buf.id.is_empty() {
                buf.id = format!("call_{index}");
            }
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
        // Partition this turn's calls (#A1). A call is **parallel-eligible** only
        // when it is side-effect-free: valid JSON args, a permitted, non-interactive,
        // non-subagent tool whose `safety` is `ReadOnly`. Such calls touch only the
        // workspace (reads) and borrow nothing mutable, so running them concurrently
        // is safe and collapses N sequential provider-driven round-trips into one --
        // the single biggest, most model-agnostic latency win. Everything else
        // (writes/dangerous behind the approval gate, interactive `ask_user`,
        // sub-agents, hidden/unpermitted tools, unparseable args, nameless calls)
        // stays on the **serial** path with its exact prior semantics.
        let mut parallel: Vec<(&CallBuf, serde_json::Value)> = Vec::new();
        let mut serial: Vec<&CallBuf> = Vec::new();
        for call in calls.values() {
            let parsed = serde_json::from_str::<serde_json::Value>(&call.arguments);
            let eligible = match &parsed {
                Ok(args) => {
                    !call.name.trim().is_empty()
                        && advertised
                            .as_ref()
                            .is_none_or(|set| set.contains(&call.name))
                        && !tools.registry.is_interactive(&call.name)
                        && !ff_tools::is_subagent(&call.name)
                        && tools.registry.safety(&call.name, args) == Safety::ReadOnly
                }
                Err(_) => false,
            };
            match parsed {
                Ok(args) if eligible => parallel.push((call, args)),
                _ => serial.push(call),
            }
        }

        // Outcomes keyed by call id; filled by the parallel batch and the serial
        // pass, then drained in original call order for persistence + events so the
        // transcript and the frontend see a stable ordering regardless of which path
        // produced each result.
        let mut outcomes: HashMap<String, ff_tools::ToolOutcome> = HashMap::new();

        // Read-only batch: announce each (in call order), then run all concurrently.
        // Skipped wholesale on cancel; unrun calls fall to the backfill guard.
        if !cancel.is_cancelled() && !parallel.is_empty() {
            for (call, args) in &parallel {
                on_event(AgentEvent::ToolCallStarted {
                    message_id: message_id.clone(),
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    args: args.clone(),
                });
            }
            let futs = parallel.iter().map(|(call, args)| {
                let name = call.name.clone();
                let args = args.clone();
                async move {
                    (
                        call.id.clone(),
                        tools.registry.run(&name, args, tools.root).await,
                    )
                }
            });
            for (id, outcome) in futures_util::future::join_all(futs).await {
                outcomes.insert(id, outcome);
            }
        }

        // Serial pass: order-preserving, sequential, awaiting each as before.
        for call in &serial {
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
            let outcome = if call.name.trim().is_empty() {
                // Defense-in-depth (#374): a model that never sends a tool name at all
                // would otherwise dispatch "" and surface a cryptic `unknown tool:`. The
                // empty-string-continuation quirk (GLM-5.2) is fixed at accumulation; this
                // catches a genuinely nameless call with an actionable message instead.
                ff_tools::ToolOutcome::error(
                    "the model returned a tool call with no name -- it likely does not \
                     support OpenAI-compatible tool-calling in FlowForge. Try a standard \
                     tool-caller (Bedrock Claude, or deepseek-ai/DeepSeek-V4-Pro on \
                     SiliconFlow).",
                )
            } else {
                match serde_json::from_str::<serde_json::Value>(&call.arguments) {
                    Err(e) if output_truncated => ff_tools::ToolOutcome::error(format!(
                        "tool `{}` arguments were truncated -- the response hit the output \
                     token limit before the JSON finished ({e}); received: `{}`. Extended \
                     thinking likely consumed the output budget. Retry with a smaller \
                     payload: create the file with a short `write`, then append the rest \
                     in chunks with `bash`.",
                        call.name, call.arguments
                    )),
                    Err(e) => ff_tools::ToolOutcome::error(format!(
                        "tool `{}` arguments were not valid JSON ({e}); received: `{}`. \
                     Re-issue the call with a valid JSON object matching the tool schema.",
                        call.name, call.arguments
                    )),
                    Ok(args) => {
                        if tools.registry.is_interactive(&call.name) {
                            match tools.approve.ask(&message_id, &call.id, &args).await {
                                Some(answer) => ff_tools::ToolOutcome::ok(answer),
                                None => {
                                    ff_tools::ToolOutcome::error("[no answer: question dismissed]")
                                }
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
                                session_id,
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
                                tools
                                    .registry
                                    .run_with_session(&call.name, args, tools.root, session_id)
                                    .await
                            } else {
                                ff_tools::ToolOutcome::error(format!(
                                    "call to `{}` was not approved",
                                    call.name
                                ))
                            }
                        }
                    }
                }
            };
            outcomes.insert(call.id.clone(), outcome);
        }

        // Persist results, emit Finished, and run repeat-stall accounting in
        // original call order, regardless of which path produced each outcome. A
        // call with no outcome (cancelled before it ran) is left to the backfill
        // guard, which writes a `[cancelled]` result on drop (#316).
        for call in calls.values() {
            let Some(mut outcome) = outcomes.remove(&call.id) else {
                continue;
            };
            // Semantic read-dedupe (#458 RC5): if this is a content read (e.g. `view`)
            // whose content is byte-identical to an earlier read of the same target
            // this turn, replace the payload with a small staleness sentinel instead
            // of re-injecting the bytes. A changed file (different hash, e.g. after an
            // `edit`) is not deduped -- the full content flows through. Only successful
            // reads participate; an error result is never cached.
            if outcome.success {
                let args = serde_json::from_str::<serde_json::Value>(&call.arguments)
                    .unwrap_or(serde_json::Value::Null);
                if let Some(key) = tools.registry.dedupe_key(&call.name, &args) {
                    let hash = content_hash(&outcome.content);
                    match read_cache.get(&key) {
                        Some(&(step, prev)) if prev == hash => {
                            outcome.content = format!(
                                "[unchanged since step {step} -- identical content, not re-sent]"
                            );
                        }
                        _ => {
                            read_cache.insert(key, (turn_count, hash));
                        }
                    }
                }
            }
            // Keep a tool result verbatim on the turn it is produced (RC1, #453):
            // the model must read the full content on its first look, or it is
            // forced into a `compaction_retrieve` round-trip / re-read loop. Only a
            // result that exceeds the hard per-result byte cap is reversibly
            // compacted at ingest -- so an oversized payload stays retrievable
            // rather than hard-truncated -- while everything within the cap is
            // stored byte-for-byte. Whole-transcript pressure is still relieved by
            // cold-tail compaction (`compact_cold_collect` + `KEEP_RECENT_VERBATIM`)
            // once a result ages out of the hot window; `truncate_tool_result`
            // remains the last-resort backstop for an oversized payload that does
            // not compress below the cap.
            // `compaction_retrieve` returns a verbatim original the model explicitly
            // asked to un-compact; re-compacting it here would re-emit the same
            // elision and the same deterministic key -- a no-op loop that makes
            // retrieve useless for any original above the cap (RC6, #476). Pass it
            // through verbatim; it ages out normally via cold-tail compaction.
            let (stored, original) = if outcome.content.len() > TOOL_RESULT_MAX_BYTES
                && call.name != COMPACTION_RETRIEVE_TOOL
            {
                let compacted = compaction_extractive::ExtractiveCompactor::default()
                    .compress_one(&outcome.content);
                (truncate_tool_result(&compacted.text), compacted.original)
            } else {
                (outcome.content.clone(), None)
            };
            let result_msg = store.add_tool_result_message(session_id, call.id.clone(), stored);
            if let Some((key, original)) = original {
                store.put_compaction_original(session_id, &result_msg.id, &key, &original);
            }
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
        estimator
            .assess(&store.get_messages(session_id), model)
            .estimated_tokens as u32,
    );
    on_event(AgentEvent::Done {
        message_id: msg.id.clone(),
        final_message: Some(msg.content.clone()),
        turns: Some(turn_count),
        token_count,
        prefill_estimates: Some(prefill_estimates),
        tier1_fires: Some(tier1_fires),
        tier2_fires: Some(tier2_fires),
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
    parent_session_id: &str,
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
        abstractive: parent.abstractive.clone(),
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
        ReasoningVisibility::WrapUp,
        cancel,
        |_event| {},
    ))
    .await;

    // Re-home any compaction originals the child summary still points at, so they
    // survive the child-session teardown below (#469). A `[compacted; retrieve
    // key=...]` marker that crosses the delegation boundary would otherwise dangle:
    // `compaction_originals` cascades on session delete, so the row would vanish and
    // `compaction_retrieve` would have nothing to return -- forcing the parent to
    // re-delegate. Re-homing keeps the marker compact in the parent transcript while
    // the verbatim original stays retrievable on demand from the parent session.
    let outcome = match result {
        Ok(msg) if !msg.content.trim().is_empty() => {
            for key in marker_keys(&msg.content) {
                store.rehome_compaction_original(key, parent_session_id);
            }
            ff_tools::ToolOutcome::ok(msg.content)
        }
        Ok(_) => ff_tools::ToolOutcome::ok("[sub-agent finished without a summary]"),
        Err(e) => ff_tools::ToolOutcome::error(format!("sub-agent failed: {e}")),
    };

    store.delete_session(&child.id);
    outcome
}

/// Extract every `[compacted; retrieve key=<HEX>]` marker key in `content`, in
/// order. Used to re-home a sub-agent's surviving compaction originals to the
/// parent session before the child session is torn down (#469). A marker missing
/// its closing `]` is skipped.
fn marker_keys(content: &str) -> Vec<&str> {
    let mut keys = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find(COMPACTION_MARKER_PREFIX) {
        let after = &rest[start + COMPACTION_MARKER_PREFIX.len()..];
        let Some(end) = after.find(']') else {
            break;
        };
        let key = &after[..end];
        if !key.is_empty() {
            keys.push(key);
        }
        rest = &after[end + 1..];
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ff_llm::{Chunk, ChunkStream, LlmError, ToolCallDelta};
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    #[test]
    fn cancel_token_ptr_eq_distinguishes_clone_from_fresh() {
        let token = CancelToken::new();
        let clone = token.clone();
        assert!(token.ptr_eq(&clone), "a clone shares the underlying flag");
        assert!(clone.ptr_eq(&token), "ptr_eq is symmetric");
        let other = CancelToken::new();
        assert!(
            !token.ptr_eq(&other),
            "two independently-created tokens are distinct"
        );
    }

    #[test]
    fn marker_keys_extracts_one_many_zero_and_skips_unterminated() {
        // #469: re-homing depends on pulling every retrieve key out of a sub-agent
        // summary. Single, multiple-in-order, none, empty-key, and a marker missing
        // its closing `]` must all behave.
        assert_eq!(
            marker_keys("see report\n[compacted; retrieve key=4f441c46bdb87160]"),
            vec!["4f441c46bdb87160"]
        );
        assert_eq!(
            marker_keys("a [compacted; retrieve key=aaa] mid b [compacted; retrieve key=bbb] end"),
            vec!["aaa", "bbb"]
        );
        assert!(marker_keys("a plain summary with no markers").is_empty());
        assert!(marker_keys("[compacted; retrieve key=]").is_empty());
        assert!(marker_keys("dangling [compacted; retrieve key=ccc no bracket").is_empty());
    }

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
    fn to_chat_caps_reasoning_replay_to_last_n_tool_turns() {
        // C1: a transcript with more than REASONING_REPLAY_KEEP reasoning-bearing
        // tool-call turns keeps reasoning only on the most-recent `keep` of them;
        // older CoT is dropped from the wire (the store keeps it verbatim).
        let tool_turn = |id: &str, cot: &str| ff_core::Message {
            id: id.into(),
            session_id: "s1".into(),
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(vec![ff_core::ToolCall {
                id: format!("call_{id}"),
                name: "search".into(),
                arguments: "{}".into(),
            }]),
            tool_call_id: None,
            attachments: None,
            reasoning: Some(cot.into()),
            created_at: 0,
        };
        let history = vec![
            tool_turn("m1", "oldest"),
            tool_turn("m2", "middle"),
            tool_turn("m3", "newest"),
        ];
        assert_eq!(REASONING_REPLAY_KEEP, 2);
        let out = to_chat(&history);
        assert_eq!(out[0].reasoning, None, "oldest CoT dropped from wire");
        assert_eq!(out[1].reasoning.as_deref(), Some("middle"));
        assert_eq!(out[2].reasoning.as_deref(), Some("newest"));
    }

    #[test]
    fn should_reason_wrapup_only_on_planning_and_wrapup_steps() {
        use ReasoningVisibility::{All, WrapUp};
        // WrapUp (#449): reason on the first iteration and the wrap-up step; skip mid-loop.
        let max_iter = 25usize;
        assert!(should_reason(0, max_iter, WrapUp)); // planning
        assert!(!should_reason(1, max_iter - 1, WrapUp)); // mid-loop
        assert!(!should_reason(10, max_iter - 10, WrapUp));
        assert!(should_reason(max_iter - 1, WRAP_UP_AT_REMAINING, WrapUp)); // wrap-up
                                                                            // All (#549): every step reasons, including the natural mid/final ones.
        assert!(should_reason(0, max_iter, All));
        assert!(should_reason(1, max_iter - 1, All));
        assert!(should_reason(10, max_iter - 10, All));
        assert!(should_reason(max_iter - 1, WRAP_UP_AT_REMAINING, All));
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
            abstractive: AbstractiveConfig::default(),
        };

        run_turn(
            &provider,
            &store,
            &plan,
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
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

    /// A `Safety::ReadOnly` tool that sleeps before returning, so two concurrent
    /// invocations finish in ~one sleep's wall-clock rather than two -- letting the
    /// #A1 parallel-execution test prove concurrency by timing.
    struct SlowRead;
    #[async_trait]
    impl ff_tools::Tool for SlowRead {
        fn name(&self) -> &str {
            "slow_read"
        }
        fn description(&self) -> &str {
            "test-only read tool that sleeps 150ms"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type":"object","properties":{"k":{"type":"string"}}})
        }
        fn safety(&self, _args: &serde_json::Value) -> Safety {
            Safety::ReadOnly
        }
        fn max_safety(&self) -> Safety {
            Safety::ReadOnly
        }
        async fn run(&self, _args: serde_json::Value, _root: &Path) -> ff_tools::ToolOutcome {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            ff_tools::ToolOutcome::ok("read done")
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

    /// First call streams a tool call the way SiliconFlow GLM-5.2 does (#374): the
    /// name arrives only in the first fragment, then every continuation fragment
    /// carries `name: Some("")` (an empty string, not `None`) alongside the argument
    /// pieces. A blind overwrite would clobber the name to "" -> `unknown tool:`.
    struct GlmFragmentedToolCall {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for GlmFragmentedToolCall {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![
                    Ok(Chunk {
                        tool_calls: vec![ToolCallDelta {
                            index: 0,
                            id: Some("call_1".into()),
                            name: Some("bash".into()),
                            arguments: String::new(),
                        }],
                        ..Chunk::default()
                    }),
                    Ok(Chunk {
                        tool_calls: vec![ToolCallDelta {
                            index: 0,
                            id: None,
                            name: Some(String::new()),
                            arguments: r#"{"command":"#.into(),
                        }],
                        ..Chunk::default()
                    }),
                    Ok(Chunk {
                        tool_calls: vec![ToolCallDelta {
                            index: 0,
                            id: None,
                            name: Some(String::new()),
                            arguments: r#""echo wired"}"#.into(),
                        }],
                        done: true,
                        ..Chunk::default()
                    }),
                ]
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

    /// First call streams a tool call whose name never arrives (the fragment carries
    /// `name: None`), the way a model with no real OpenAI-compatible tool-calling
    /// would (#374); the second call returns plain text so the turn can resume after
    /// the actionable error result. Must fail with that message, not `unknown tool:`.
    struct NamelessToolCall {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for NamelessToolCall {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("call_x".into()),
                        name: None,
                        arguments: r#"{"command":"ls"}"#.into(),
                    }],
                    done: true,
                    ..Chunk::default()
                })]
            } else {
                vec![Ok(Chunk {
                    delta: "ok, switching approach".into(),
                    done: true,
                    ..Chunk::default()
                })]
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    /// First call streams a tool call the way SiliconFlow does (#512): the delta
    /// never carries an `id` (every fragment has `id: None`), so the accumulated
    /// buffer id stays empty. The capture site must mint a stable id so the
    /// persisted assistant tool_call and its tool result are not bound to "".
    struct IdlessToolCall {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for IdlessToolCall {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: None,
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
            ReasoningVisibility::All,
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

    /// A text provider that reports no vision support, for the #338 degrade notice.
    struct NoVisionText;

    #[async_trait]
    impl Provider for NoVisionText {
        fn supports_vision(&self) -> bool {
            false
        }

        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            Ok(futures_util::stream::iter(vec![Ok(Chunk {
                delta: "ok".into(),
                done: true,
                ..Chunk::default()
            })])
            .boxed())
        }
    }

    fn one_image() -> Vec<ff_core::Attachment> {
        vec![ff_core::Attachment {
            kind: ff_core::AttachmentKind::Image,
            media_type: "image/png".into(),
            source: ff_core::AttachmentSource::Inline("aGk=".into()),
            name: None,
            bytes: 2,
        }]
    }

    #[tokio::test]
    async fn no_vision_model_emits_one_attachments_dropped_notice() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message_with_attachments(&s.id, Role::User, "look at this".into(), one_image());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;

        let mut dropped: Vec<u32> = Vec::new();
        run_turn(
            &NoVisionText,
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::AttachmentsDropped { count, .. } = ev {
                    dropped.push(count);
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(
            dropped,
            vec![1],
            "a non-vision model emits exactly one notice carrying the dropped count"
        );
    }

    #[tokio::test]
    async fn vision_model_does_not_emit_attachments_dropped() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message_with_attachments(&s.id, Role::User, "look at this".into(), one_image());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;

        let mut emitted = false;
        run_turn(
            &TextProvider,
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |ev| {
                if matches!(ev, AgentEvent::AttachmentsDropped { .. }) {
                    emitted = true;
                }
            },
        )
        .await
        .unwrap();

        assert!(
            !emitted,
            "a vision-capable model keeps attachments, so no drop notice fires"
        );
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
            ReasoningVisibility::All,
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

    /// Step 0 returns a tool call (no reasoning emitted there); step 1 — the
    /// *natural* final-answer step, well before any cap — emits reasoning then
    /// text. Models the #549 gap: a turn that finishes naturally must still show
    /// and persist a Thought block for its answer.
    struct ToolThenReasonedText {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for ToolThenReasonedText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("call_1".into()),
                        name: Some("bash".into()),
                        arguments: r#"{"command":"echo hi"}"#.into(),
                    }],
                    done: true,
                    ..Chunk::default()
                })]
            } else {
                vec![
                    Ok(Chunk {
                        reasoning_delta: "the output ".into(),
                        ..Chunk::default()
                    }),
                    Ok(Chunk {
                        reasoning_delta: "says hi".into(),
                        ..Chunk::default()
                    }),
                    Ok(Chunk {
                        delta: "It printed hi.".into(),
                        done: true,
                        ..Chunk::default()
                    }),
                ]
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    #[tokio::test]
    async fn all_visibility_persists_reasoning_on_natural_final_answer() {
        // #549: with All, the natural synthesis step (step 1, not a cap wrap-up)
        // carries reasoning, and it is persisted on the assistant message.
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "run echo".into());
        let registry = ToolRegistry::with_defaults();
        let approve = AlwaysApprove;
        let provider = ToolThenReasonedText {
            calls: AtomicUsize::new(0),
        };

        let msg = run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            true,
            ReasoningVisibility::All,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        assert_eq!(msg.content, "It printed hi.");
        assert_eq!(msg.reasoning.as_deref(), Some("the output says hi"));
    }

    #[tokio::test]
    async fn wrapup_visibility_skips_reasoning_on_natural_final_answer() {
        // The contrast: under WrapUp the same step-1 synthesis runs with reasoning
        // OFF (it is neither the planning step nor a cap wrap-up), so nothing is
        // persisted — the #449 latency optimization, now opt-in.
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "run echo".into());
        let registry = ToolRegistry::with_defaults();
        let approve = AlwaysApprove;
        let provider = ToolThenReasonedText {
            calls: AtomicUsize::new(0),
        };

        let msg = run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            true,
            ReasoningVisibility::WrapUp,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        assert_eq!(msg.content, "It printed hi.");
        assert_eq!(msg.reasoning, None);
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
            ReasoningVisibility::All,
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
            ReasoningVisibility::All,
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
                AgentEvent::AttachmentsDropped { .. } => {}
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

    #[tokio::test]
    async fn idless_tool_call_gets_synthesized_id_matched_to_its_result() {
        // #512: SiliconFlow streams tool calls without an id. The capture site must
        // mint a stable id so the persisted assistant tool_call and its tool result
        // share a non-empty id; an empty id is what the gateway later rejects (400).
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "run echo".into());
        let registry = ToolRegistry::with_defaults();
        let approve = AlwaysApprove;
        let provider = IdlessToolCall {
            calls: AtomicUsize::new(0),
        };
        run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let history = store.get_messages(&s.id);
        let call_id = history[1].tool_calls.as_ref().unwrap()[0].id.clone();
        assert!(
            !call_id.is_empty(),
            "assistant tool_call id must be synthesized"
        );
        assert_eq!(
            history[2].tool_call_id.as_deref(),
            Some(call_id.as_str()),
            "tool result must bind to the same synthesized id"
        );
    }

    #[test]
    fn repair_binds_persisted_empty_ids_in_fifo_order() {
        // #512 salvage path: a session recorded before the capture-site fix has
        // empty ids on both the assistant tool_call and its tool result. to_chat
        // must mint matching non-empty ids so the replayed turn is accepted.
        let assistant = ff_core::Message {
            id: "m1".into(),
            session_id: "s1".into(),
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(vec![ff_core::ToolCall {
                id: String::new(),
                name: "bash".into(),
                arguments: "{}".into(),
            }]),
            tool_call_id: None,
            attachments: None,
            reasoning: None,
            created_at: 0,
        };
        let tool = ff_core::Message {
            id: "m2".into(),
            session_id: "s1".into(),
            role: Role::Tool,
            content: "ok".into(),
            tool_calls: None,
            tool_call_id: Some(String::new()),
            attachments: None,
            reasoning: None,
            created_at: 1,
        };
        let out = to_chat(&[assistant, tool]);
        let call_id = out[0].tool_calls.as_ref().unwrap()[0].id.clone();
        assert!(!call_id.is_empty(), "assistant id must be repaired");
        assert_eq!(
            out[1].tool_call_id.as_deref(),
            Some(call_id.as_str()),
            "tool result must be bound to the repaired id"
        );
    }

    #[test]
    fn repair_binds_multiple_empty_ids_in_one_message_in_fifo_order() {
        // Depth >1: two id-less calls in a single assistant message, then their two
        // results. The minted ids must be distinct and each result must bind to its
        // call in order -- locks the VecDeque FIFO contract beyond the single-call path.
        let assistant = ff_core::Message {
            id: "m1".into(),
            session_id: "s1".into(),
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(vec![
                ff_core::ToolCall {
                    id: String::new(),
                    name: "bash".into(),
                    arguments: "{}".into(),
                },
                ff_core::ToolCall {
                    id: String::new(),
                    name: "view".into(),
                    arguments: "{}".into(),
                },
            ]),
            tool_call_id: None,
            attachments: None,
            reasoning: None,
            created_at: 0,
        };
        let result = |mid: &str, ts: i64| ff_core::Message {
            id: mid.into(),
            session_id: "s1".into(),
            role: Role::Tool,
            content: "ok".into(),
            tool_calls: None,
            tool_call_id: Some(String::new()),
            attachments: None,
            reasoning: None,
            created_at: ts,
        };
        let out = to_chat(&[assistant, result("m2", 1), result("m3", 2)]);
        let calls = out[0].tool_calls.as_ref().unwrap();
        let (id0, id1) = (calls[0].id.clone(), calls[1].id.clone());
        assert!(!id0.is_empty() && !id1.is_empty());
        assert_ne!(id0, id1, "minted ids must be distinct");
        assert_eq!(out[1].tool_call_id.as_deref(), Some(id0.as_str()));
        assert_eq!(out[2].tool_call_id.as_deref(), Some(id1.as_str()));
    }

    #[test]
    fn repair_leaves_valid_tool_call_ids_untouched() {
        let assistant = ff_core::Message {
            id: "m1".into(),
            session_id: "s1".into(),
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(vec![ff_core::ToolCall {
                id: "call_real".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            }]),
            tool_call_id: None,
            attachments: None,
            reasoning: None,
            created_at: 0,
        };
        let tool = ff_core::Message {
            id: "m2".into(),
            session_id: "s1".into(),
            role: Role::Tool,
            content: "ok".into(),
            tool_calls: None,
            tool_call_id: Some("call_real".into()),
            attachments: None,
            reasoning: None,
            created_at: 1,
        };
        let out = to_chat(&[assistant, tool]);
        assert_eq!(out[0].tool_calls.as_ref().unwrap()[0].id, "call_real");
        assert_eq!(out[1].tool_call_id.as_deref(), Some("call_real"));
    }

    /// #374: GLM-5.2 streams `name: ""` on every continuation fragment. The
    /// accumulator must keep the name from the first fragment and still assemble the
    /// arguments, so the call dispatches to `bash` -- not a clobbered `unknown tool:`.
    #[tokio::test]
    async fn glm_empty_string_name_fragments_do_not_clobber_the_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "run echo".into());
        let registry = ToolRegistry::with_defaults();
        let approve = AlwaysApprove;
        let provider = GlmFragmentedToolCall {
            calls: AtomicUsize::new(0),
        };

        let mut started_name = String::new();
        let mut finished_ok = false;
        let mut result = String::new();
        let mut final_text = String::new();
        let msg = run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |ev| match ev {
                AgentEvent::ToolCallStarted { name, .. } => started_name = name,
                AgentEvent::ToolCallFinished {
                    success, result: r, ..
                } => {
                    finished_ok = success;
                    result = r;
                }
                AgentEvent::Token { delta, .. } => final_text.push_str(&delta),
                AgentEvent::Reasoning { .. } => {}
                AgentEvent::Error { message } => panic!("error: {message}"),
                AgentEvent::Done { .. } => {}
                AgentEvent::MemoryFlushed { .. } => {}
                AgentEvent::AttachmentsDropped { .. } => {}
            },
        )
        .await
        .unwrap();

        assert_eq!(
            started_name, "bash",
            "name must survive the empty-string frags"
        );
        assert!(
            finished_ok,
            "the bash call must run, not fail as unknown tool"
        );
        assert!(
            result.contains("wired"),
            "args must assemble across fragments"
        );
        assert_eq!(msg.content, "done: wired");
        assert!(
            !result.contains("unknown tool"),
            "must not regress to the clobbered-name failure"
        );
    }

    /// #374: a model that never sends a tool name at all must fail with an actionable
    /// message, not the cryptic `unknown tool:` from dispatching an empty name.
    #[tokio::test]
    async fn nameless_tool_call_fails_with_actionable_message() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "do something".into());
        let registry = ToolRegistry::with_defaults();
        let approve = AlwaysApprove;

        let mut finished_ok = true;
        let mut result = String::new();
        let provider = NamelessToolCall {
            calls: AtomicUsize::new(0),
        };
        run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |ev| match ev {
                AgentEvent::ToolCallFinished {
                    success, result: r, ..
                } => {
                    finished_ok = success;
                    result = r;
                }
                AgentEvent::Error { message } => panic!("error: {message}"),
                _ => {}
            },
        )
        .await
        .unwrap();

        assert!(!finished_ok, "a nameless call is a failed tool result");
        assert!(
            result.contains("no name"),
            "must explain the model returned a tool call with no name, got: {result}"
        );
        assert!(
            !result.contains("unknown tool"),
            "must not surface the cryptic unknown-tool error"
        );
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
            ReasoningVisibility::All,
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
                AgentEvent::AttachmentsDropped { .. } => {}
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
            ReasoningVisibility::All,
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
            ReasoningVisibility::All,
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

    /// #A1: two read-only tool calls in one turn run concurrently (timed), and each
    /// requested call id gets exactly one tool result.
    #[tokio::test]
    async fn parallel_readonly_calls_run_concurrently() {
        struct TwoReadsThenText {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl Provider for TwoReadsThenText {
            async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                let chunks = if n == 0 {
                    vec![Ok(Chunk {
                        tool_calls: vec![
                            ToolCallDelta {
                                index: 0,
                                id: Some("r1".into()),
                                name: Some("slow_read".into()),
                                arguments: r#"{"k":"a"}"#.into(),
                            },
                            ToolCallDelta {
                                index: 1,
                                id: Some("r2".into()),
                                name: Some("slow_read".into()),
                                arguments: r#"{"k":"b"}"#.into(),
                            },
                        ],
                        done: true,
                        ..Chunk::default()
                    })]
                } else {
                    vec![Ok(Chunk {
                        delta: "done reading".into(),
                        done: true,
                        ..Chunk::default()
                    })]
                };
                Ok(futures_util::stream::iter(chunks).boxed())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "read two".into());
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(SlowRead));
        let approve = AlwaysApprove;
        let provider = TwoReadsThenText {
            calls: AtomicUsize::new(0),
        };

        let start = std::time::Instant::now();
        let msg = run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();
        let elapsed = start.elapsed();

        // Two 150ms reads concurrently ~= 150ms; serial would be ~300ms.
        assert!(
            elapsed < std::time::Duration::from_millis(280),
            "read-only calls must run concurrently, took {elapsed:?}"
        );
        // Exactly one tool result per requested id.
        let history = store.get_messages(&s.id);
        let replied: Vec<String> = history
            .iter()
            .filter(|m| m.role == Role::Tool)
            .filter_map(|m| m.tool_call_id.clone())
            .collect();
        assert_eq!(replied.len(), 2, "one result per call: {replied:?}");
        assert!(replied.iter().any(|r| r == "r1"));
        assert!(replied.iter().any(|r| r == "r2"));
        assert_eq!(msg.content, "done reading");
    }

    /// #A1: a turn mixing a read-only call and a write call keeps the write on the
    /// serial, approval-gated path; the read-only call never reaches the approver.
    #[tokio::test]
    async fn mixed_read_and_write_keeps_write_gated() {
        struct ReadAndWriteThenText {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl Provider for ReadAndWriteThenText {
            async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                let chunks = if n == 0 {
                    vec![Ok(Chunk {
                        tool_calls: vec![
                            ToolCallDelta {
                                index: 0,
                                id: Some("r1".into()),
                                name: Some("slow_read".into()),
                                arguments: r#"{"k":"a"}"#.into(),
                            },
                            ToolCallDelta {
                                index: 1,
                                id: Some("w1".into()),
                                name: Some("bash".into()),
                                arguments: r#"{"command":"touch made_by_write"}"#.into(),
                            },
                        ],
                        done: true,
                        ..Chunk::default()
                    })]
                } else {
                    vec![Ok(Chunk {
                        delta: "did both".into(),
                        done: true,
                        ..Chunk::default()
                    })]
                };
                Ok(futures_util::stream::iter(chunks).boxed())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "read and write".into());
        let mut registry = ToolRegistry::with_defaults();
        registry.register(Box::new(SlowRead));
        let consulted = Arc::new(AtomicBool::new(false));
        let approve = RecordingApprover {
            consulted: consulted.clone(),
        };
        let provider = ReadAndWriteThenText {
            calls: AtomicUsize::new(0),
        };

        run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        // The write went through the approval gate; the read-only call did not need it.
        assert!(
            consulted.load(Ordering::SeqCst),
            "the write call must be approval-gated on the serial path"
        );
        // Both calls produced a tool result.
        let history = store.get_messages(&s.id);
        let replied: Vec<String> = history
            .iter()
            .filter(|m| m.role == Role::Tool)
            .filter_map(|m| m.tool_call_id.clone())
            .collect();
        assert!(replied.iter().any(|r| r == "r1"));
        assert!(replied.iter().any(|r| r == "w1"));
        // The approved write actually ran.
        assert!(dir.path().join("made_by_write").exists());
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
            ReasoningVisibility::All,
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
            ReasoningVisibility::All,
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
            ReasoningVisibility::All,
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
            ReasoningVisibility::All,
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
            ReasoningVisibility::All,
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
            abstractive: AbstractiveConfig::default(),
        };

        run_turn(
            &provider,
            &store,
            &at_cap,
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
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
            abstractive: AbstractiveConfig::default(),
        };

        run_turn(
            &provider,
            &store,
            &scoped,
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
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
    /// request, whether a wrap-up nudge was present (`nudge_seen`, either copy),
    /// whether the *hard* "do not call tools" copy was present (`hard_copy_seen`),
    /// and whether the tool schema was withheld (`tools_withheld`). Lets a test
    /// drive the loop to its cap and assert that the soft nudge spans the window
    /// while the hard copy and tool-withholding align only on the final iteration.
    struct RecordingToolLooper {
        nudge_seen: Arc<std::sync::Mutex<Vec<bool>>>,
        hard_copy_seen: Arc<std::sync::Mutex<Vec<bool>>>,
        tools_withheld: Arc<std::sync::Mutex<Vec<bool>>>,
    }

    #[async_trait]
    impl Provider for RecordingToolLooper {
        async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let system_text = |needle: &str| {
                req.messages.iter().any(|m| {
                    m.role == "system" && m.content.as_deref().is_some_and(|c| c.contains(needle))
                })
            };
            // "tool-call limit" appears in both the soft and hard wrap-up copies
            // (and not in the repeat-stall nudge), so it marks the whole window.
            self.nudge_seen
                .lock()
                .unwrap()
                .push(system_text("tool-call limit"));
            self.hard_copy_seen
                .lock()
                .unwrap()
                .push(system_text("Do not call any more tools"));
            self.tools_withheld
                .lock()
                .unwrap()
                .push(req.tools.is_empty());
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
    async fn wrap_up_nudge_graduates_then_hard_stops_and_withholds_tools_on_final_iteration() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "keep going".into());
        let registry = ToolRegistry::with_defaults();
        let approve = AlwaysApprove;
        let nudge_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hard_copy_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let tools_withheld = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = RecordingToolLooper {
            nudge_seen: nudge_seen.clone(),
            hard_copy_seen: hard_copy_seen.clone(),
            tools_withheld: tools_withheld.clone(),
        };
        // Cap 5 with WRAP_UP_AT_REMAINING == 3: remaining counts down 5,4,3,2,1.
        let tools = ToolContext::new(&registry, dir.path(), &approve, 5);

        run_turn(
            &provider,
            &store,
            &tools,
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let seen = nudge_seen.lock().unwrap();
        // The provider is hit once per iteration, up to the cap.
        assert_eq!(seen.len(), 5, "loop should run to the iteration cap");
        // A wrap-up nudge (either copy) fires across the window (remaining <= 3).
        assert_eq!(seen.as_slice(), &[false, false, true, true, true]);
        // But the hard "do not call tools" copy is reserved for the final
        // iteration (remaining == 1) -- the earlier window gets the soft nudge.
        let hard = hard_copy_seen.lock().unwrap();
        assert_eq!(hard.as_slice(), &[false, false, false, false, true]);
        // ...and tool-withholding aligns exactly with the hard copy, so the
        // instruction never tells the model to stop while tools are still offered.
        let withheld = tools_withheld.lock().unwrap();
        assert_eq!(withheld.as_slice(), &[false, false, false, false, true]);
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
            hard_copy_seen: Arc::new(std::sync::Mutex::new(Vec::new())),
            tools_withheld: Arc::new(std::sync::Mutex::new(Vec::new())),
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
            ReasoningVisibility::All,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let seen = nudge_seen.lock().unwrap();
        // With a single-iteration cap there is no "next step" to wrap up toward.
        assert_eq!(seen.as_slice(), &[false]);
    }

    /// Loops on tool calls while tools are advertised, but emits a final text
    /// answer the moment the request carries no tools. Lets a test prove that
    /// withholding tools on the last iteration forces a real answer.
    struct FinalizesWhenToolsWithdrawn;
    #[async_trait]
    impl Provider for FinalizesWhenToolsWithdrawn {
        async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let chunk = if req.tools.is_empty() {
                Chunk {
                    delta: "wrapped up".into(),
                    done: true,
                    ..Chunk::default()
                }
            } else {
                Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("call_1".into()),
                        name: Some("bash".into()),
                        arguments: r#"{"command":"echo loop"}"#.into(),
                    }],
                    done: true,
                    ..Chunk::default()
                }
            };
            Ok(futures_util::stream::iter(vec![Ok(chunk)]).boxed())
        }
    }

    #[tokio::test]
    async fn cap_finalization_produces_answer_not_stopped_notice() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "review this".into());
        let registry = ToolRegistry::with_defaults();
        let approve = AlwaysApprove;
        // A model that never finishes on its own would previously loop to the cap
        // and yield "[stopped: reached tool-call limit]". Withholding tools on the
        // final iteration (RC3, #454) must instead force a real text answer.
        let tools = ToolContext::new(&registry, dir.path(), &approve, 3);

        let final_msg = run_turn(
            &FinalizesWhenToolsWithdrawn,
            &store,
            &tools,
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        assert!(
            final_msg.content.contains("wrapped up"),
            "the turn must end with a real answer, got: {}",
            final_msg.content
        );
        assert!(
            !final_msg
                .content
                .contains("[stopped: reached tool-call limit]"),
            "withholding tools on the final iteration must avoid the dead-end notice"
        );
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
            ReasoningVisibility::All,
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

    /// First turn: a tool call whose JSON args are cut off, on a chunk flagged
    /// `truncated` (output-token cap, #528). Second turn: a real answer.
    struct TruncatedToolArgs {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Provider for TruncatedToolArgs {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("call_cut".into()),
                        name: Some("write".into()),
                        arguments: r#"{"path": "docs/rfc.md"#.into(),
                    }],
                    done: true,
                    truncated: true,
                    ..Chunk::default()
                })]
            } else {
                vec![Ok(Chunk {
                    delta: "done in chunks".into(),
                    done: true,
                    ..Chunk::default()
                })]
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    #[tokio::test]
    async fn truncated_tool_args_report_truncation_not_invalid_json() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "write a long file".into());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;
        let calls = Arc::new(AtomicUsize::new(0));

        let msg = run_turn(
            &TruncatedToolArgs {
                calls: calls.clone(),
            },
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        assert_eq!(msg.content, "done in chunks");
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let history = store.get_messages(&s.id);
        let tool_reply = history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("a tool result must exist for the truncated call");
        assert!(
            tool_reply.content.contains("truncated"),
            "a cap-truncated call should report truncation, got: {}",
            tool_reply.content
        );
        assert!(
            !tool_reply.content.contains("not valid JSON"),
            "truncation must not be mislabeled as invalid JSON (#528), got: {}",
            tool_reply.content
        );
    }

    /// First turn: a truncated chunk (cut tool-call JSON, `done:false`) followed by
    /// a *clean* terminal chunk (`done:true`, `truncated:false`) -- mirroring a
    /// provider that streams a `length`/`MaxTokens` frame and then a separate
    /// terminal frame. The trailing clean chunk must NOT reset the truncation flag
    /// (OR-accumulate, not last-write), or the cut call re-mislabels as invalid
    /// JSON -- the exact #528 regression the `|=` guards against. Second turn: a
    /// real answer.
    struct TruncatedThenCleanTerminal {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Provider for TruncatedThenCleanTerminal {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![
                    Ok(Chunk {
                        tool_calls: vec![ToolCallDelta {
                            index: 0,
                            id: Some("call_cut".into()),
                            name: Some("write".into()),
                            arguments: r#"{"path": "docs/rfc.md"#.into(),
                        }],
                        done: false,
                        truncated: true,
                        ..Chunk::default()
                    }),
                    Ok(Chunk {
                        done: true,
                        truncated: false,
                        ..Chunk::default()
                    }),
                ]
            } else {
                vec![Ok(Chunk {
                    delta: "done in chunks".into(),
                    done: true,
                    ..Chunk::default()
                })]
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    #[tokio::test]
    async fn truncation_survives_a_trailing_clean_terminal_chunk() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "write a long file".into());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;
        let calls = Arc::new(AtomicUsize::new(0));

        let msg = run_turn(
            &TruncatedThenCleanTerminal {
                calls: calls.clone(),
            },
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        assert_eq!(msg.content, "done in chunks");

        let tool_reply = store
            .get_messages(&s.id)
            .into_iter()
            .find(|m| m.role == Role::Tool)
            .expect("a tool result must exist for the truncated call");
        assert!(
            tool_reply.content.contains("truncated"),
            "a trailing clean chunk must not reset the truncation flag (#528), got: {}",
            tool_reply.content
        );
        assert!(
            !tool_reply.content.contains("not valid JSON"),
            "truncation must not be mislabeled as invalid JSON (#528), got: {}",
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
            ReasoningVisibility::All,
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
            ReasoningVisibility::All,
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
            ReasoningVisibility::All,
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
            ReasoningVisibility::All,
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
            ReasoningVisibility::All,
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
            ReasoningVisibility::All,
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

    #[tokio::test]
    async fn done_event_reports_f1b_prefill_and_compaction_telemetry() {
        // #441: the Done event carries one prefill estimate per provider round-trip,
        // and zero compaction fires for a tiny transcript well under the budget.
        let store = SessionStore::new();
        let s = store.create_session(None);
        // A non-trivial prompt so the chars/4 proxy rounds to a positive estimate.
        store.add_message(
            &s.id,
            Role::User,
            "please summarize the architecture of this project in detail".into(),
        );
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;

        type F1b = (Option<Vec<u32>>, Option<u32>, Option<u32>);
        let seen: std::sync::Mutex<Option<F1b>> = std::sync::Mutex::new(None);
        run_turn(
            &TextProvider,
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::Done {
                    prefill_estimates,
                    tier1_fires,
                    tier2_fires,
                    ..
                } = ev
                {
                    *seen.lock().unwrap() = Some((prefill_estimates, tier1_fires, tier2_fires));
                }
            },
        )
        .await
        .unwrap();

        let (prefill, t1, t2) = seen
            .lock()
            .unwrap()
            .clone()
            .expect("Done event was emitted");
        let prefill = prefill.expect("prefill_estimates must be populated");
        // TextProvider answers in a single round-trip -> exactly one estimate, > 0.
        assert_eq!(prefill.len(), 1, "one prefill estimate per round-trip");
        assert!(
            prefill[0] > 0,
            "estimate should be positive, got {}",
            prefill[0]
        );
        // A two-message transcript is far under budget: no compaction engages, and
        // Tier-2 is default-off regardless.
        assert_eq!(t1, Some(0), "Tier-1 must not fire under budget");
        assert_eq!(t2, Some(0), "Tier-2 must not fire (and is default-off)");
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
            ReasoningVisibility::All,
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

    /// A tool whose result is a compressible JSON blob of a configurable size.
    struct JsonResultTool {
        summary_len: usize,
    }
    #[async_trait]
    impl ff_tools::Tool for JsonResultTool {
        fn name(&self) -> &str {
            "jsonbig"
        }
        fn description(&self) -> &str {
            "returns a large json blob"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": {} })
        }
        fn safety(&self, _args: &serde_json::Value) -> Safety {
            Safety::ReadOnly
        }
        async fn run(&self, _args: serde_json::Value, _root: &Path) -> ff_tools::ToolOutcome {
            let blob = serde_json::to_string(&serde_json::json!({
                "summary": "x".repeat(self.summary_len),
                "items": (0..50).map(|i| format!("row {i}")).collect::<Vec<_>>(),
            }))
            .unwrap();
            ff_tools::ToolOutcome::ok(blob)
        }
    }

    /// First call invokes `jsonbig`; second returns plain text.
    struct JsonToolThenText {
        calls: AtomicUsize,
    }
    #[async_trait]
    impl Provider for JsonToolThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("call_1".into()),
                        name: Some("jsonbig".into()),
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

    /// Call 0 invokes `jsonbig` (oversized -> compacted at ingest, gains a retrieve
    /// marker); call 1 reads the key out of that marker and invokes
    /// `compaction_retrieve`; call 2 returns plain text. Drives the RC6 path.
    struct JsonThenRetrieveThenText {
        calls: AtomicUsize,
    }
    #[async_trait]
    impl Provider for JsonThenRetrieveThenText {
        async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = match n {
                0 => vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("call_1".into()),
                        name: Some("jsonbig".into()),
                        arguments: "{}".into(),
                    }],
                    done: true,
                    ..Chunk::default()
                })],
                1 => {
                    // Pull the retrieve key out of the compacted tool result the loop
                    // just appended to the request, exactly as a real model would.
                    let key = req
                        .messages
                        .iter()
                        .filter_map(|m| m.content.as_deref())
                        .find_map(|c| c.split("[compacted; retrieve key=").nth(1))
                        .and_then(|rest| rest.split(']').next())
                        .map(str::to_owned)
                        .expect("the jsonbig result must carry a retrieve key");
                    vec![Ok(Chunk {
                        tool_calls: vec![ToolCallDelta {
                            index: 0,
                            id: Some("call_2".into()),
                            name: Some(COMPACTION_RETRIEVE_TOOL.into()),
                            arguments: format!(r#"{{"key":"{key}"}}"#),
                        }],
                        done: true,
                        ..Chunk::default()
                    })]
                }
                _ => vec![Ok(Chunk {
                    delta: "done".into(),
                    done: true,
                    ..Chunk::default()
                })],
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    /// RC6 (#476): `compaction_retrieve` returns a verbatim original that is, by
    /// definition, larger than the cap (exceeding the cap is the only reason it was
    /// compacted). The ingest gate must NOT re-compact the retrieve result -- doing
    /// so re-emits the same elision and the same deterministic key, a no-op loop
    /// that makes retrieve useless for any large original. The model's retrieve must
    /// land verbatim, marker-free.
    #[tokio::test]
    async fn retrieve_output_is_not_recompacted_at_ingest() {
        let store = std::sync::Arc::new(SessionStore::new());
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "expand the diff".into());
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(JsonResultTool {
            summary_len: TOOL_RESULT_MAX_BYTES + 4000,
        }));
        registry.register(Box::new(ff_tools::CompactionRetrieveTool::new(
            std::sync::Arc::clone(&store),
        )));
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;

        run_turn(
            &JsonThenRetrieveThenText {
                calls: AtomicUsize::new(0),
            },
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let history = store.get_messages(&s.id);
        let tool_msgs: Vec<_> = history.iter().filter(|m| m.role == Role::Tool).collect();
        assert_eq!(
            tool_msgs.len(),
            2,
            "one jsonbig result + one retrieve result"
        );

        // Sanity: the original jsonbig result was compacted at ingest (it is oversized).
        let jsonbig = &tool_msgs[0];
        assert!(
            jsonbig.content.contains("[compacted; retrieve key="),
            "oversized jsonbig result should be compacted: {}",
            jsonbig.content
        );

        // The fix: the retrieve result reaches the transcript verbatim -- no marker,
        // and larger than the cap (so it was neither re-compacted nor truncated).
        let retrieved = &tool_msgs[1];
        assert!(
            !retrieved.content.contains("[compacted; retrieve key="),
            "retrieve output must NOT be re-compacted at ingest: {}",
            &retrieved.content[..retrieved.content.len().min(200)]
        );
        assert!(
            retrieved.content.len() > TOOL_RESULT_MAX_BYTES,
            "retrieve output must be the verbatim (oversized) original, got {} bytes",
            retrieved.content.len()
        );
        // And it is exactly the original stored under the marker key.
        let key = jsonbig
            .content
            .rsplit("key=")
            .next()
            .unwrap()
            .trim_end_matches(']')
            .trim();
        assert_eq!(
            retrieved.content,
            store.compaction_original(key).expect("original stored"),
            "retrieve output must equal the verbatim stored original"
        );
    }

    #[tokio::test]
    async fn large_tool_result_is_compacted_and_retrievable() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "go".into());
        let mut registry = ToolRegistry::new();
        // Over the hard per-result byte cap, so it takes the reversible ingest
        // compaction path (RC1 #453: only oversized results are compacted at ingest).
        registry.register(Box::new(JsonResultTool {
            summary_len: TOOL_RESULT_MAX_BYTES + 4000,
        }));
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;

        let mut full_len = 0usize;
        run_turn(
            &JsonToolThenText {
                calls: AtomicUsize::new(0),
            },
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::ToolCallFinished { result, .. } = ev {
                    full_len = result.len();
                }
            },
        )
        .await
        .unwrap();

        let history = store.get_messages(&s.id);
        let tool_msg = history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("a tool result message");

        // The stored content was compacted (smaller than the original) and carries
        // the reversible retrieve marker.
        assert!(
            tool_msg.content.contains("[compacted; retrieve key="),
            "compacted tool result must carry a retrieve marker: {}",
            tool_msg.content
        );
        assert!(
            tool_msg.content.len() < full_len,
            "compacted content ({}) must be smaller than the original ({full_len})",
            tool_msg.content.len()
        );

        // The marker key resolves to the verbatim original in the store.
        let key = tool_msg
            .content
            .rsplit("key=")
            .next()
            .unwrap()
            .trim_end_matches(']')
            .trim();
        let original = store
            .compaction_original(key)
            .expect("the original must be retrievable by the marker key");
        assert_eq!(
            original.len(),
            full_len,
            "retrieved original must be verbatim"
        );
    }

    /// RC1 reproduction (PR #452 review timeline): a large tool result produced
    /// on the CURRENT turn must reach the model verbatim. Today it is compressed
    /// at ingest (lib.rs `compress_one` on the just-produced outcome) before the
    /// model ever reads it, so the model's first read of a large diff comes back
    /// already `[compacted; retrieve key=...]`. That forces a `compaction_retrieve`
    /// round-trip (or a re-read with a different tool), which is the redundant-step
    /// loop Abid observed. The cold-tail path (`compact_cold_collect` +
    /// `KEEP_RECENT_VERBATIM`) already compresses results once they age out of the
    /// hot window, so ingest-time compression of the hot result is both redundant
    /// and harmful.
    ///
    /// This test asserts the DESIRED behavior and currently FAILS, reproducing RC1.
    #[tokio::test]
    async fn current_turn_tool_result_reaches_model_verbatim() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "review this".into());
        let mut registry = ToolRegistry::new();
        // Within the hard per-result byte cap: must be stored verbatim at ingest.
        registry.register(Box::new(JsonResultTool { summary_len: 4000 }));
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;

        let mut full_len = 0usize;
        run_turn(
            &JsonToolThenText {
                calls: AtomicUsize::new(0),
            },
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::ToolCallFinished { result, .. } = ev {
                    full_len = result.len();
                }
            },
        )
        .await
        .unwrap();

        let history = store.get_messages(&s.id);
        let tool_msg = history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("a tool result message");

        // The most-recent tool result is in the hot window: the model has not yet
        // had a chance to read it, so it must be stored verbatim with NO retrieve
        // marker. A marker here means the model's first read is already compacted.
        assert!(
            !tool_msg.content.contains("[compacted; retrieve key="),
            "a current-turn tool result must NOT be compacted at ingest \
             (the model has not read it yet); got: {}",
            &tool_msg.content[..tool_msg.content.len().min(200)]
        );
        assert_eq!(
            tool_msg.content.len(),
            full_len,
            "the current-turn tool result must reach the transcript verbatim \
             (stored {} bytes vs original {full_len})",
            tool_msg.content.len()
        );
    }

    /// A tool result below the compaction threshold is stored verbatim with no
    /// marker and no stored original.
    struct SmallResultTool;
    #[async_trait]
    impl ff_tools::Tool for SmallResultTool {
        fn name(&self) -> &str {
            "small"
        }
        fn description(&self) -> &str {
            "returns a tiny blob"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": {} })
        }
        fn safety(&self, _args: &serde_json::Value) -> Safety {
            Safety::ReadOnly
        }
        async fn run(&self, _args: serde_json::Value, _root: &Path) -> ff_tools::ToolOutcome {
            ff_tools::ToolOutcome::ok("ok: 3 results")
        }
    }

    struct SmallToolThenText {
        calls: AtomicUsize,
    }
    #[async_trait]
    impl Provider for SmallToolThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("call_1".into()),
                        name: Some("small".into()),
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
    async fn small_tool_result_is_passed_through_uncompacted() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "go".into());
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(SmallResultTool));
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;

        run_turn(
            &SmallToolThenText {
                calls: AtomicUsize::new(0),
            },
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |_ev| {},
        )
        .await
        .unwrap();

        let history = store.get_messages(&s.id);
        let tool_msg = history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("a tool result message");
        assert_eq!(tool_msg.content, "ok: 3 results");
        assert!(!tool_msg.content.contains("[compacted"));
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
            ReasoningVisibility::All,
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
            ReasoningVisibility::All,
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
            ReasoningVisibility::All,
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

    fn extract_marker_key(content: &str) -> Option<String> {
        if !content.contains(COMPACTION_MARKER_PREFIX) {
            return None;
        }
        Some(
            content
                .rsplit("key=")
                .next()
                .unwrap()
                .trim_end_matches(']')
                .trim()
                .to_string(),
        )
    }

    #[tokio::test]
    async fn over_pressure_compacts_wire_but_store_stays_verbatim() {
        // Build a transcript heavy enough to clear the 0.75 budget fraction with
        // a long cold prefix of large, compressible blobs followed by small recent
        // turns. The wire request must be compacted; the store must stay verbatim.
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);

        // 10 cold messages, each a large JSON blob that compresses decisively.
        let mut cold_contents = Vec::new();
        for i in 0..10 {
            let blob = serde_json::to_string(&serde_json::json!({
                "idx": i,
                "summary": "y".repeat(9000),
                "items": (0..60).collect::<Vec<i32>>(),
            }))
            .unwrap();
            let role = if i % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            };
            store.add_message(&s.id, role, blob.clone());
            cold_contents.push(blob);
        }
        // 6 small recent turns kept byte-identical on the wire.
        let recents = ["r0", "r1", "r2", "r3", "r4", "r5"];
        for (i, r) in recents.iter().enumerate() {
            let role = if i % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            };
            store.add_message(&s.id, role, (*r).to_string());
        }

        // Sanity: we are actually over the extractive threshold.
        let history = store.get_messages(&s.id);
        let pressure = ProxyTokenEstimator::default().assess(&history, "mock");
        assert!(
            pressure.is_over(EXTRACTIVE_COMPACT_AT_FRACTION),
            "test transcript must exceed the extractive threshold: fraction={}",
            pressure.fraction()
        );

        let registry = ToolRegistry::new();
        let root = dir.path().to_path_buf();
        let approve = AlwaysApprove;
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = RecordingProvider { seen: seen.clone() };

        run_turn(
            &provider,
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let wire = seen.lock().unwrap().clone();
        // Wire has no system prompt here -> first message is the first cold blob.
        // The cold prefix must be compacted (marker present) and shorter.
        let cold_wire = &wire[0];
        assert!(
            cold_wire
                .content
                .as_deref()
                .unwrap()
                .contains(COMPACTION_MARKER_PREFIX),
            "cold prefix must be compacted on the wire"
        );
        assert!(
            cold_wire.content.as_deref().unwrap().len() < cold_contents[0].len(),
            "compacted wire content must be shorter than the original blob"
        );

        // The 6 most recent messages stay byte-identical on the wire.
        let n = wire.len();
        for (i, r) in recents.iter().enumerate() {
            assert_eq!(
                wire[n - recents.len() + i].content.as_deref().unwrap(),
                *r,
                "recent message {i} must be verbatim on the wire"
            );
        }

        // The store keeps the full verbatim transcript untouched.
        let stored = store.get_messages(&s.id);
        for (i, original) in cold_contents.iter().enumerate() {
            assert_eq!(
                &stored[i].content, original,
                "store must keep cold message {i} verbatim"
            );
        }

        // Each compacted blob's original is retrievable by its marker key.
        let key = extract_marker_key(cold_wire.content.as_deref().unwrap()).unwrap();
        assert_eq!(
            store.compaction_original(&key).as_deref(),
            Some(cold_contents[0].as_str()),
            "the verbatim original must be retrievable by its marker key"
        );
    }

    #[tokio::test]
    async fn below_pressure_wire_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "a short question".into());
        store.add_message(&s.id, Role::Assistant, "a short answer".into());
        store.add_message(&s.id, Role::User, "another short one".into());

        let history = store.get_messages(&s.id);
        let pressure = ProxyTokenEstimator::default().assess(&history, "mock");
        assert!(
            !pressure.is_over(EXTRACTIVE_COMPACT_AT_FRACTION),
            "small transcript must be below the extractive threshold"
        );

        let registry = ToolRegistry::new();
        let root = dir.path().to_path_buf();
        let approve = AlwaysApprove;
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = RecordingProvider { seen: seen.clone() };

        run_turn(
            &provider,
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let wire = seen.lock().unwrap().clone();
        for m in &wire {
            assert!(
                !m.content
                    .as_deref()
                    .unwrap_or_default()
                    .contains(COMPACTION_MARKER_PREFIX),
                "below pressure, no message may be compacted on the wire"
            );
        }
        // And nothing was persisted to the originals store.
        assert!(
            history
                .iter()
                .all(|m| store.compaction_original(&m.id).is_none()),
            "below pressure, no originals may be persisted"
        );
    }

    #[tokio::test]
    async fn tier2_summarizes_cold_prefix_but_store_stays_verbatim() {
        // Single-line cold messages that the Tier-1 extractive pass leaves alone
        // (one line each, so its line-elision never triggers): pressure stays high
        // *after* Tier 1, so the Tier-2 abstractive fallback engages and collapses
        // the cold prefix into a single summary message.
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);

        let mut cold_contents = Vec::new();
        for i in 0..30 {
            let line = format!("cold-{i} {}", "lorem ipsum dolor sit amet ".repeat(150));
            let role = if i % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            };
            store.add_message(&s.id, role, line.clone());
            cold_contents.push(line);
        }
        let recents = ["r0", "r1", "r2", "r3", "r4", "r5"];
        for (i, r) in recents.iter().enumerate() {
            let role = if i % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            };
            store.add_message(&s.id, role, (*r).to_string());
        }

        // Sanity: even after Tier 1 would run, these single-line blobs do not
        // shrink, so the wire stays over the Tier-2 fraction.
        let history = store.get_messages(&s.id);
        let wire_t1 =
            ExtractiveCompactor::default().compact_cold_collect(&history, KEEP_RECENT_VERBATIM);
        let pressure = ProxyTokenEstimator::default().assess(&wire_t1.messages, "mock");
        assert!(
            pressure.is_over(0.90),
            "post-Tier-1 transcript must exceed the Tier-2 fraction: fraction={}",
            pressure.fraction()
        );

        let registry = ToolRegistry::new();
        let root = dir.path().to_path_buf();
        let approve = AlwaysApprove;
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = RecordingProvider { seen: seen.clone() };

        let mut tctx = ctx(&registry, &root, &approve);
        tctx.abstractive = AbstractiveConfig {
            enabled: true,
            fire_at_fraction: 0.90,
            ..AbstractiveConfig::default()
        };

        run_turn(
            &provider,
            &store,
            &tctx,
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        // The main-turn wire begins with the synthetic summary (no system prompt
        // passed), and the 6 most recent messages stay byte-identical.
        let wire = seen.lock().unwrap().clone();
        let summary = &wire[0];
        assert_eq!(summary.role.as_str(), "user");
        let summary_text = summary.content.as_deref().unwrap();
        assert!(
            summary_text.contains("Summary of") && summary_text.contains(COMPACTION_MARKER_PREFIX),
            "the wire must lead with the abstractive summary + retrieve marker"
        );
        let n = wire.len();
        for (i, r) in recents.iter().enumerate() {
            assert_eq!(
                wire[n - recents.len() + i].content.as_deref().unwrap(),
                *r,
                "recent message {i} must be verbatim on the wire"
            );
        }
        // The collapsed cold prefix is far smaller than the 30 originals combined.
        assert!(wire.len() < history.len(), "cold prefix must be collapsed");

        // The store keeps the full verbatim transcript (plus this turn's reply).
        let stored = store.get_messages(&s.id);
        assert!(stored.len() >= history.len());
        for (i, original) in cold_contents.iter().enumerate() {
            assert_eq!(
                &stored[i].content, original,
                "store keeps cold {i} verbatim"
            );
        }

        // Reversible: the marker key resolves to the verbatim cold block.
        let key = extract_marker_key(summary_text).unwrap();
        let retrieved = store
            .compaction_original(&key)
            .expect("cold block is retrievable");
        assert!(retrieved.contains("cold-0"));
        assert!(retrieved.contains("cold-23"));
    }

    // ---- #458 RC5: per-turn semantic read dedupe ----

    #[tokio::test]
    async fn rereads_of_unchanged_file_collapse_to_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello world\nsecond line\n").unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "read it twice".into());
        let registry = ToolRegistry::with_defaults();
        let approve = AlwaysApprove;

        // Two views of the same file under *different* args (a line range on the
        // second), so the byte-identical repeat-breaker would NOT fire -- only RC5's
        // content dedupe catches it.
        struct ViewTwice {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl Provider for ViewTwice {
            async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
                let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let chunk = match n {
                    0 => Chunk {
                        tool_calls: vec![ToolCallDelta {
                            index: 0,
                            id: Some("v1".into()),
                            name: Some("view".into()),
                            arguments: r#"{"path":"f.txt"}"#.into(),
                        }],
                        done: true,
                        ..Chunk::default()
                    },
                    1 => Chunk {
                        tool_calls: vec![ToolCallDelta {
                            index: 0,
                            id: Some("v2".into()),
                            name: Some("view".into()),
                            arguments: r#"{"path":"f.txt","start_line":1}"#.into(),
                        }],
                        done: true,
                        ..Chunk::default()
                    },
                    _ => Chunk {
                        delta: "done".into(),
                        done: true,
                        ..Chunk::default()
                    },
                };
                Ok(futures_util::stream::iter(vec![Ok(chunk)]).boxed())
            }
        }

        run_turn(
            &ViewTwice {
                calls: AtomicUsize::new(0),
            },
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let history = store.get_messages(&s.id);
        let results: Vec<&str> = history
            .iter()
            .filter(|m| m.role == Role::Tool)
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(results.len(), 2, "two view calls -> two tool results");
        assert!(
            results[0].contains("hello world"),
            "first read returns full content: {}",
            results[0]
        );
        assert!(
            results[1].contains("unchanged since step"),
            "re-read of unchanged file is deduped to the sentinel: {}",
            results[1]
        );
    }

    #[tokio::test]
    async fn changed_file_is_not_deduped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "v1\n").unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "read, change, read".into());
        let registry = ToolRegistry::with_defaults();
        let approve = AlwaysApprove;

        // view -> bash overwrites the file -> view again. The second read's content
        // differs (different hash), so it must NOT be deduped.
        struct ViewEditView {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl Provider for ViewEditView {
            async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
                let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let tc = |id: &str, name: &str, args: &str| ToolCallDelta {
                    index: 0,
                    id: Some(id.into()),
                    name: Some(name.into()),
                    arguments: args.into(),
                };
                let chunk = match n {
                    0 => Chunk {
                        tool_calls: vec![tc("v1", "view", r#"{"path":"f.txt"}"#)],
                        done: true,
                        ..Chunk::default()
                    },
                    1 => Chunk {
                        tool_calls: vec![tc(
                            "b2",
                            "bash",
                            r#"{"command":"printf 'v2\n' > f.txt"}"#,
                        )],
                        done: true,
                        ..Chunk::default()
                    },
                    2 => Chunk {
                        tool_calls: vec![tc("v3", "view", r#"{"path":"f.txt"}"#)],
                        done: true,
                        ..Chunk::default()
                    },
                    _ => Chunk {
                        delta: "done".into(),
                        done: true,
                        ..Chunk::default()
                    },
                };
                Ok(futures_util::stream::iter(vec![Ok(chunk)]).boxed())
            }
        }

        run_turn(
            &ViewEditView {
                calls: AtomicUsize::new(0),
            },
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            ReasoningVisibility::All,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let history = store.get_messages(&s.id);
        // The second view (id v3) returns the new content in full, not a sentinel.
        let reread = history
            .iter()
            .find(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some("v3"))
            .expect("re-read tool result present");
        assert!(
            reread.content.contains("v2"),
            "changed file is re-read in full: {}",
            reread.content
        );
        assert!(
            !reread.content.contains("unchanged since step"),
            "a changed file must NOT be deduped: {}",
            reread.content
        );
    }
}
