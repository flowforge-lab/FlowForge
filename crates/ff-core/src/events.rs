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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_progress_serializes_with_exact_contract_keys() {
        let ev = UpdateProgressEvent {
            downloaded: 1024,
            total: Some(4096),
        };
        // The frontend listener reads these exact keys; both are single words so
        // camelCase leaves them unchanged.
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            serde_json::json!({ "downloaded": 1024, "total": 4096 })
        );
        // An absent content length serializes as null, not a missing key.
        let unknown = UpdateProgressEvent {
            downloaded: 1024,
            total: None,
        };
        assert_eq!(
            serde_json::to_value(&unknown).unwrap(),
            serde_json::json!({ "downloaded": 1024, "total": null })
        );
    }

    #[test]
    fn phenotype_mcp_unavailable_serializes_with_exact_contract_keys() {
        let ev = PhenotypeMcpUnavailableEvent {
            phenotype: "codon".into(),
            servers: vec!["codegraph".into(), "fetch".into()],
        };
        let v = serde_json::to_value(&ev).unwrap();
        // The frontend listener (lib/ipc.ts onPhenotypeMcpUnavailable) reads these
        // exact keys; both are single words so camelCase leaves them unchanged.
        assert_eq!(
            v,
            serde_json::json!({ "phenotype": "codon", "servers": ["codegraph", "fetch"] })
        );
        let back: PhenotypeMcpUnavailableEvent = serde_json::from_value(v).unwrap();
        assert_eq!(back.phenotype, "codon");
        assert_eq!(back.servers, vec!["codegraph", "fetch"]);
    }
}
