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

use ff_core::{
    AttachmentKind, Message, Mode, PermissionCell, PermissionMatrix, ReasoningVisibility, Role,
    StopReason,
};
use ff_llm::{ChatMessage, ChatRequest, FunctionCall, LlmError, Provider, ToolCall as LlmToolCall};
use ff_session::SessionStore;
use ff_tools::{Safety, ToolRegistry, COMPACTION_RETRIEVE_TOOL};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

mod compaction;
mod compaction_abstractive;
mod compaction_cache;
mod compaction_extractive;
mod goal_loop;
mod system_prompt;
pub use compaction::{
    flush_due, CompactionContext, CompactionOutcome, CompactionStrategy, ContextPressure,
    ContextPressureEstimator, MemoryFlush, ProxyTokenEstimator, DEFAULT_CONTEXT_BUDGET_TOKENS,
    DEFAULT_FLUSH_AT_FRACTION,
};
pub use compaction_abstractive::{
    build_summary_prompt, summary_due, AbstractiveConfig, AbstractiveSummarizer, SummaryResult,
};
pub use compaction_cache::CompactionCache;
pub use compaction_extractive::{
    classify, proxy_tokens, ColdCompaction, CompactionSavings, CompressOutcome, ContentKind,
    ExtractiveCompactor, ReversibleCache, COMPACTION_MARKER_PREFIX,
};
pub use goal_loop::{drive_goal, GateDecision, GoalIteration, IterationOutcome, LoopStop};
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

/// A rate-limit (429/quota) window is a ~minute-scale TPM/RPM reset, not a
/// transport blip, so it gets its own, larger retry budget (#571). Waiting out a
/// window must not consume the snappy transport-retry budget above.
const MAX_RATE_LIMIT_ATTEMPTS: usize = 5;

/// Base backoff for rate-limit retries when the gateway sends no `Retry-After`:
/// the Nth retry (0-based) waits `BASE << N` ms (1s, 2s, 4s, 8s, 16s), capped.
const RATE_LIMIT_BACKOFF_BASE_MS: u64 = 1_000;

/// Hard ceiling on any single rate-limit wait, whether from exponential backoff
/// or a `Retry-After` header, so a hostile/buggy header cannot park a turn for
/// minutes.
const RATE_LIMIT_BACKOFF_MAX_MS: u64 = 30_000;

/// Backoff (ms) before a rate-limit retry. Honors the gateway's `Retry-After`
/// when present (clamped to [`RATE_LIMIT_BACKOFF_MAX_MS`]); otherwise exponential
/// on the 0-based `attempt` (1s, 2s, 4s, ... capped). Pure for unit testing.
fn rate_limit_delay(attempt: usize, retry_after: Option<std::time::Duration>) -> u64 {
    if let Some(d) = retry_after {
        let ms = u64::try_from(d.as_millis()).unwrap_or(RATE_LIMIT_BACKOFF_MAX_MS);
        return ms.min(RATE_LIMIT_BACKOFF_MAX_MS);
    }
    RATE_LIMIT_BACKOFF_BASE_MS
        .checked_shl(attempt as u32)
        .unwrap_or(RATE_LIMIT_BACKOFF_MAX_MS)
        .min(RATE_LIMIT_BACKOFF_MAX_MS)
}

/// Decide whether a transient provider error should be retried, and if so how
/// long to back off. Splits the two regimes (#571): a `RateLimited` window uses
/// the seconds-scale schedule + its own `rate_limit_attempt` budget (honoring
/// `Retry-After`), while every other transient error keeps the snappy
/// transport-blip schedule + the `attempt` budget. Returns `Some(delay_ms)` to
/// retry, or `None` to surface the error. `attempt` / `rate_limit_attempt` are
/// the counts *already consumed* for each regime (1-based: the unconditional
/// `attempt += 1` at the loop top runs before this is consulted).
fn retry_backoff_ms(error: &LlmError, attempt: usize, rate_limit_attempt: usize) -> Option<u64> {
    if !error.is_transient() {
        return None;
    }
    match error {
        LlmError::RateLimited { retry_after, .. } => {
            if rate_limit_attempt >= MAX_RATE_LIMIT_ATTEMPTS {
                return None;
            }
            Some(rate_limit_delay(rate_limit_attempt, *retry_after))
        }
        _ => {
            if attempt >= MAX_PROVIDER_ATTEMPTS {
                return None;
            }
            Some(RETRY_BACKOFF_BASE_MS << (attempt - 1))
        }
    }
}

