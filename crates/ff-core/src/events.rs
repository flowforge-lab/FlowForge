//! Payloads for backend -> frontend Tauri events. Names mirror the SOP event table.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{McpServerStatus, SessionStatus, StopReason};

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct TokenEvent {
    pub session_id: String,
    pub message_id: String,
    pub delta: String,
}

/// Reasoning/thinking stream delta for an in-flight assistant message (#181).
/// Emitted only when the provider sends reasoning content; never persisted on
/// [`crate::Message`] — the frontend accumulates it separately from `content`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ReasoningEvent {
    pub session_id: String,
    pub message_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ToolCallEvent {
    pub session_id: String,
    /// Assistant message the call belongs to.
    pub message_id: String,
    /// Correlates the call with its [`ToolResultEvent`].
    pub call_id: String,
    pub tool: String,
    #[ts(type = "unknown")]
    pub args: serde_json::Value,
}

/// Trust level of a tool call that requires user approval. Read-only calls never
/// reach approval, so this enum carries only the two gated levels — it is the typed
/// contract for [`ToolApprovalRequestEvent::safety`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum ApprovalSafety {
    Write,
    Sensitive,
    Dangerous,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ToolApprovalRequestEvent {
    pub session_id: String,
    /// Assistant message the call belongs to.
    pub message_id: String,
    /// Correlates with the [`ToolCallEvent`] / [`ToolResultEvent`] for this call.
    pub call_id: String,
    pub tool: String,
    #[ts(type = "unknown")]
    pub args: serde_json::Value,
    /// Trust level — read-only calls never require approval.
    pub safety: ApprovalSafety,
}

/// Backend -> frontend request to put a clarifying question to the user (the
/// `ask_user` tool, #44). The turn pauses until the frontend replies via the
/// `respond_ask(session_id, call_id, answer)` command. Dismissing it (turn cancel)
/// resolves the question as "[no answer: question dismissed]" — never a hang. `call_id` correlates with
/// the [`ToolCallEvent`] / [`ToolResultEvent`] for the same step.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ToolAskRequestEvent {
    pub session_id: String,
    pub message_id: String,
    pub call_id: String,
    pub question: String,
    /// The model requested a secret (#562): the frontend renders a masked field
    /// and the resolved answer is redacted to a placeholder when persisted. `false`
    /// for an ordinary question.
    pub secret: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ToolResultEvent {
    pub session_id: String,
    pub message_id: String,
    pub call_id: String,
    pub success: bool,
    pub result: String,
}

/// Which standard stream a live output chunk (#680) came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum OutputStreamKind {
    Stdout,
    Stderr,
}

/// A live chunk of a running command's output (#680), emitted as the process
/// produces it — before (and in addition to) the final [`ToolResultEvent`]. The
/// frontend appends `delta` to the running tool-call block so long builds/tests
/// show progress instead of appearing frozen. `call_id` correlates with the
/// [`ToolCallEvent`] / [`ToolResultEvent`] for the same step.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ToolOutputChunkEvent {
    pub session_id: String,
    pub message_id: String,
    pub call_id: String,
    pub stream: OutputStreamKind,
    pub delta: String,
}

/// The `chars/4` context-size estimate at turn end, split into the three
/// component buckets the context-usage popover renders (#931): the transient
/// system prompt, the advertised tool schemas, and the persisted message
/// transcript. Proportions drive the segmented bar; the popover shows each
/// bucket's token count and share of the budget.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ContextBreakdown {
    /// Estimated tokens of the system prompt (persona + skills + ambient context).
    pub system_tokens: u32,
    /// Estimated tokens of the advertised tool schemas.
    pub tool_tokens: u32,
    /// Number of tool specs advertised this turn.
    pub tool_specs: u32,
    /// Estimated tokens of the persisted message transcript (user/assistant/tool).
    pub message_tokens: u32,
    /// Number of messages in the transcript.
    pub message_count: u32,
}

/// Provider-reported token usage for a turn (#931), summed across the turn's
/// round-trips. Distinct from the `chars/4` proxy: these are authoritative
/// counts from the provider's usage metadata (Bedrock `ConverseStream`
/// `TokenUsage`, OpenAI `usage`). The frontend accumulates these across turns to
/// render the SESSION TOTALS block; all fields are 0 when the provider does not
/// report usage.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct TurnUsage {
    /// Prompt tokens sent to the model this turn (cumulative over round-trips).
    pub input_tokens: u32,
    /// Completion tokens generated by the model this turn.
    pub output_tokens: u32,
    /// Prompt-prefix cache-read (hit) tokens this turn.
    pub cache_read_tokens: u32,
    /// Prompt-prefix cache-write (miss) tokens this turn.
    pub cache_write_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct TurnDoneEvent {
    pub session_id: String,
    pub message_id: String,
    /// Estimated token count of the session context at turn end, for a
    /// frontend context-usage indicator. `None` when not assessed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u32>,
    /// Why the turn stopped without a usable answer (#658), when it did. `None`
    /// for a normal completion. Lets the frontend offer the Continue affordance
    /// and render the Cancelled banner without string-matching the notice text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    /// Component breakdown of the context-size estimate (#931), for the
    /// context-usage popover's segmented bar and rows. `None` when not assessed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub breakdown: Option<ContextBreakdown>,
    /// Provider-reported token usage for this turn (#931). `None` when the
    /// provider does not report usage (e.g. no metadata frame).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub usage: Option<TurnUsage>,
}

