//! Agent→Client method payloads, their responses, and notifications.
//!
//! These 9 methods are the requests the *agent* sends to the *client*
//! (editor/IDE). Our ACP integration must handle them.

use serde::{Deserialize, Serialize};

use crate::permission::{PermissionOption, RequestPermissionOutcome};
use crate::rpc::Meta;
use crate::session::SessionId;
use crate::tool::TerminalId;
use crate::tool::ToolCallUpdate;

// ---------------------------------------------------------------------------
// fs/read_text_file
// ---------------------------------------------------------------------------

/// Request: `fs/read_text_file`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTextFileRequest {
    pub session_id: SessionId,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `fs/read_text_file`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTextFileResponse {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// fs/write_text_file
// ---------------------------------------------------------------------------

/// Request: `fs/write_text_file`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteTextFileRequest {
    pub session_id: SessionId,
    pub path: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `fs/write_text_file`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteTextFileResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// session/request_permission
// ---------------------------------------------------------------------------

/// Request: `session/request_permission`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionRequest {
    pub session_id: SessionId,
    pub tool_call: ToolCallUpdate,
    pub options: Vec<PermissionOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `session/request_permission`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionResponse {
    pub outcome: RequestPermissionOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// terminal/create
// ---------------------------------------------------------------------------

/// Request: `terminal/create`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTerminalRequest {
    pub session_id: SessionId,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<EnvVariable>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_byte_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `terminal/create`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTerminalResponse {
    pub terminal_id: TerminalId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// An environment variable for a terminal or MCP server sub-process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvVariable {
    pub name: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// terminal/output
// ---------------------------------------------------------------------------

/// Request: `terminal/output`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOutputRequest {
    pub session_id: SessionId,
    pub terminal_id: TerminalId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `terminal/output`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOutputResponse {
    pub output: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<TerminalExitStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// The exit status of a terminal command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalExitStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// terminal/release
// ---------------------------------------------------------------------------

/// Request: `terminal/release`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseTerminalRequest {
    pub session_id: SessionId,
    pub terminal_id: TerminalId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `terminal/release`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseTerminalResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// terminal/wait_for_exit
// ---------------------------------------------------------------------------

/// Request: `terminal/wait_for_exit`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitForTerminalExitRequest {
    pub session_id: SessionId,
    pub terminal_id: TerminalId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `terminal/wait_for_exit`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitForTerminalExitResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// terminal/kill
// ---------------------------------------------------------------------------

/// Request: `terminal/kill`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KillTerminalRequest {
    pub session_id: SessionId,
    pub terminal_id: TerminalId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `terminal/kill`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KillTerminalResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// elicitation/create
// ---------------------------------------------------------------------------

/// Request: `elicitation/create`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateElicitationRequest {
    pub session_id: SessionId,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ElicitationMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// The elicitation mode: a form or URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum ElicitationMode {
    Form(ElicitationFormMode),
    Url(ElicitationUrlMode),
}

/// Form-based elicitation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationFormMode {
    pub requested_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// URL-based elicitation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationUrlMode {
    pub elicitation_id: ElicitationId,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// A unique elicitation identifier.
pub type ElicitationId = String;

/// Response: `elicitation/create`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateElicitationResponse {
    pub action: ElicitationAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// The client's action in response to an elicitation request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum ElicitationAction {
    Accept(ElicitationAcceptAction),
    Decline,
    Cancel,
}

/// An accepted elicitation action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationAcceptAction {
    pub elicitation_id: ElicitationId,
    pub result: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ===========================================================================
// Notifications
// ===========================================================================

// ---------------------------------------------------------------------------
// session/update (agent → client)
// ---------------------------------------------------------------------------

/// Notification: `session/update`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotification {
    pub session_id: SessionId,
    pub update: crate::session::SessionUpdate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// elicitation/complete (agent → client)
// ---------------------------------------------------------------------------

/// Notification: `elicitation/complete`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteElicitationNotification {
    pub elicitation_id: ElicitationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::{PermissionOption, PermissionOptionKind};

    #[test]
    fn test_read_text_file_request_round_trip() {
        let req = ReadTextFileRequest {
            session_id: "sess_1".into(),
            path: "/workspace/main.rs".into(),
            line: None,
            limit: Some(100),
            _meta: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["sessionId"], "sess_1");
        assert_eq!(json["path"], "/workspace/main.rs");
        assert_eq!(json["limit"], 100);

        let back: ReadTextFileRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.session_id, "sess_1");
    }

    #[test]
    fn test_read_text_file_response_round_trip() {
        let res = ReadTextFileResponse {
            content: "fn main() {}".into(),
            _meta: None,
        };
        let json = serde_json::to_value(&res).unwrap();
        assert_eq!(json["content"], "fn main() {}");
        let back: ReadTextFileResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back.content, "fn main() {}");
    }

    #[test]
    fn test_write_text_file_request_round_trip() {
        let req = WriteTextFileRequest {
            session_id: "sess_1".into(),
            path: "/workspace/main.rs".into(),
            content: "fn main() {}".into(),
            _meta: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["sessionId"], "sess_1");
        assert_eq!(json["path"], "/workspace/main.rs");
        assert_eq!(json["content"], "fn main() {}");
        let back: WriteTextFileRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.path, "/workspace/main.rs");
    }

    #[test]
    fn test_write_text_file_response_round_trip() {
        let res = WriteTextFileResponse { _meta: None };
        let json = serde_json::to_value(&res).unwrap();
        assert!(json.is_object());
        let back: WriteTextFileResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back, res);
    }

    #[test]
    fn test_create_terminal_request_round_trip() {
        let req = CreateTerminalRequest {
            session_id: "sess_1".into(),
            command: "cargo".into(),
            args: Some(vec!["build".into()]),
            env: Some(vec![EnvVariable {
                name: "RUST_LOG".into(),
                value: "debug".into(),
                _meta: None,
            }]),
            cwd: Some("/workspace".into()),
            output_byte_limit: Some(10000),
            _meta: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["command"], "cargo");
        assert_eq!(json["args"][0], "build");
        assert_eq!(json["env"][0]["name"], "RUST_LOG");
        let back: CreateTerminalRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.command, "cargo");
    }

    #[test]
    fn test_create_terminal_response_round_trip() {
        let res = CreateTerminalResponse {
            terminal_id: "term_1".into(),
            _meta: None,
        };
        let json = serde_json::to_value(&res).unwrap();
        assert_eq!(json["terminalId"], "term_1");
        let back: CreateTerminalResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back.terminal_id, "term_1");
    }

    #[test]
    fn test_terminal_output_response_round_trip() {
        let res = TerminalOutputResponse {
            output: "Compiling...".into(),
            truncated: false,
            exit_status: Some(TerminalExitStatus {
                exit_code: Some(0),
                signal: None,
                _meta: None,
            }),
            _meta: None,
        };
        let json = serde_json::to_value(&res).unwrap();
        assert_eq!(json["output"], "Compiling...");
        assert_eq!(json["truncated"], false);
        assert_eq!(json["exitStatus"]["exitCode"], 0);
        let back: TerminalOutputResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back.output, "Compiling...");
    }

    #[test]
    fn test_request_permission_in_agent_context() {
        let req = super::RequestPermissionRequest {
            session_id: "sess_1".into(),
            tool_call: crate::tool::ToolCallUpdate {
                tool_call_id: "tc_1".into(),
                kind: None,
                status: None,
                title: None,
                content: None,
                locations: None,
                raw_input: None,
                raw_output: None,
                _meta: None,
            },
            options: vec![PermissionOption {
                option_id: "allow_1".into(),
                name: "Allow".into(),
                kind: PermissionOptionKind::AllowOnce,
                _meta: None,
            }],
            _meta: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["sessionId"], "sess_1");
        assert_eq!(json["options"][0]["kind"], "allow_once");
        let back: super::RequestPermissionRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.session_id, "sess_1");
    }

    #[test]
    fn test_wait_for_terminal_exit_response_round_trip() {
        let res = WaitForTerminalExitResponse {
            exit_code: Some(0),
            signal: None,
            _meta: None,
        };
        let json = serde_json::to_value(&res).unwrap();
        assert_eq!(json["exitCode"], 0);
        let back: WaitForTerminalExitResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back.exit_code, Some(0));
    }

    #[test]
    fn test_session_notification_round_trip() {
        let notif = SessionNotification {
            session_id: "sess_1".into(),
            update: crate::session::SessionUpdate::SessionInfoUpdate(
                crate::session::SessionInfoUpdate {
                    title: Some("Updated Title".into()),
                    updated_at: None,
                    _meta: None,
                },
            ),
            _meta: None,
        };
        let json = serde_json::to_value(&notif).unwrap();
        assert_eq!(json["sessionId"], "sess_1");
        assert_eq!(json["update"]["sessionUpdate"], "session_info_update");
        assert_eq!(json["update"]["title"], "Updated Title");
        let back: SessionNotification = serde_json::from_value(json).unwrap();
        assert_eq!(back.session_id, "sess_1");
    }

    #[test]
    fn test_complete_elicitation_notification_round_trip() {
        let notif = CompleteElicitationNotification {
            elicitation_id: "elic_1".into(),
            _meta: None,
        };
        let json = serde_json::to_value(&notif).unwrap();
        assert_eq!(json["elicitationId"], "elic_1");
        let back: CompleteElicitationNotification = serde_json::from_value(json).unwrap();
        assert_eq!(back.elicitation_id, "elic_1");
    }

    #[test]
    fn test_elicitation_mode_form_round_trip() {
        let mode = ElicitationMode::Form(ElicitationFormMode {
            requested_schema: serde_json::json!({"type": "object"}),
            _meta: None,
        });
        let json = serde_json::to_value(&mode).unwrap();
        assert_eq!(json["mode"], "form");
        assert_eq!(json["requestedSchema"]["type"], "object");
        let back: ElicitationMode = serde_json::from_value(json).unwrap();
        assert_eq!(back, mode);
    }

    #[test]
    fn test_unknown_fields_tolerated_on_agent_request() {
        let json = serde_json::json!({
            "sessionId": "sess_1",
            "path": "/workspace/file.txt",
            "unknownField": "ignored"
        });
        let req: ReadTextFileRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.path, "/workspace/file.txt");
    }
}
