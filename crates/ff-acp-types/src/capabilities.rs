//! Capability types — the `ClientCapabilities` and `AgentCapabilities` objects
//! exchanged during `initialize`.

use serde::{Deserialize, Serialize};

use crate::rpc::Meta;

// ---------------------------------------------------------------------------
// Client capabilities
// ---------------------------------------------------------------------------

/// Capabilities the client (editor/IDE) advertises during initialization.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    #[serde(default)]
    pub fs: FileSystemCapabilities,
    #[serde(default)]
    pub terminal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<ClientSessionCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elicitation: Option<ElicitationCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Filesystem capabilities the client serves.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSystemCapabilities {
    #[serde(default)]
    pub read_text_file: bool,
    #[serde(default)]
    pub write_text_file: bool,
}

/// Session-management capabilities the client serves.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSessionCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_options: Option<SessionConfigOptionsCapabilities>,
}

/// Whether the client supports session config options.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigOptionsCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boolean: Option<BooleanConfigOptionCapabilities>,
}

/// Capabilities for boolean config options.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BooleanConfigOptionCapabilities {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_ids: Vec<String>,
}

/// Elicitation capabilities the client serves.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<ElicitationFormCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<ElicitationUrlCapabilities>,
}

/// Form-elicitation capabilities.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationFormCapabilities {
    #[serde(default)]
    pub enabled: bool,
}

/// URL-elicitation capabilities.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationUrlCapabilities {
    #[serde(default)]
    pub enabled: bool,
}

// ---------------------------------------------------------------------------
// Agent capabilities
// ---------------------------------------------------------------------------

/// Capabilities the agent advertises during initialization.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    pub prompt_capabilities: PromptCapabilities,
    #[serde(default)]
    pub mcp_capabilities: McpCapabilities,
    #[serde(default)]
    pub session_capabilities: SessionCapabilities,
    #[serde(default)]
    pub auth: AgentAuthCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Prompt capabilities the agent supports.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    #[serde(default)]
    pub image: bool,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub embedded_context: bool,
}

/// MCP-transport capabilities the agent supports.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCapabilities {
    #[serde(default)]
    pub http: bool,
    #[serde(default)]
    pub sse: bool,
}

/// Which session-management verbs the agent supports. Each sub-capability is
/// an empty tag object; its presence on the wire signals support.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<CapabilitiesObj>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<CapabilitiesObj>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<CapabilitiesObj>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<CapabilitiesObj>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close: Option<CapabilitiesObj>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Authentication capabilities the agent supports.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logout: Option<LogoutCapabilities>,
}

/// Logout capabilities.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogoutCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// An empty object whose presence on the wire signals a capability is supported.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesObj {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_capabilities_default() {
        let caps = ClientCapabilities::default();
        let json = serde_json::to_value(&caps).unwrap();
        assert_eq!(json["fs"]["readTextFile"], false);
        assert_eq!(json["fs"]["writeTextFile"], false);
        assert_eq!(json["terminal"], false);
        // session and elicitation are optional, should be absent
        assert!(json.get("session").is_none());
        assert!(json.get("elicitation").is_none());
    }

    #[test]
    fn test_client_capabilities_round_trip() {
        let caps = ClientCapabilities {
            fs: FileSystemCapabilities {
                read_text_file: true,
                write_text_file: true,
            },
            terminal: true,
            session: Some(ClientSessionCapabilities {
                config_options: Some(SessionConfigOptionsCapabilities {
                    boolean: Some(BooleanConfigOptionCapabilities {
                        config_ids: vec!["opt_1".into(), "opt_2".into()],
                    }),
                }),
            }),
            elicitation: Some(ElicitationCapabilities {
                form: Some(ElicitationFormCapabilities { enabled: true }),
                url: Some(ElicitationUrlCapabilities { enabled: false }),
            }),
            _meta: None,
        };
        let json = serde_json::to_value(&caps).unwrap();
        assert_eq!(json["fs"]["readTextFile"], true);
        assert_eq!(json["fs"]["writeTextFile"], true);
        assert_eq!(json["terminal"], true);
        assert!(json["session"]["configOptions"]["boolean"]["configIds"][0].is_string());
        assert_eq!(json["elicitation"]["form"]["enabled"], true);

        let back: ClientCapabilities = serde_json::from_value(json).unwrap();
        assert_eq!(back, caps);
    }

    #[test]
    fn test_agent_capabilities_default() {
        let caps = AgentCapabilities::default();
        let json = serde_json::to_value(&caps).unwrap();
        assert_eq!(json["loadSession"], false);
        assert_eq!(json["promptCapabilities"]["image"], false);
        assert_eq!(json["sessionCapabilities"]["list"], serde_json::Value::Null);
    }

    #[test]
    fn test_agent_capabilities_round_trip() {
        let caps = AgentCapabilities {
            load_session: true,
            prompt_capabilities: PromptCapabilities {
                image: true,
                audio: false,
                embedded_context: true,
            },
            mcp_capabilities: McpCapabilities {
                http: true,
                sse: false,
            },
            session_capabilities: SessionCapabilities {
                list: Some(CapabilitiesObj { _meta: None }),
                delete: Some(CapabilitiesObj { _meta: None }),
                additional_directories: None,
                resume: None,
                close: Some(CapabilitiesObj { _meta: None }),
                _meta: None,
            },
            auth: AgentAuthCapabilities {
                logout: Some(LogoutCapabilities { _meta: None }),
            },
            _meta: None,
        };
        let json = serde_json::to_value(&caps).unwrap();
        assert_eq!(json["loadSession"], true);
        assert!(json["sessionCapabilities"]["list"].is_object());
        assert!(json["sessionCapabilities"]["delete"].is_object());
        assert_eq!(
            json["sessionCapabilities"]["additionalDirectories"],
            serde_json::Value::Null
        );

        let back: AgentCapabilities = serde_json::from_value(json).unwrap();
        assert_eq!(back, caps);
    }

    #[test]
    fn test_session_capabilities_empty_vs_present() {
        // CapabilitiesObj being Some means the tag is present on the wire
        let caps = SessionCapabilities {
            list: Some(CapabilitiesObj::default()),
            delete: None,
            additional_directories: None,
            resume: None,
            close: None,
            _meta: None,
        };
        let json = serde_json::to_value(&caps).unwrap();
        assert!(json["list"].is_object());
        assert_eq!(json["delete"], serde_json::Value::Null);
    }

    #[test]
    fn test_unknown_fields_tolerated_on_client_capabilities() {
        let json = serde_json::json!({
            "unknown": true,
            "fs": { "readTextFile": true, "writeTextFile": false }
        });
        let caps: ClientCapabilities = serde_json::from_value(json).unwrap();
        assert!(caps.fs.read_text_file);
        assert!(!caps.fs.write_text_file);
    }

    #[test]
    fn test_boolean_config_option_capabilities() {
        let caps = BooleanConfigOptionCapabilities {
            config_ids: vec!["mode".into(), "theme".into()],
        };
        let json = serde_json::to_value(&caps).unwrap();
        assert_eq!(json["configIds"][0], "mode");
        let back: BooleanConfigOptionCapabilities = serde_json::from_value(json).unwrap();
        assert_eq!(back.config_ids, vec!["mode", "theme"]);
    }
}
