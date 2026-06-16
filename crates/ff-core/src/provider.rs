//! User-configurable LLM provider contract. These types ARE the settings IPC
//! surface, exported to TypeScript via `ts-rs`.
//!
//! Phase 1 ships the two local, credential-free backends (candle-vllm + Ollama).
//! Hosted providers and API keys land later behind the same enum; secret material
//! is NEVER part of this contract — keys live in the OS keychain and surface only
//! as the [`ProviderConfig::has_key`] boolean.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Which LLM backend FlowForge talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum ProviderKind {
    /// Local candle-vllm, OpenAI-compatible SSE (FlowForge default).
    #[default]
    CandleVllm,
    /// Local Ollama, native NDJSON `/api/chat`.
    Ollama,
}

impl ProviderKind {
    /// The built-in endpoint used when [`ProviderConfig::base_url`] is `None`.
    pub fn default_base_url(self) -> &'static str {
        match self {
            ProviderKind::CandleVllm => "http://localhost:8000/v1",
            ProviderKind::Ollama => "http://localhost:11434",
        }
    }
}

/// Non-secret, persisted LLM provider settings. Serialized as JSON to the app
/// config dir and round-tripped across IPC to drive the settings panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    /// Endpoint override. `None` = use [`ProviderKind::default_base_url`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base_url: Option<String>,
    /// Model id sent on each chat request.
    pub model: String,
    /// Whether an API key is stored for this provider (OS keychain). Always
    /// `false` in Phase 1 — the field keeps the contract stable for when hosted
    /// providers and secrets land.
    pub has_key: bool,
}

/// FlowForge's out-of-the-box default: local candle-vllm serving Qwen3-4B.
impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            kind: ProviderKind::CandleVllm,
            base_url: None,
            model: "Qwen3-4B-Instruct-2507".to_string(),
            has_key: false,
        }
    }
}

impl ProviderConfig {
    /// The endpoint this config resolves to (override or built-in default).
    pub fn resolved_base_url(&self) -> &str {
        self.base_url
            .as_deref()
            .unwrap_or_else(|| self.kind.default_base_url())
    }
}

/// Stable identifier for a [`ProviderConnection`] within a [`ProviderRegistry`].
/// A short slug (e.g. `"candle-vllm"`, `"ollama"`); generated from the vendor or
/// display name when a new connection is created without one.
pub type ConnectionId = String;

/// One configured provider endpoint. A registry holds several of these so the
/// user can keep, say, a local candle-vLLM and a local Ollama side by side and
/// switch the active one without losing the other's settings.
///
/// Mirrors [`ProviderConfig`] (the legacy singleton) plus the identity fields
/// (`id`, `display_name`, `vendor`) needed to address it in the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ProviderConnection {
    /// Stable slug used to select this connection as active.
    pub id: ConnectionId,
    pub kind: ProviderKind,
    /// Human-facing label shown in the provider picker.
    pub display_name: String,
    /// Optional vendor descriptor (e.g. `"openai"`, `"openrouter"`) for hosted
    /// backends; `None` for the bare local kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub vendor: Option<String>,
    /// Endpoint override. `None` = use [`ProviderKind::default_base_url`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base_url: Option<String>,
    pub model: String,
    /// Whether an API key is stored for this connection (OS keychain).
    pub has_key: bool,
}

impl ProviderConnection {
    /// The endpoint this connection resolves to (override or built-in default).
    pub fn resolved_base_url(&self) -> &str {
        self.base_url
            .as_deref()
            .unwrap_or_else(|| self.kind.default_base_url())
    }
}

/// The full set of configured connections plus a pointer to the active one.
/// Replaces the single [`ProviderConfig`] as the persisted provider contract;
/// switching providers is now non-destructive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ProviderRegistry {
    pub connections: Vec<ProviderConnection>,
    /// Id of the connection [`build_provider`](crate) resolves against. Always
    /// references one of `connections`.
    pub active: ConnectionId,
}

impl ProviderRegistry {
    /// The currently selected connection, or `None` if `active` dangles (which
    /// the registry invariants forbid, but callers should degrade gracefully).
    pub fn active_connection(&self) -> Option<&ProviderConnection> {
        self.connections.iter().find(|c| c.id == self.active)
    }
}