/// Whether an error is a rate-limit window (used to bump the right counter).
fn is_rate_limited(error: &LlmError) -> bool {
    matches!(error, LlmError::RateLimited { .. })
}

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

/// Stand-in for a secret `ask_user` answer (#562). When the model asked with
/// `secret: true`, the real reply must never surface downstream — not in the
/// stored transcript (chat history + replayed model context) and not on the
/// `ToolCallFinished` event that reaches the UI. The outcome content is replaced
/// with this placeholder at the source, so every consumer sees it instead.
const SECRET_ANSWER_PLACEHOLDER: &str = "[secret provided by user]";

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
        /// Why the turn stopped without a usable answer (#658), when it did.
        /// `None` for a normal text completion.
        #[serde(skip_serializing_if = "Option::is_none")]
        stop_reason: Option<StopReason>,
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
        /// Prefix cache hit tokens across all iterations this turn (#766).
        /// Populated from the provider's usage response; 0 when not reported.
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_hit_tokens: Option<u32>,
        /// Prefix cache miss tokens across all iterations this turn (#766).
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_miss_tokens: Option<u32>,
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
    /// because the active model can't carry their kind (#338). The per-provider
    /// capability strip is otherwise silent; this turns the drop into a visible
    /// notice. Emitted once per turn (first iteration only), keyed to that turn's
    /// assistant message. As of the #338 follow-up documents are universally
    /// supported (Bedrock `DocumentBlock` + OpenAI/Ollama extraction fallback), so
    /// in the host path the only kind that can drop is images — a non-vision
    /// model. The agent logic stays general (counts per unsupported kind) so a
    /// future provider that drops documents needs no change here.
    AttachmentsDropped {
        message_id: String,
        count: u32,
    },
    /// A live chunk of a running tool's output (#680), emitted as it is produced —
    /// before (and additive to) the final [`AgentEvent::ToolCallFinished`]. Only
    /// streaming tools (currently `bash`) produce these; the frontend appends `delta`
    /// to the running tool-call block so slow builds/tests show progress.
    ToolOutputChunk {
        message_id: String,
        call_id: String,
        stream: ff_tools::OutputStream,
        delta: String,
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
    /// Agent autonomy mode (RFC 0011). Controls tool visibility and approval
    /// via the [`PermissionMatrix`].
    pub mode: Mode,
    /// Permission policy for this turn (#699). Determines which tools are
    /// advertised (non-Deny) and which are auto-approved (Allow) vs prompted (Ask).
    pub matrix: &'a PermissionMatrix,
    /// Tier-2 abstractive cold-tail summary config (RFC 0016 M7.0). Default-off;
    /// the host enables/tunes it. Sub-agents inherit the parent's setting.
    pub abstractive: AbstractiveConfig,
    /// Fast model for compaction/flush LLM calls (#756). When set, memory flush
    /// and abstractive summarization use this instead of the session model, so a
    /// cheap model (Haiku, gpt-4o-mini) handles bookkeeping without blocking the
    /// main turn with a slow Opus round-trip. `None` = use session model (legacy).
    pub compaction_model: Option<String>,
    /// Explicit compaction budget in tokens (#756). When set, overrides the
    /// default `model_window * CONTEXT_BUDGET_SAFETY`. Maps to the UI's
    /// "Summarization threshold" slider. `None` = use the computed default.
    pub compaction_budget: Option<u64>,
    /// Cross-turn summary cache (#757). When provided, `run_turn` seeds its
    /// local summary state from the previous turn's result and writes back on
    /// update, eliminating redundant summarizer calls across turns. `None` =
    /// no cross-turn caching (legacy / CLI / sub-agents).
    pub compaction_cache: Option<&'a CompactionCache>,
}

