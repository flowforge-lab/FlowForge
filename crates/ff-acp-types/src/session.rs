//! Session types — session identifiers, session-scoped values, and the
//! `session/update` notification payload tree.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::rpc::Meta;
use crate::tool::{ToolCallId, ToolCallLocation, ToolCallStatus, ToolCallUpdate, ToolKind};

// The ACP wire IDs (`SessionId`, `ToolCallId`, `TerminalId`, `PermissionOptionId`,
// ...) are modeled as type aliases over `String`, not newtypes. A newtype would
// catch a `ToolCallId` passed where a `SessionId` is expected at compile time,
// which matters for the #1203 permission-mapping layer. We keep aliases so the
// wire layer stays maximally plain and conversions are zero-cost everywhere; if
// a consumer proves type confusion is a real risk, promote the aliases to
// `pub struct X(pub String)` — the change is mechanical and this crate is the
// only place that needs editing.

/// A unique session identifier.
pub type SessionId = String;

/// A message identifier within a session.
pub type MessageId = String;

/// A session-mode identifier.
pub type SessionModeId = String;

/// A session-config-option identifier.
pub type SessionConfigId = String;

// ---------------------------------------------------------------------------
// Session/info
// ---------------------------------------------------------------------------

/// Summary information about a session, returned by `session/list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// Session/update — the SessionUpdate tagged union
// ---------------------------------------------------------------------------

/// A session update notification payload. Tagged by `sessionUpdate`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdate {
    /// A chunk of the user's message.
    UserMessageChunk(ContentChunk),
    /// A chunk of the agent's message.
    AgentMessageChunk(ContentChunk),
    /// A chunk of the agent's internal thought.
    AgentThoughtChunk(ContentChunk),
    /// A new tool call.
    ToolCall(ToolCall),
    /// An update to an existing tool call.
    ToolCallUpdate(ToolCallUpdate),
    /// The agent's current plan.
    Plan(Plan),
    /// Update to available commands.
    AvailableCommandsUpdate(AvailableCommandsUpdate),
    /// Update to the current session mode.
    CurrentModeUpdate(CurrentModeUpdate),
    /// Update to config options.
    ConfigOptionUpdate(ConfigOptionUpdate),
    /// Update to session info (title, timestamps).
    SessionInfoUpdate(SessionInfoUpdate),
    /// Update to usage statistics.
    UsageUpdate(UsageUpdate),
}

/// A chunk of streaming content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentChunk {
    pub content: crate::content::ContentBlock,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// A tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub tool_call_id: ToolCallId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ToolKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ToolCallStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<crate::tool::ToolCallContent>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<ToolCallLocation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// The agent's plan structure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub entries: Vec<PlanEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// A single plan entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PlanEntryStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<PlanEntryPriority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Plan entry status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// Plan entry priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryPriority {
    Critical,
    High,
    Medium,
    Low,
}