/// FlowForge's out-of-the-box registry: local candle-vLLM (active) plus a ready
/// keyless Ollama, so the user can switch between the two local backends with no
/// setup.
impl Default for ProviderRegistry {
    fn default() -> Self {
        let candle = ProviderConnection {
            id: "candle-vllm".to_string(),
            kind: ProviderKind::CandleVllm,
            display_name: "candle-vLLM".to_string(),
            vendor: None,
            base_url: None,
            model: "Qwen3-4B-Instruct-2507".to_string(),
            has_key: false,
        };
        let ollama = ProviderConnection {
            id: "ollama".to_string(),
            kind: ProviderKind::Ollama,
            display_name: "Ollama".to_string(),
            vendor: None,
            base_url: None,
            model: "llama3.2".to_string(),
            has_key: false,
        };
        Self {
            active: candle.id.clone(),
            connections: vec![candle, ollama],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_local_candle_vllm() {
        let cfg = ProviderConfig::default();
        assert_eq!(cfg.kind, ProviderKind::CandleVllm);
        assert_eq!(cfg.base_url, None);
        assert!(!cfg.has_key);
    }

    #[test]
    fn resolved_base_url_falls_back_to_kind_default() {
        let cfg = ProviderConfig {
            kind: ProviderKind::Ollama,
            base_url: None,
            ..ProviderConfig::default()
        };
        assert_eq!(cfg.resolved_base_url(), "http://localhost:11434");
    }

    #[test]
    fn resolved_base_url_prefers_override() {
        let cfg = ProviderConfig {
            base_url: Some("http://example:9000/v1".into()),
            ..ProviderConfig::default()
        };
        assert_eq!(cfg.resolved_base_url(), "http://example:9000/v1");
    }

    #[test]
    fn config_round_trips_through_json_without_secrets() {
        let cfg = ProviderConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("baseUrl"), "None base_url is skipped");
        assert!(json.contains("hasKey"));
        let back: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn default_registry_has_two_local_connections_candle_active() {
        let reg = ProviderRegistry::default();
        assert_eq!(reg.connections.len(), 2);
        assert_eq!(reg.active, "candle-vllm");
        let active = reg.active_connection().expect("active resolves");
        assert_eq!(active.kind, ProviderKind::CandleVllm);
        assert!(reg
            .connections
            .iter()
            .any(|c| c.kind == ProviderKind::Ollama));
    }

    #[test]
    fn active_connection_is_none_when_pointer_dangles() {
        let reg = ProviderRegistry {
            active: "missing".to_string(),
            ..ProviderRegistry::default()
        };
        assert!(reg.active_connection().is_none());
    }

    #[test]
    fn connection_resolved_base_url_falls_back_to_kind_default() {
        let conn = ProviderConnection {
            id: "ollama".into(),
            kind: ProviderKind::Ollama,
            display_name: "Ollama".into(),
            vendor: None,
            base_url: None,
            model: "llama3.2".into(),
            has_key: false,
        };
        assert_eq!(conn.resolved_base_url(), "http://localhost:11434");
        let overridden = ProviderConnection {
            base_url: Some("http://example:9000".into()),
            ..conn
        };
        assert_eq!(overridden.resolved_base_url(), "http://example:9000");
    }

    #[test]
    fn registry_round_trips_through_json() {
        let reg = ProviderRegistry::default();
        let json = serde_json::to_string(&reg).unwrap();
        assert!(json.contains("\"active\""));
        assert!(json.contains("candle-vllm"));
        let back: ProviderRegistry = serde_json::from_str(&json).unwrap();
        assert_eq!(reg, back);
    }

    #[test]
    fn kind_deserializes_from_camel_case() {
        let k: ProviderKind = serde_json::from_str("\"ollama\"").unwrap();
        assert_eq!(k, ProviderKind::Ollama);
        let k: ProviderKind = serde_json::from_str("\"candleVllm\"").unwrap();
        assert_eq!(k, ProviderKind::CandleVllm);
    }
}
