//! Client→Agent method payloads and their corresponding response payloads.
//!
//! These 12 methods are the requests the *client* (editor/IDE) sends to the
//! *agent* (FlowForge). Our ACP integration must handle them.

use serde::{Deserialize, Serialize};

use crate::capabilities::{AgentCapabilities, ClientCapabilities};
use crate::content::ContentBlock;
use crate::rpc::Meta;
use crate::session::{McpServer, SessionConfigOption, SessionId, SessionModeId, SessionModeState};

// ---------------------------------------------------------------------------
// initialize
// ---------------------------------------------------------------------------

/// Request: `initialize`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    pub protocol_version: u16,
    #[serde(default)]
    pub client_capabilities: ClientCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_info: Option<Implementation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `initialize`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: u16,
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
    #[serde(default)]
    pub auth_methods: Vec<AuthMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_info: Option<Implementation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// authenticate
// ---------------------------------------------------------------------------

/// Request: `authenticate`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticateRequest {
    pub method_id: AuthMethodId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `authenticate`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticateResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// A unique authentication method identifier.
pub type AuthMethodId = String;

/// An authentication method description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthMethod {
    pub method_id: AuthMethodId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// logout
// ---------------------------------------------------------------------------

/// Request: `logout`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogoutRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `logout`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogoutResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// session/new
// ---------------------------------------------------------------------------