/// Per-turn timing baseline (F1, #427): the wall-clock breakdown the performance
/// epic (#426) measures every later change against. `round_trips` is the number of
/// provider responses (agent loop iterations) this turn; `iter_ms` is the
/// per-iteration wall-clock in arrival order; `flushes` counts silent mid-turn
/// memory flushes (each an extra provider round-trip); `chars` is the streamed
/// assistant text, a coarse token-cost proxy; `prefill_estimates` is the per-
/// round-trip projected request size and `tier1_fires`/`tier2_fires` count how
/// often each compaction tier engaged (F1b, #441). The F1b fields are optional on
/// the wire -- the desktop always populates them, but a non-desktop emitter may
/// omit them cleanly (#475 follow-up). Emitted once at turn end.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct TurnStatsEvent {
    pub session_id: String,
    pub round_trips: u32,
    pub total_ms: u32,
    pub iter_ms: Vec<u32>,
    pub flushes: u32,
    pub chars: u32,
    /// F1b (#441): projected prefill-token estimate of each round-trip's outgoing
    /// request (post-compaction wire), in iteration order. Omitted by emitters that
    /// do not compute F1b telemetry.
    ///
    /// Invariant when present: `prefill_estimates.len() == round_trips` (one
    /// estimate per round-trip). The frontend should assert this when it eventually
    /// consumes `turn:stats` events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub prefill_estimates: Option<Vec<u32>>,
    /// F1b (#441): iterations that engaged the Tier-1 extractive compaction pass.
    /// Omitted by emitters that do not compute F1b telemetry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tier1_fires: Option<u32>,
    /// F1b (#441): iterations that engaged the Tier-2 abstractive cold-tail summary.
    /// Omitted by emitters that do not compute F1b telemetry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tier2_fires: Option<u32>,
    /// TTFT: milliseconds from the moment the host handed the request to
    /// `run_turn` to the arrival of the first assistant token. Answers the
    /// dominant question the #427 baseline could not: "how long until the
    /// model starts talking?" — the sum of provider RTT + queue + prefill.
    /// `None` when the turn produced no assistant message (e.g. an early
    /// error or cancel before the first token streamed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub first_token_ms: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct TurnErrorEvent {
    pub session_id: String,
    pub message: String,
}

/// A silent context-pressure memory flush wrote durable facts to the user's
/// on-disk memory mid-turn (#283, follow-up to #244 R5). Emitted only when the
/// flush actually wrote something, so the memory browser can surface provenance
/// ("memory auto-updated this turn").
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct MemoryFlushedEvent {
    pub session_id: String,
    pub message_id: String,
    /// Number of durable facts written this turn (always > 0 when emitted).
    pub writes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct IntentionSignal {
    pub session_id: String,
    pub goal: String,
}

/// A session's title was regenerated as an LLM one-line summary after its first
/// turn (#671 item 2b). The heuristic `auto_title` seeds an instant title on the
/// first user message; this replaces it with a better summary once a reply exists.
/// The frontend patches the cached session title in place -- no refetch.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct SessionTitleUpdatedEvent {
    pub session_id: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct OutcomeSignal {
    pub session_id: String,
    pub status: SessionStatus,
}

/// Backend -> frontend request to approve installing a skill (M3.2). Emitted after
/// the bundle is fetched and validated, so the user approves with the real declared
/// `manifest` (name, version, tools/permissions) in hand. The frontend replies via
/// the shared `respond_approval(session_id, call_id, approved)` command — the same
/// gate as dangerous tool calls. An install has no turn, so the backend keys it by
/// `request_id`: reply with `request_id` for BOTH `session_id` and `call_id`.
/// `warnings` carries non-fatal validation notes.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct SkillInstallApprovalRequestEvent {
    pub request_id: String,
    pub source: String,
    pub manifest: crate::SkillManifest,
    pub warnings: Vec<String>,
}

/// Backend -> frontend notice that the active skill set changed (an
/// activate/deactivate, or an install/uninstall reload that pruned a missing
/// skill). Carries the full active set so the frontend replaces its state rather
/// than diffing. The installed-skill list itself is re-fetched via `list_skills`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct SkillsChangedEvent {
    pub active: Vec<String>,
}

/// Backend -> frontend notice that one or more MCP servers changed status (a
/// start/stop/restart, a connect failure, or an enable/disable/add/remove reload).
/// Carries the full status snapshot so the frontend replaces its state rather than
/// diffing, mirroring [`SkillsChangedEvent`]. The server definitions themselves are
/// re-fetched via `list_mcp_servers`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct McpStatusChangedEvent {
    pub servers: Vec<McpServerStatus>,
}