impl<'a> ToolContext<'a> {
    /// A top-level context: full toolset, no delegation parent, default depth cap.
    pub fn new(
        registry: &'a ToolRegistry,
        root: &'a Path,
        approve: &'a dyn Approver,
        max_iterations: usize,
        matrix: &'a PermissionMatrix,
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
            matrix,
            abstractive: AbstractiveConfig::default(),
            compaction_model: None,
            compaction_budget: None,
            compaction_cache: None,
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
/// Filter the advertised tool set based on Mode (#699, #793).
///
/// In [`Mode::Plan`] (RFC 0011) a tool is advertised when it has a genuine
/// read-only path ([`ToolRegistry::readonly_capable_names`], e.g. `bash ls`,
/// `gh pr_list`) **or** its worst-case tier is not `Deny` in the Plan matrix row
/// (so opening `Plan x Sensitive` surfaces `web_fetch`/`web_search`). The per-call
/// [`Tool::safety`] gate then rejects any concrete invocation that exceeds what the
/// Plan row permits — `bash rm` (Dangerous) and `gh pr_create` (Write) are denied
/// even though the tool is visible. Pure-mutation tools (`python`, `write`) whose
/// floor is above ReadOnly stay hidden unless the matrix opens their tier.
///
/// In Act/Auto all tools remain visible; the matrix's Deny cells are enforced at
/// **invocation time** (the approver rejects the call) rather than hiding the tool.
fn advertised_tools(
    mode: Mode,
    matrix: &PermissionMatrix,
    allowed: Option<&std::collections::HashSet<String>>,
    registry: &ToolRegistry,
) -> Option<std::collections::HashSet<String>> {
    if !mode.is_plan() {
        return allowed.cloned();
    }
    // Plan mode: read-capable tools, plus any whose ceiling the Plan matrix row
    // does not Deny. Invocation-time `safety` + matrix gate the concrete calls.
    let mut visible = registry.readonly_capable_names();
    for tool in registry.iter_tools() {
        if matrix.cell(Mode::Plan, tool.max_safety()) != PermissionCell::Deny {
            visible.insert(tool.name().to_string());
        }
    }
    Some(match allowed {
        Some(set) => set.intersection(&visible).cloned().collect(),
        None => visible,
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

/// Notice written to a reserved-but-unfinalized assistant row when its turn is
/// interrupted mid-flight. Shares the `[stopped: ...]` vocabulary with the
/// in-loop terminal notices (empty-response / tool-call limit / cancel).
pub const INTERRUPTED_NOTICE: &str = StopReason::Interrupted.marker();

/// RAII guard for the assistant message row reserved at the top of each loop
/// iteration. The row is created empty (so the frontend can route streaming
/// tokens) *before* the provider is called; if the `run_turn` future is dropped
/// mid-iteration (window closed, superseding turn, runtime teardown) before the
/// row is finalized with content, tool calls, or a terminal notice, the empty
/// row would otherwise survive as a silent blank assistant bubble (#646).
///
/// Mirrors [`ToolResultBackfill`]: seeded per iteration, [`finalize`](Self::finalize)d
/// on every normal exit path (content set, tool calls attached, or a break/return
/// that hands off to the post-loop notice), and on `Drop` writes
/// [`INTERRUPTED_NOTICE`] only when still unfinalized. The store call is
/// synchronous, so a plain `Drop` impl suffices. This covers the *graceful* drop;
/// a hard kill (SIGKILL / panic=abort) runs no `Drop`, so session load reconciles
/// any orphan left behind (see the desktop `get_messages` sweep).
struct AssistantRowGuard<'a> {
    store: &'a SessionStore,
    session_id: &'a str,
    message_id: String,
    finalized: bool,
}

impl<'a> AssistantRowGuard<'a> {
    fn new(store: &'a SessionStore, session_id: &'a str, message_id: String) -> Self {
        Self {
            store,
            session_id,
            message_id,
            finalized: false,
        }
    }

    /// Mark the row as durably resolved: it now carries content, tool calls, or is
    /// about to be finalized by the post-loop notice path. Suppresses the drop-time
    /// interrupted notice so it never clobbers a more accurate terminal message.
    fn finalize(&mut self) {
        self.finalized = true;
    }
}

impl Drop for AssistantRowGuard<'_> {
    fn drop(&mut self) {
        if self.finalized {
            return;
        }
        self.store.set_message_content(
            &self.message_id,
            self.session_id,
            INTERRUPTED_NOTICE.to_string(),
        );
        // Stamp the structured reason alongside the notice text so the frontend
        // classifies the interrupted row via `Message.stop_reason` instead of the
        // legacy `[stopped…]` string match. Both store calls are synchronous and the
        // guard holds the id, so this is a direct second call — no async plumbing.
        self.store.set_message_stop_reason(
            &self.message_id,
            self.session_id,
            StopReason::Interrupted,
        );
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
    let advertised = advertised_tools(
        tools.mode,
        tools.matrix,
        tools.allowed.as_ref(),
        tools.registry,
    );
    let tool_schemas = tools
        .registry
        .openai_tools_for(advertised.as_ref(), allow_subagent);
    let mut last: Option<Message> = None;

    let max_iter = tools.max_iterations.max(1);
    // Size the compaction budget to THIS model's real context window (#B1) so a
    // large-window model isn't force-compacted at a small fixed ceiling. Built
    // once per turn and reused for every pressure check below.
    let estimator = ProxyTokenEstimator {
        budget_tokens: tools.compaction_budget.unwrap_or_else(|| {
            ((provider.context_window(model) as f64) * CONTEXT_BUDGET_SAFETY) as u64
        }),
    };
    let mut turn_count: u32 = 0;
    // Repeated-call / no-progress guard (#244 R2): count identical `(tool, arguments)`
    // calls across the turn; `repeat_nudge` carries a tool name to warn about on the
    // next request; `stop_reason` ends the turn with a clear notice when a stall
    // persists past the nudge.
    let mut call_counts: HashMap<(String, String), usize> = HashMap::new();
    let mut repeat_nudge: Option<String> = None;
    let mut stop_reason: Option<StopReason> = None;
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
    //
    // Cross-turn seeding (#757): if a previous turn left a cached summary for
    // this session, start from it instead of None so we skip the expensive
    // re-summarization when only a few messages were appended.
    let (mut last_summary, mut last_summary_count) = tools
        .compaction_cache
        .and_then(|c| c.get(session_id))
        .map(|(b, msg, count)| (Some((b, msg)), Some(count)))
        .unwrap_or((None, None));
    // F1b (#441) telemetry: the projected prefill estimate of each round-trip's
    // outgoing wire, plus how often each compaction tier engaged this turn. Folded
    // into the `Done` event so the desktop's `turn:stats` can report them. Purely
    // observational -- never gates behavior.
    let mut prefill_estimates: Vec<u32> = Vec::new();
    let mut tier1_fires: u32 = 0;
    let mut tier2_fires: u32 = 0;
    // Prefix cache observability (#766): accumulate provider-reported cache
    // hit/miss tokens across all iterations this turn.
    let mut cache_hit_tokens: u32 = 0;
    let mut cache_miss_tokens: u32 = 0;
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
            let flush_model = tools.compaction_model.as_deref().unwrap_or(model);
            if let Ok(CompactionOutcome::Wrote { writes }) = MemoryFlush
                .compact(CompactionContext {
                    provider,
                    store,
                    registry: tools.registry,
                    root: tools.root,
                    session_id,
                    model: flush_model,
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
                    let compact_model = tools.compaction_model.as_deref().unwrap_or(model);
                    match AbstractiveSummarizer::new(tools.abstractive.clone())
                        .summarize_cold(
                            provider,
                            compact_model,
                            &wire,
                            KEEP_RECENT_VERBATIM,
                            &cancel,
                        )
                        .await
                    {
                        Ok(Some(result)) => {
                            if let Some((mid, key, original)) = &result.original {
                                store.put_compaction_original(session_id, mid, key, original);
                            }
                            last_summary = Some((result.boundary, result.messages[0].clone()));
                            last_summary_count = Some(message_count);
                            // Write-through to cross-turn cache (#757).
                            if let Some(cache) = tools.compaction_cache {
                                cache.put(
                                    session_id,
                                    result.boundary,
                                    result.messages[0].clone(),
                                    message_count,
                                );
                            }
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

        // Drop-safe finalize (#646): the row above is persisted empty so streaming
        // tokens have a home. If the turn future is dropped before the row is
        // finalized with content, tool calls, or a terminal notice, this guard
        // backfills an interrupted notice on Drop so the row is never a silent
        // blank bubble. `finalize()` disarms it on every normal exit path.
        let mut row_guard = AssistantRowGuard::new(store, session_id, message_id.clone());

        // Provenance for a flush that ran at the top of this iteration (#283): now
        // that the turn's assistant message id exists, correlate the event with it.
        if let Some(writes) = flushed_writes {
            on_event(AgentEvent::MemoryFlushed {
                message_id: message_id.clone(),
                writes,
            });
        }

        // Graceful-degradation notice (#338): when the active model can't carry an
        // attachment kind, the per-provider capability strip silently drops it
        // before it reaches the wire. Surface that once per turn so the drop isn't
        // invisible. Count only the attachments the model truly can't handle —
        // images when `!supports_vision`, documents when `!supports_documents` —
        // so the text-extraction fallback (#338 follow-up) doesn't false-fire a
        // "dropped" notice for a document the model actually received as text.
        // Counted on the turn's triggering user message (the last user message in
        // history); first-iteration gating avoids re-notifying on each tool-loop
        // iteration, and last-message scoping avoids re-firing on later turns
        // whose own input carries nothing.
        let supports_vision = provider.supports_vision();
        let supports_documents = provider.supports_documents();
        if iter == 0 && (!supports_vision || !supports_documents) {
            let dropped = history
                .iter()
                .rev()
                .find(|m| m.role == Role::User)
                .and_then(|m| m.attachments.as_ref())
                .map_or(0, |atts| {
                    atts.iter()
                        .filter(|a| match a.kind {
                            AttachmentKind::Image => !supports_vision,
                            AttachmentKind::Document => !supports_documents,
                        })
                        .count()
                });
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
        // Rate-limit (429/quota) retries use a separate budget + seconds-scale
        // schedule so waiting out a TPM window does not exhaust the transport
        // retry budget (#571).
        let mut rate_limit_attempt = 0usize;
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
            // Pin a context-aware output ceiling so a large tool-call payload is not
            // truncated at the gateway's small default cap (#550). Reuse this turn's
            // prefill estimate (pushed above) as the input size.
            let input_tokens = prefill_estimates.last().copied().unwrap_or(0) as u64;
            let req = ChatRequest {
                model: model.to_string(),
                messages: messages.clone(),
                tools: if withhold_tools {
                    Vec::new()
                } else {
                    tool_schemas.clone()
                },
                thinking: step_thinking,
                max_tokens: ff_llm::budgeted_max_output_tokens(model, input_tokens),
                cache_messages: true,
            };

            let mut stream = match provider.chat_stream(req).await {
                Ok(s) => s,
                Err(e) => {
                    if let Some(delay) = retry_backoff_ms(&e, attempt, rate_limit_attempt) {
                        if is_rate_limited(&e) {
                            // A rate-limit wait must not consume the transport
                            // budget that the unconditional `attempt += 1` above
                            // just charged (#571).
                            attempt -= 1;
                            rate_limit_attempt += 1;
                        }
                        cancellable_backoff(&cancel, delay).await;
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
                    // Fatal provider error: the Error event above already tells the
                    // user why the turn ended. Disarm so the guard does not overwrite
                    // the reserved row with a redundant interrupted notice (#646).
                    row_guard.finalize();
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
                        // Prefix cache observability (#766): the final usage chunk
                        // carries the totals; earlier chunks report 0.
                        cache_hit_tokens += chunk.cache_hit_tokens;
                        cache_miss_tokens += chunk.cache_miss_tokens;
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
                            // Drain one trailing chunk for the usage frame (#766).
                            // OpenAI/SiliconFlow send cache metrics on a separate
                            // final chunk (choices:[], usage:{...}) AFTER the
                            // finish_reason chunk. Without this, cache_hit_tokens
                            // is always 0.
                            if let Some(Ok(trailing)) = stream.next().await {
                                cache_hit_tokens += trailing.cache_hit_tokens;
                                cache_miss_tokens += trailing.cache_miss_tokens;
                            }
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
                Some(e)
                    if !emitted_any
                        && retry_backoff_ms(&e, attempt, rate_limit_attempt).is_some() =>
                {
                    // Safe: the guard only matches when `retry_backoff_ms` is Some.
                    let delay = retry_backoff_ms(&e, attempt, rate_limit_attempt).unwrap();
                    if is_rate_limited(&e) {
                        attempt -= 1;
                        rate_limit_attempt += 1;
                    }
                    cancellable_backoff(&cancel, delay).await;
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
                    // Fatal provider error: the Error event above already tells the
                    // user why the turn ended. Disarm so the guard does not overwrite
                    // the reserved row with a redundant interrupted notice (#646).
                    row_guard.finalize();
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
        // The row is now committed (content, or an empty body that a subsequent
        // break hands to the post-loop notice, or tool calls attached just below).
        // Every path from here reaches a proper terminal state, so disarm the
        // drop-time interrupted backfill (#646).
        row_guard.finalize();

        // No tool calls -> this is the final text answer.
        if calls.is_empty() {
            // Empty even after the bounded R7 retries (or the turn was cancelled): don't
            // emit an empty Done bubble. Set a notice and let the post-loop finalize on
            // this same reserved message, so there is no orphan empty assistant message.
            if final_text.trim().is_empty() {
                if !cancel.is_cancelled() && stop_reason.is_none() {
                    stop_reason = Some(StopReason::EmptyResponse);
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
                stop_reason: None,
                turns: Some(turn_count),
                token_count,
                prefill_estimates: Some(prefill_estimates.clone()),
                tier1_fires: Some(tier1_fires),
                tier2_fires: Some(tier2_fires),
                cache_hit_tokens: Some(cache_hit_tokens),
                cache_miss_tokens: Some(cache_miss_tokens),
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
                        // Use the session-aware entry (not bare `run`, which passes
                        // NO_SESSION): a ReadOnly tool run in the parallel batch may
                        // still be session-scoped — e.g. notebook_runner `status`,
                        // ProcessManagerTool `poll`/`list` — and must see the same
                        // session_id the serial `run_streaming` path uses, or it
                        // queries an empty anonymous bucket (#863).
                        tools
                            .registry
                            .run_with_session(&name, args, tools.root, session_id)
                            .await,
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
                                // Stream live output (#680): pass a sink into the
                                // streaming dispatch and drive the tool future
                                // concurrently with a drain loop that forwards each
                                // chunk via `on_event`. `on_event` is owned by this
                                // loop and the tool `await` would otherwise block it,
                                // so the concurrent drive is required. Non-streaming
                                // tools ignore the sink and this reduces to a plain
                                // await. The final outcome is unchanged.
                                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(
                                    ff_tools::OutputStream,
                                    String,
                                )>();
                                let sink = ff_tools::OutputSink::new(tx);
                                let fut = tools.registry.run_streaming(
                                    &call.name,
                                    args,
                                    tools.root,
                                    session_id,
                                    Some(sink),
                                );
                                tokio::pin!(fut);
                                loop {
                                    tokio::select! {
                                        outcome = &mut fut => {
                                            // Forward any chunks buffered before the
                                            // future resolved and dropped its sender.
                                            while let Ok((stream, delta)) = rx.try_recv() {
                                                on_event(AgentEvent::ToolOutputChunk {
                                                    message_id: message_id.clone(),
                                                    call_id: call.id.clone(),
                                                    stream,
                                                    delta,
                                                });
                                            }
                                            break outcome;
                                        }
                                        Some((stream, delta)) = rx.recv() => {
                                            on_event(AgentEvent::ToolOutputChunk {
                                                message_id: message_id.clone(),
                                                call_id: call.id.clone(),
                                                stream,
                                                delta,
                                            });
                                        }
                                    }
                                }
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
            // Secret redaction (#562): an `ask_user` answered with `secret: true`
            // must not surface the cleartext anywhere downstream. Replace the
            // outcome content with the placeholder at the source so BOTH the
            // persisted transcript row AND the `ToolCallFinished` event below carry
            // the placeholder — never the real value. This is loss-free: nothing
            // in-flight consumes it (the model replays the placeholder from history;
            // `process.rs` spawns with `Stdio::null()`).
            let is_secret_ask = outcome.success
                && tools.registry.is_interactive(&call.name)
                && serde_json::from_str::<serde_json::Value>(&call.arguments)
                    .ok()
                    .and_then(|a| a.get("secret").and_then(serde_json::Value::as_bool))
                    .unwrap_or(false);
            if is_secret_ask {
                outcome.content = SECRET_ANSWER_PLACEHOLDER.to_string();
            }
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
                stop_reason = Some(StopReason::Stall);
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
    // The turn ended without a plain-text answer: resolve why so the notice text,
    // the persisted structured `stop_reason`, and the `Done` event all agree (#658).
    // A user cancel wins over any in-loop reason; absent one, the loop exhausted its
    // iteration cap.
    let reason = if cancel.is_cancelled() {
        StopReason::Cancelled
    } else {
        stop_reason.unwrap_or(StopReason::ToolLimit)
    };
    let stop_reason = if msg.content.is_empty() {
        // The final assistant message only carried tool calls, so it would render as
        // an empty bubble. Persist the structured reason first, then replace the
        // content with the reason's marker -- set_message_content re-selects the row,
        // so the returned `msg` carries the just-written stop_reason too. The `Done`
        // event and the persisted `Message.stop_reason` therefore always agree.
        store.set_message_stop_reason(&msg.id, session_id, reason);
        msg = store.set_message_content(&msg.id, session_id, reason.marker().to_string());
        Some(reason)
    } else {
        // The turn produced real content (rare on this path): leave it as-is and
        // report no stop reason, exactly as before #658.
        None
    };
    // Same context-size estimate as the plain-text completion path (#244 R6).
    let token_count = Some(
        estimator
            .assess(&store.get_messages(session_id), model)
            .estimated_tokens as u32,
    );
    on_event(AgentEvent::Done {
        message_id: msg.id.clone(),
        final_message: Some(msg.content.clone()),
        stop_reason,
        turns: Some(turn_count),
        token_count,
        prefill_estimates: Some(prefill_estimates),
        tier1_fires: Some(tier1_fires),
        tier2_fires: Some(tier2_fires),
        cache_hit_tokens: Some(cache_hit_tokens),
        cache_miss_tokens: Some(cache_miss_tokens),
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
        matrix: parent.matrix,
        abstractive: parent.abstractive.clone(),
        compaction_model: parent.compaction_model.clone(),
        compaction_budget: parent.compaction_budget,
        compaction_cache: None, // Sub-agents are ephemeral; no cross-turn caching.
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
mod tests;
