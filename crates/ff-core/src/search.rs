//! User-configurable web-search backend contract. Like [`crate::provider`], these
//! types ARE the settings IPC surface, exported to TypeScript via `ts-rs`.
//!
//! This PR wires the keyless, self-hosted [`SearchBackend::SearxNg`] backend end to
//! end. The hosted [`SearchBackend::Brave`] / [`SearchBackend::OpenAiCompatible`]
//! variants exist so the contract is stable for when API keys land, but the tool
//! refuses them until then. Secret material is NEVER part of this contract — keys
//! will live in the OS keychain and surface only as the [`SearchConfig::has_key`]
//! boolean (always `false` for now).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Which web-search provider `web_search` queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum SearchBackend {
    /// Self-hosted SearXNG, queried via its keyless JSON API (FlowForge default).
    /// Requires a configured [`SearchConfig::base_url`] — there is no baked-in
    /// public instance (those are unreliable and an unvetted egress target).
    #[default]
    SearxNg,
    /// Brave Search API. Reserved — needs an API key, so the tool errors until the
    /// OS-keychain work lands (Issue #8).
    Brave,
    /// An OpenAI-compatible search endpoint. Reserved — same key gate as `Brave`.
    OpenAiCompatible,
}

impl SearchBackend {
    /// Whether this backend needs an API key to function. Keyed backends are gated
    /// off until secret storage exists.
    pub fn requires_key(self) -> bool {
        matches!(self, SearchBackend::Brave | SearchBackend::OpenAiCompatible)
    }
}

/// Non-secret, persisted web-search settings. Serialized as JSON to the app config
/// dir and round-tripped across IPC to drive the (future) settings panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct SearchConfig {
    pub backend: SearchBackend,
    /// Endpoint base URL. Required for SearXNG (e.g. `https://searx.example.org`);
    /// the path `/search?format=json` is appended by the tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base_url: Option<String>,
    /// Whether an API key is stored for this backend (OS keychain). Always `false`
    /// for now — the field keeps the contract stable for when secrets land.
    pub has_key: bool,
}

/// FlowForge's out-of-the-box default: SearXNG with no endpoint set yet (the tool
/// asks the user to configure one rather than hitting an unvetted public instance).
impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            backend: SearchBackend::SearxNg,
            base_url: None,
            has_key: false,
        }
    }
}

impl SearchConfig {
    /// The configured endpoint, if any. `None` means the user has not set one.
    pub fn resolved_base_url(&self) -> Option<&str> {
        self.base_url.as_deref().filter(|u| !u.trim().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_searxng_without_endpoint() {
        let cfg = SearchConfig::default();
        assert_eq!(cfg.backend, SearchBackend::SearxNg);
        assert_eq!(cfg.base_url, None);
        assert!(!cfg.has_key);
        assert_eq!(cfg.resolved_base_url(), None);
    }

    #[test]
    fn resolved_base_url_ignores_blank_override() {
        let cfg = SearchConfig {
            base_url: Some("   ".into()),
            ..SearchConfig::default()
        };
        assert_eq!(cfg.resolved_base_url(), None);
    }

    #[test]
    fn resolved_base_url_returns_configured_endpoint() {
        let cfg = SearchConfig {
            base_url: Some("https://searx.example.org".into()),
            ..SearchConfig::default()
        };
        assert_eq!(cfg.resolved_base_url(), Some("https://searx.example.org"));
    }

    #[test]
    fn only_hosted_backends_require_a_key() {
        assert!(!SearchBackend::SearxNg.requires_key());
        assert!(SearchBackend::Brave.requires_key());
        assert!(SearchBackend::OpenAiCompatible.requires_key());
    }

    #[test]
    fn config_round_trips_through_json_without_secrets() {
        let cfg = SearchConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("baseUrl"), "None base_url is skipped");
        assert!(json.contains("hasKey"));
        assert!(
            !json.contains("key\":\""),
            "no secret material in the contract"
        );
        let back: SearchConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn backend_deserializes_from_camel_case() {
        let b: SearchBackend = serde_json::from_str("\"searxNg\"").unwrap();
        assert_eq!(b, SearchBackend::SearxNg);
        let b: SearchBackend = serde_json::from_str("\"brave\"").unwrap();
        assert_eq!(b, SearchBackend::Brave);
        let b: SearchBackend = serde_json::from_str("\"openAiCompatible\"").unwrap();
        assert_eq!(b, SearchBackend::OpenAiCompatible);
    }
}
