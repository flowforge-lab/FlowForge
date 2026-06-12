//! Payloads for backend -> frontend Tauri events. Names mirror the SOP event table.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::SessionStatus;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct TokenEvent {
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
    /// `"write"` or `"dangerous"` — read-only calls never require approval.
    pub safety: String,
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
