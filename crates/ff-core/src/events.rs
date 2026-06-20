//! Payloads for backend -> frontend Tauri events. Names mirror the SOP event table.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{McpServerStatus, SessionStatus};

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
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct TurnErrorEvent {
    pub session_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct IntentionSignal {
    pub session_id: String,
    pub goal: String,
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