/// Update to available commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableCommandsUpdate {
    pub available_commands: Vec<AvailableCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// A single available command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableCommand {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Update to the current session mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentModeUpdate {
    pub current_mode_id: SessionModeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Update to session config options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigOptionUpdate {
    pub config_options: Vec<SessionConfigOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// A session config option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigOption {
    pub config_id: String,
    pub name: String,
    pub description: String,
    pub value: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Update to session metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Update to usage statistics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageUpdate {
    pub used: u64,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Usage cost information.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    pub amount: f64,
    pub currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// Session mode and state
// ---------------------------------------------------------------------------

/// The current mode state within a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModeState {
    pub modes: Vec<SessionMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// A session mode definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMode {
    pub mode_id: SessionModeId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// MCP server definition
// ---------------------------------------------------------------------------

/// A single MCP server entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_info_round_trip() {
        let info = SessionInfo {
            session_id: "sess_1".into(),
            title: Some("My Session".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
            _meta: None,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["sessionId"], "sess_1");
        assert_eq!(json["title"], "My Session");

        let back: SessionInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back, info);
    }

    #[test]
    fn test_session_update_tool_call_variant() {
        let update = SessionUpdate::ToolCall(ToolCall {
            tool_call_id: "tc_1".into(),
            title: "Editing file".into(),
            kind: Some(crate::tool::ToolKind::Edit),
            status: Some(crate::tool::ToolCallStatus::InProgress),
            content: None,
            locations: None,
            raw_input: None,
            raw_output: None,
            _meta: None,
        });
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["sessionUpdate"], "tool_call");
        assert_eq!(json["toolCallId"], "tc_1");
        assert_eq!(json["title"], "Editing file");

        let back: SessionUpdate = serde_json::from_value(json).unwrap();
        assert_eq!(back, update);
    }

    #[test]
    fn test_session_update_plan_variant() {
        let update = SessionUpdate::Plan(Plan {
            entries: vec![PlanEntry {
                id: "entry_1".into(),
                title: "Step 1".into(),
                description: "Do the thing".into(),
                status: Some(PlanEntryStatus::InProgress),
                priority: Some(PlanEntryPriority::High),
                _meta: None,
            }],
            _meta: None,
        });
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["sessionUpdate"], "plan");
        assert_eq!(json["entries"][0]["id"], "entry_1");
        assert_eq!(json["entries"][0]["status"], "in_progress");

        let back: SessionUpdate = serde_json::from_value(json).unwrap();
        assert_eq!(back, update);
    }

    #[test]
    fn test_usage_update_round_trip() {
        let update = UsageUpdate {
            used: 1000,
            size: 5000,
            cost: Some(Cost {
                amount: 0.05,
                currency: "USD".into(),
                _meta: None,
            }),
            _meta: None,
        };
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["used"], 1000);
        assert_eq!(json["cost"]["amount"], 0.05);
        assert_eq!(json["cost"]["currency"], "USD");

        let back: UsageUpdate = serde_json::from_value(json).unwrap();
        assert_eq!(back, update);
    }

    #[test]
    fn test_plan_entry_status_round_trip() {
        for status in [
            PlanEntryStatus::Pending,
            PlanEntryStatus::InProgress,
            PlanEntryStatus::Completed,
            PlanEntryStatus::Failed,
        ] {
            let json = serde_json::to_value(status).unwrap();
            let back: PlanEntryStatus = serde_json::from_value(json).unwrap();
            assert_eq!(back, status);
        }
    }

    #[test]
    fn test_plan_entry_priority_round_trip() {
        for priority in [
            PlanEntryPriority::Critical,
            PlanEntryPriority::High,
            PlanEntryPriority::Medium,
            PlanEntryPriority::Low,
        ] {
            let json = serde_json::to_value(priority).unwrap();
            let back: PlanEntryPriority = serde_json::from_value(json).unwrap();
            assert_eq!(back, priority);
        }
    }

    #[test]
    fn test_mcp_server_round_trip() {
        let server = McpServer {
            id: "mcp_1".into(),
            url: "http://localhost:8080".into(),
            headers: vec![("Auth".into(), "token".into())].into_iter().collect(),
            _meta: None,
        };
        let json = serde_json::to_value(&server).unwrap();
        assert_eq!(json["id"], "mcp_1");
        assert_eq!(json["headers"]["Auth"], "token");
        let back: McpServer = serde_json::from_value(json).unwrap();
        assert_eq!(back, server);
    }

    #[test]
    fn test_session_mode_round_trip() {
        let mode = SessionMode {
            mode_id: "mode_1".into(),
            name: "Plan".into(),
            description: Some("Plan mode".into()),
            _meta: None,
        };
        let json = serde_json::to_value(&mode).unwrap();
        assert_eq!(json["modeId"], "mode_1");
        let back: SessionMode = serde_json::from_value(json).unwrap();
        assert_eq!(back, mode);
    }

    #[test]
    fn test_unknown_fields_tolerated_on_session_info() {
        let json = serde_json::json!({
            "sessionId": "sess_1",
            "extraField": "ignored"
        });
        let info: SessionInfo = serde_json::from_value(json).unwrap();
        assert_eq!(info.session_id, "sess_1");
    }
}
