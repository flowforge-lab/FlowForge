//! ACP agent configuration types.

use std::collections::BTreeMap;

use ff_core::ReconcilableConfig;
use serde::{Deserialize, Serialize};

/// One external ACP agent definition, mirroring [`ff_core::McpServerConfig`]
/// but without MCP-specific fields (scope, reaches_network, defer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpAgentConfig {
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub disabled: bool,
}

impl ReconcilableConfig for AcpAgentConfig {
    fn id(&self) -> &str {
        &self.id
    }
    fn disabled(&self) -> bool {
        self.disabled
    }
}

impl From<AcpAgentConfig> for agent_client_protocol::AcpAgentConfig {
    fn from(cfg: AcpAgentConfig) -> Self {
        let mut acp = agent_client_protocol::AcpAgentConfig::new(cfg.command);
        for arg in cfg.args {
            acp = acp.arg(arg);
        }
        for (k, v) in cfg.env {
            acp = acp.env(k, v);
        }
        acp
    }
}

/// Lifecycle state of a supervised ACP agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcpServerState {
    Starting,
    Running,
    Failed,
    Disabled,
}

/// A snapshot of one agent's status for the UI / supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpServerStatus {
    pub id: String,
    pub state: AcpServerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ff_core::reconcile;

    fn cfg(id: &str, command: &str) -> AcpAgentConfig {
        AcpAgentConfig {
            id: id.into(),
            command: command.into(),
            args: vec![],
            env: BTreeMap::new(),
            disabled: false,
        }
    }

    #[test]
    fn reconcile_works_for_acp_config() {
        let desired = vec![cfg("a", "x")];
        let running = vec![];
        let actions = reconcile(&desired, &running);
        assert_eq!(
            actions,
            vec![ff_core::ReconcileAction::Start(cfg("a", "x"))]
        );
    }

    #[test]
    fn acp_agent_config_converts_to_sdk() {
        let cfg = AcpAgentConfig {
            id: "test".into(),
            command: "python".into(),
            args: vec!["agent.py".into(), "--verbose".into()],
            env: BTreeMap::from([("RUST_LOG".into(), "debug".into())]),
            disabled: false,
        };
        let sdk: agent_client_protocol::AcpAgentConfig = cfg.into();
        assert_eq!(sdk.command(), std::path::Path::new("python"));
        assert_eq!(sdk.arguments(), &["agent.py", "--verbose"]);
        assert_eq!(
            sdk.environment().get("RUST_LOG").map(String::as_str),
            Some("debug")
        );
    }

    #[test]
    fn acp_server_state_serializes_lowercase() {
        let json = serde_json::to_value(AcpServerState::Starting).unwrap();
        assert_eq!(json, serde_json::json!("starting"));
        let json = serde_json::to_value(AcpServerState::Running).unwrap();
        assert_eq!(json, serde_json::json!("running"));
        let json = serde_json::to_value(AcpServerState::Failed).unwrap();
        assert_eq!(json, serde_json::json!("failed"));
        let json = serde_json::to_value(AcpServerState::Disabled).unwrap();
        assert_eq!(json, serde_json::json!("disabled"));
    }

    #[test]
    fn acp_server_status_round_trips() {
        let status = AcpServerStatus {
            id: "agent-1".into(),
            state: AcpServerState::Failed,
            last_error: Some("connection refused".into()),
        };
        let json = serde_json::to_value(&status).unwrap();
        let back: AcpServerStatus = serde_json::from_value(json).unwrap();
        assert_eq!(status, back);

        // Minimal (no error).
        let minimal = AcpServerStatus {
            id: "agent-2".into(),
            state: AcpServerState::Running,
            last_error: None,
        };
        let json = serde_json::to_value(&minimal).unwrap();
        let back: AcpServerStatus = serde_json::from_value(json).unwrap();
        assert_eq!(minimal, back);
    }

    #[test]
    fn acp_agent_config_round_trips() {
        let cfg = cfg("agent-1", "/usr/bin/agent");
        let json = serde_json::to_value(&cfg).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "id": "agent-1",
                "command": "/usr/bin/agent",
                "args": [],
                "env": {},
                "disabled": false
            })
        );
        let back: AcpAgentConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg, back);
    }
}