/// Backend -> frontend notice that a just-activated phenotype lists a skill whose
/// declared MCP server is unavailable (#301/#235) -- absent from `mcp.json`, or
/// present but not `Running`. Mirrors the warn-only signal `warn_missing_skill_mcp`
/// logs; emitted only when the unavailable list is non-empty so the frontend toast
/// fires exactly when there is something to report. Non-fatal: activation never
/// blocks and the skill's grep/glob fallbacks still work. `servers` is name-sorted
/// and deduplicated.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct PhenotypeMcpUnavailableEvent {
    pub phenotype: String,
    pub servers: Vec<String>,
}

/// Backend -> frontend telemetry: a skill became active for a turn (M3.5, RFC 0001
/// §8). Emitted once per active skill at the start of each agent turn. `ff-signals`
/// folds these into per-skill aggregates (activation counts); the frontend may also
/// surface live activity. Pairs with [`SkillCompleted`] at turn end.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct SkillActivated {
    pub skill: String,
    pub session_id: String,
}

/// Backend -> frontend telemetry: a turn that had `skill` active finished (M3.5, RFC
/// 0001 §8). Emitted once per active skill when the turn ends. `tokens` is a coarse
/// cost proxy (streamed assistant characters / 4) until real provider usage is wired
/// (deferred with the M4 autonomous trigger); `turns` counts agent loop iterations;
/// `latency_ms` is wall-clock turn duration; `success` is true when the turn ended
/// cleanly (not error/cancel). `ff-signals` folds these into rolling per-skill
/// aggregates (mean tokens/turns, success rate).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct SkillCompleted {
    pub skill: String,
    pub session_id: String,
    pub tokens: u32,
    pub latency_ms: u32,
    pub turns: u32,
    pub success: bool,
}

/// Rough cost projection shown alongside an optimize proposal (RFC 0001 §8). Both
/// values use the same coarse token proxy as the telemetry substrate, so they are
/// comparable to each other but approximate in absolute terms (real provider usage
/// lands in M4). `estimatedMeanTokens` scales the current rolling mean by the body's
/// size change; `0.0` when the skill has no telemetry yet.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct EvolveCostEstimate {
    pub current_mean_tokens: f64,
    pub estimated_mean_tokens: f64,
}

/// Backend -> frontend request to approve an optimize/evolve rewrite of a skill
/// (M3.5, RFC 0001 §8). The model has proposed `after_body`; the frontend shows the
/// before→after diff (it has both bodies) plus the cost estimate, and the user
/// approves or rejects. Like a skill install, this is a standalone approval with no
/// turn, so it is keyed by `request_id` and answered via the shared
/// `respond_approval(request_id, request_id, approved)` command. On approval the
/// skill is version-bumped to `new_version`, retaining the previous version for
/// rollback — it is never silently overwritten.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct SkillEvolveApprovalRequestEvent {
    pub request_id: String,
    pub skill: String,
    pub current_version: String,
    pub new_version: String,
    pub before_body: String,
    pub after_body: String,
    pub cost_estimate: EvolveCostEstimate,
}

/// Download progress for an in-flight self-update install (#566, RFC 0014 section
/// 12.2). Emitted as `update:progress` from `install_update` per downloaded chunk;
/// `total` is the content length, absent when the feed does not send one (the UI
/// then renders an indeterminate bar). App-global -- there is one install at a time,
/// so no session id. A terminal `update:download-finished` event (empty payload)
/// follows the last chunk, before the app relaunches.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct UpdateProgressEvent {
    pub downloaded: u64,
    pub total: Option<u64>,
}

/// One chunk of live stdout/stderr from a background process started via
/// `process_manager action=start` (#873). Emitted as `process:output`,
/// independently of any assistant turn, by a desktop bridge task that
/// subscribes to the process's output broadcast the moment it starts. The
/// frontend appends `delta` to the process's output panel keyed by
/// `process_id`. `stream` is `"stdout"` or `"stderr"`. Unlike the per-turn
/// `token`/`tool:output` events, these keep flowing across turns for the
/// life of the process.
///
/// `process_id` is the small sequential id `start` returns (fits a `u32`,
/// so the frontend gets a plain `number`, not a `bigint`). `stream` reuses the
/// same [`OutputStreamKind`] the per-turn `tool:output` chunks use.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ProcessOutputEvent {
    pub session_id: String,
    pub process_id: u32,
    pub stream: OutputStreamKind,
    pub delta: String,
}

/// A background process ended (exited, was killed, or failed to run) (#873).
/// Emitted once as `process:exited` after the last `process:output`, when the
/// bridge task sees the output broadcast close. `status` is the supervisor's
/// human-readable label -- `"exited(0)"`, `"exited(3)"`, `"killed"`, or
/// `"failed: <reason>"` -- which the frontend renders as the process's
/// terminal status badge.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ProcessExitedEvent {
    pub session_id: String,
    pub process_id: u32,
    pub status: String,
}

#[cfg(test)]
mod tests;
