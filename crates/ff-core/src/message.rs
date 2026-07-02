use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

/// Why a turn ended without a usable answer (#658). The single source of truth
/// for the `[stopped…]` marker text the agent loop persists, so the frontend can
/// classify and render a stopped turn structurally instead of string-matching the
/// marker. Persisted on the assistant [`Message`] and carried on
/// [`TurnDoneEvent`](crate::events::TurnDoneEvent); absent for a normal turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum StopReason {
    /// The user pressed Stop (`cancel.is_cancelled()`).
    Cancelled,
    /// The agent loop hit its tool-call iteration cap.
    ToolLimit,
    /// A no-progress stall: the model repeated the identical tool call past the
    /// repeat-breaker threshold (#244 R2).
    Stall,
    /// The model returned an empty response even after the bounded retries.
    EmptyResponse,
}

impl StopReason {
    /// The persisted `[stopped…]` notice text for this reason. The one place the
    /// marker strings live, so the frontend never has to reproduce them.
    pub fn marker(self) -> &'static str {
        match self {
            StopReason::Cancelled => "[stopped]",
            StopReason::ToolLimit => "[stopped: reached tool-call limit]",
            StopReason::Stall => "[stopped: repeated a tool call without making progress]",
            StopReason::EmptyResponse => "[stopped: the model returned an empty response]",
        }
    }

    /// The stable string persisted in SQLite / crossed on the wire (camelCase, to
    /// match the ts-rs binding). Pairs with [`StopReason::from_wire`].
    pub fn as_wire(self) -> &'static str {
        match self {
            StopReason::Cancelled => "cancelled",
            StopReason::ToolLimit => "toolLimit",
            StopReason::Stall => "stall",
            StopReason::EmptyResponse => "emptyResponse",
        }
    }

    /// Parse a persisted wire string back into a variant; `None` for an unknown
    /// value (a forward-compat guard for a row written by a newer schema).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "cancelled" => Some(StopReason::Cancelled),
            "toolLimit" => Some(StopReason::ToolLimit),
            "stall" => Some(StopReason::Stall),
            "emptyResponse" => Some(StopReason::EmptyResponse),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// JSON-encoded arguments object (a string, per the tool-calling protocol).
    pub arguments: String,
}

/// What an [`Attachment`] is, so a provider/UI can route it (vision vs. document).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum AttachmentKind {
    Image,
    Document,
}

/// Where an attachment's bytes live. A `Path` reference is preferred — it keeps the
/// transcript and the database small, with base64 materialized only at provider
/// send time. `Inline` carries base64 directly for sources that have no file on
/// disk (e.g. a clipboard paste).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", tag = "type", content = "value")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum AttachmentSource {
    Path(String),
    Inline(String),
}

/// A file attached to a message (image or document). Round-trips through
/// persistence and bridges to each provider's wire format at send time. Foundation
/// for multimodal support (#332); providers consume it in their own tickets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct Attachment {
    pub kind: AttachmentKind,
    /// IANA media type, e.g. "image/png" or "application/pdf".
    pub media_type: String,
    pub source: AttachmentSource,
    /// Original file name, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    /// Size of the payload in bytes.
    #[ts(type = "number")]
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub role: Role,
    pub content: String,
    /// Present on an assistant message that requested tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Present on a `Role::Tool` message, binding the result to its request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tool_call_id: Option<String>,
    /// Files attached to this message (multimodal, #332). Absent for a plain
    /// text message; omitted from the wire/binding when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub attachments: Option<Vec<Attachment>>,
    /// The model's chain-of-thought for an assistant turn, captured from the
    /// reasoning stream so it can be round-tripped to reasoning-capable providers
    /// on later tool-calling turns (#375). Absent for non-reasoning turns; stored
    /// verbatim and omitted from the wire/binding when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reasoning: Option<String>,
    /// Why this assistant turn ended without a usable answer (#658). Present only
    /// on a stopped turn's finalized notice message; lets the frontend classify
    /// and render the stop structurally instead of string-matching `content`.
    /// Omitted from the wire/binding when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stop_reason: Option<StopReason>,
    /// The phenotype that produced this (assistant) message, captured when the row
    /// was created so history shows the true historical author rather than the
    /// currently active phenotype (#657). Holds the raw phenotype name (the FE
    /// profile id), resolved to a display name at render. `None` for user/tool/system
    /// rows and for pre-existing rows, which fall back to live resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub author_name: Option<String>,
    /// Unix epoch milliseconds.
    #[ts(type = "number")]
    pub created_at: i64,
}
