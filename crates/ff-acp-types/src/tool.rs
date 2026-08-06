//! Tool call types — the wire representations of tool execution, used in
//! `session/update` notifications and `session/request_permission` payloads.

use serde::{Deserialize, Serialize};

use crate::rpc::Meta;

/// A unique identifier for a tool call within a session.
pub type ToolCallId = String;

/// An update to an existing tool call. Only the `toolCallId` is required; all
/// other fields are optional — only changed fields need to be included.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallUpdate {
    pub tool_call_id: ToolCallId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ToolKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ToolCallStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ToolCallContent>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<ToolCallLocation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Categories of tools. Helps clients choose icons and display tool execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    Other,
}

/// Execution status of a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// Content produced by a tool call. Tagged by its `type` member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolCallContent {
    /// A standard content block.
    Content(Content),
    /// A file diff.
    Diff(Diff),
    /// An embedded terminal display.
    Terminal(Terminal),
}

/// A standard content block wrapped for tool call display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Content {
    pub content: crate::content::ContentBlock,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// A file diff produced by a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diff {
    /// Absolute path of the file being modified.
    pub path: String,
    /// Original content (`None` for new files).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    /// New content after modification.
    pub new_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// A location being accessed or modified by a tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallLocation {
    /// Absolute path.
    pub path: String,
    /// Optional line number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// An embedded terminal display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Terminal {
    pub terminal_id: TerminalId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// A typed identifier for terminal instances on the wire.
pub type TerminalId = String;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_kind_round_trip() {
        for kind in [
            ToolKind::Read,
            ToolKind::Edit,
            ToolKind::Delete,
            ToolKind::Move,
            ToolKind::Search,
            ToolKind::Execute,
            ToolKind::Think,
            ToolKind::Fetch,
            ToolKind::SwitchMode,
            ToolKind::Other,
        ] {
            let json = serde_json::to_value(kind).unwrap();
            let back: ToolKind = serde_json::from_value(json).unwrap();
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn test_tool_kind_serialization() {
        assert_eq!(
            serde_json::to_value(ToolKind::SwitchMode).unwrap(),
            "switch_mode"
        );
        assert_eq!(serde_json::to_value(ToolKind::Read).unwrap(), "read");
    }

    #[test]
    fn test_tool_call_status_round_trip() {
        for status in [
            ToolCallStatus::Pending,
            ToolCallStatus::InProgress,
            ToolCallStatus::Completed,
            ToolCallStatus::Failed,
        ] {
            let json = serde_json::to_value(status).unwrap();
            let back: ToolCallStatus = serde_json::from_value(json).unwrap();
            assert_eq!(back, status);
        }
    }

    #[test]
    fn test_tool_call_status_serialization() {
        assert_eq!(
            serde_json::to_value(ToolCallStatus::InProgress).unwrap(),
            "in_progress"
        );
    }

    #[test]
    fn test_tool_call_update_round_trip() {
        let update = ToolCallUpdate {
            tool_call_id: "tc_1".into(),
            kind: Some(ToolKind::Edit),
            status: Some(ToolCallStatus::InProgress),
            title: Some("Editing file".into()),
            content: None,
            locations: None,
            raw_input: None,
            raw_output: None,
            _meta: None,
        };
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["toolCallId"], "tc_1");
        assert_eq!(json["kind"], "edit");
        assert_eq!(json["status"], "in_progress");
        assert_eq!(json["title"], "Editing file");

        let back: ToolCallUpdate = serde_json::from_value(json).unwrap();
        assert_eq!(back, update);
    }

    #[test]
    fn test_tool_call_update_minimal() {
        let update = ToolCallUpdate {
            tool_call_id: "tc_2".into(),
            kind: None,
            status: None,
            title: None,
            content: None,
            locations: None,
            raw_input: None,
            raw_output: None,
            _meta: None,
        };
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["toolCallId"], "tc_2");
        assert!(json.get("kind").is_none());
        let back: ToolCallUpdate = serde_json::from_value(json).unwrap();
        assert_eq!(back, update);
    }

    #[test]
    fn test_tool_call_content_text_round_trip() {
        let content = ToolCallContent::Content(Content {
            content: crate::content::ContentBlock::Text(crate::content::TextContent {
                annotations: None,
                text: "result".into(),
                _meta: None,
            }),
            _meta: None,
        });
        let json = serde_json::to_value(&content).unwrap();
        assert_eq!(json["type"], "content");
        assert_eq!(json["content"]["type"], "text");
        let back: ToolCallContent = serde_json::from_value(json).unwrap();
        assert_eq!(back, content);
    }

    #[test]
    fn test_tool_call_content_diff_round_trip() {
        let content = ToolCallContent::Diff(Diff {
            path: "/workspace/main.rs".into(),
            old_text: Some("old".into()),
            new_text: "new".into(),
            _meta: None,
        });
        let json = serde_json::to_value(&content).unwrap();
        assert_eq!(json["type"], "diff");
        assert_eq!(json["path"], "/workspace/main.rs");
        let back: ToolCallContent = serde_json::from_value(json).unwrap();
        assert_eq!(back, content);
    }

    #[test]
    fn test_tool_call_content_terminal_round_trip() {
        let content = ToolCallContent::Terminal(Terminal {
            terminal_id: "term_1".into(),
            _meta: None,
        });
        let json = serde_json::to_value(&content).unwrap();
        assert_eq!(json["type"], "terminal");
        assert_eq!(json["terminalId"], "term_1");
        let back: ToolCallContent = serde_json::from_value(json).unwrap();
        assert_eq!(back, content);
    }

    #[test]
    fn test_tool_call_location_round_trip() {
        let loc = ToolCallLocation {
            path: "/workspace/main.rs".into(),
            line: Some(42),
            _meta: None,
        };
        let json = serde_json::to_value(&loc).unwrap();
        assert_eq!(json["path"], "/workspace/main.rs");
        assert_eq!(json["line"], 42);
        let back: ToolCallLocation = serde_json::from_value(json).unwrap();
        assert_eq!(back, loc);
    }

    #[test]
    fn test_diff_round_trip() {
        let diff = Diff {
            path: "/workspace/main.rs".into(),
            old_text: None,
            new_text: "fn main() {}".into(),
            _meta: None,
        };
        let json = serde_json::to_value(&diff).unwrap();
        assert_eq!(json["path"], "/workspace/main.rs");
        assert_eq!(json["newText"], "fn main() {}");
        let back: Diff = serde_json::from_value(json).unwrap();
        assert_eq!(back, diff);
    }

    #[test]
    fn test_unknown_fields_tolerated_on_tool_call_update() {
        let json = serde_json::json!({
            "toolCallId": "tc_1",
            "unknownField": "ignored"
        });
        let update: ToolCallUpdate = serde_json::from_value(json).unwrap();
        assert_eq!(update.tool_call_id, "tc_1");
    }
}
