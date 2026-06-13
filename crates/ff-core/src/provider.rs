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
    fn kind_deserializes_from_camel_case() {
        let k: ProviderKind = serde_json::from_str("\"ollama\"").unwrap();
        assert_eq!(k, ProviderKind::Ollama);
        let k: ProviderKind = serde_json::from_str("\"candleVllm\"").unwrap();
        assert_eq!(k, ProviderKind::CandleVllm);
    }
}