/// Request: `session/new`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionRequest {
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Vec<String>>,
    pub mcp_servers: Vec<McpServer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `session/new`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResponse {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<SessionModeState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_options: Option<Vec<SessionConfigOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// session/load
// ---------------------------------------------------------------------------

/// Request: `session/load`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSessionRequest {
    pub session_id: SessionId,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Vec<String>>,
    pub mcp_servers: Vec<McpServer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `session/load`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSessionResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<SessionModeState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_options: Option<Vec<SessionConfigOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// session/list
// ---------------------------------------------------------------------------

/// Request: `session/list`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSessionsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `session/list`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSessionsResponse {
    pub sessions: Vec<crate::session::SessionInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// session/delete
// ---------------------------------------------------------------------------

/// Request: `session/delete`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSessionRequest {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `session/delete`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSessionResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// session/resume
// ---------------------------------------------------------------------------

/// Request: `session/resume`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeSessionRequest {
    pub session_id: SessionId,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Vec<String>>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `session/resume`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeSessionResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<SessionModeState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_options: Option<Vec<SessionConfigOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// session/close
// ---------------------------------------------------------------------------

/// Request: `session/close`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseSessionRequest {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `session/close`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseSessionResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// session/set_mode
// ---------------------------------------------------------------------------

/// Request: `session/set_mode`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSessionModeRequest {
    pub session_id: SessionId,
    pub mode_id: SessionModeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `session/set_mode`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSessionModeResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// session/set_config_option
// ---------------------------------------------------------------------------

/// Request: `session/set_config_option`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSessionConfigOptionRequest {
    pub session_id: SessionId,
    pub config_id: SessionConfigId,
    pub value: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `session/set_config_option`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSessionConfigOptionResponse {
    pub config_options: Vec<SessionConfigOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// session/prompt
// ---------------------------------------------------------------------------

/// Request: `session/prompt`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptRequest {
    pub session_id: SessionId,
    pub prompt: Vec<PromptMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// A single prompt message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptMessage {
    pub role: crate::content::Role,
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Response: `session/prompt`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    pub stop_reason: StopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Why a prompt turn stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

// ---------------------------------------------------------------------------
// Notification: session/cancel (client → agent)
// ---------------------------------------------------------------------------

/// Notification: `session/cancel`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelNotification {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// Shared: Implementation
// ---------------------------------------------------------------------------

/// Information about the client or agent implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Implementation {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

// ---------------------------------------------------------------------------
// Session ID re-export
// ---------------------------------------------------------------------------

pub use crate::session::SessionConfigId;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initialize_request_round_trip() {
        let req = InitializeRequest {
            protocol_version: 1,
            client_capabilities: ClientCapabilities::default(),
            client_info: Some(Implementation {
                name: "TestClient".into(),
                version: "1.0.0".into(),
                _meta: None,
            }),
            _meta: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["protocolVersion"], 1);
        assert_eq!(json["clientInfo"]["name"], "TestClient");
        // ClientCapabilities default should serialize
        assert!(json["clientCapabilities"].is_object());

        let back: InitializeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.protocol_version, 1);
    }

    #[test]
    fn test_initialize_response_round_trip() {
        let res = InitializeResponse {
            protocol_version: 1,
            agent_capabilities: AgentCapabilities::default(),
            auth_methods: vec![],
            agent_info: None,
            _meta: None,
        };
        let json = serde_json::to_value(&res).unwrap();
        assert_eq!(json["protocolVersion"], 1);
        assert!(json["agentCapabilities"].is_object());
        let back: InitializeResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back.protocol_version, 1);
    }

    #[test]
    fn test_new_session_request_round_trip() {
        let req = NewSessionRequest {
            cwd: "/workspace".into(),
            additional_directories: None,
            mcp_servers: vec![crate::session::McpServer {
                id: "local".into(),
                url: "http://localhost:8080".into(),
                headers: Default::default(),
                _meta: None,
            }],
            _meta: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["cwd"], "/workspace");
        assert_eq!(json["mcpServers"][0]["id"], "local");

        let back: NewSessionRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.cwd, "/workspace");
    }

    #[test]
    fn test_new_session_response_round_trip() {
        let res = NewSessionResponse {
            session_id: "sess_1".into(),
            modes: None,
            config_options: None,
            _meta: None,
        };
        let json = serde_json::to_value(&res).unwrap();
        assert_eq!(json["sessionId"], "sess_1");
        let back: NewSessionResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back.session_id, "sess_1");
    }

    #[test]
    fn test_prompt_request_round_trip() {
        let req = PromptRequest {
            session_id: "sess_1".into(),
            prompt: vec![PromptMessage {
                role: crate::content::Role::User,
                content: vec![crate::content::ContentBlock::Text(
                    crate::content::TextContent {
                        annotations: None,
                        text: "Hello".into(),
                        _meta: None,
                    },
                )],
                _meta: None,
            }],
            _meta: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["sessionId"], "sess_1");
        assert_eq!(json["prompt"][0]["role"], "user");
        assert_eq!(json["prompt"][0]["content"][0]["type"], "text");
        assert_eq!(json["prompt"][0]["content"][0]["text"], "Hello");

        let back: PromptRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.session_id, "sess_1");
    }

    #[test]
    fn test_prompt_response_round_trip() {
        for reason in [
            StopReason::EndTurn,
            StopReason::MaxTokens,
            StopReason::MaxTurnRequests,
            StopReason::Refusal,
            StopReason::Cancelled,
        ] {
            let res = PromptResponse {
                stop_reason: reason,
                _meta: None,
            };
            let json = serde_json::to_value(&res).unwrap();
            let back: PromptResponse = serde_json::from_value(json).unwrap();
            assert_eq!(back, res);
        }
    }

    #[test]
    fn test_stop_reason_serialization() {
        assert_eq!(
            serde_json::to_value(StopReason::EndTurn).unwrap(),
            "end_turn"
        );
        assert_eq!(
            serde_json::to_value(StopReason::MaxTurnRequests).unwrap(),
            "max_turn_requests"
        );
    }

    #[test]
    fn test_cancel_notification_round_trip() {
        let notif = CancelNotification {
            session_id: "sess_1".into(),
            _meta: None,
        };
        let json = serde_json::to_value(&notif).unwrap();
        assert_eq!(json["sessionId"], "sess_1");
        let back: CancelNotification = serde_json::from_value(json).unwrap();
        assert_eq!(back.session_id, "sess_1");
    }

    #[test]
    fn test_set_session_mode_request_round_trip() {
        let req = SetSessionModeRequest {
            session_id: "sess_1".into(),
            mode_id: "mode_plan".into(),
            _meta: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["sessionId"], "sess_1");
        assert_eq!(json["modeId"], "mode_plan");
        let back: SetSessionModeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.session_id, "sess_1");
    }

    #[test]
    fn test_auth_method_round_trip() {
        let method = AuthMethod {
            method_id: "github_oauth".into(),
            name: "GitHub OAuth".into(),
            description: Some("Log in with GitHub".into()),
            _meta: None,
        };
        let json = serde_json::to_value(&method).unwrap();
        assert_eq!(json["methodId"], "github_oauth");
        let back: AuthMethod = serde_json::from_value(json).unwrap();
        assert_eq!(back, method);
    }

    #[test]
    fn test_unknown_fields_tolerated_on_initialize_request() {
        let json = serde_json::json!({
            "protocolVersion": 1,
            "unknownField": "ignored"
        });
        let req: InitializeRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.protocol_version, 1);
    }
}
