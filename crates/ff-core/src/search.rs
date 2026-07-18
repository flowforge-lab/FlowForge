//! User-configurable web-search backend contract. Like [`crate::provider`], these
//! types ARE the settings IPC surface, exported to TypeScript via `ts-rs`.
//!
//! The out-of-the-box default is [`SearchBackend::Tavily`], queried keylessly so
//! `web_search` works on a fresh install with zero setup. Its endpoint is a fixed,
//! vetted HTTPS host (not user-supplied), so it is not the SSRF vector that a
//! user-provided [`SearchBackend::SearxNg`] `base_url` is. The hosted
//! [`SearchBackend::Brave`] / [`SearchBackend::OpenAiCompatible`] variants exist so
//! the contract is stable for when API keys land, but the tool refuses them until
//! then. Secret material is NEVER part of this contract — keys will live in the OS
//! keychain and surface only as the [`SearchConfig::has_key`] boolean.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Which web-search provider `web_search` queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum SearchBackend {
    /// Tavily, queried via its keyless JSON API (FlowForge default). Works with no
    /// setup; an optional API key (later phase) raises the rate limit but is never
    /// required. The endpoint is a fixed vetted HTTPS host, so no `base_url`.
    #[default]
    Tavily,
    /// Self-hosted SearXNG, queried via its keyless JSON API. Requires a configured
    /// [`SearchConfig::base_url`] — there is no baked-in public instance (those are
    /// unreliable and an unvetted egress target).
    SearxNg,
    /// Brave Search API. Reserved — needs an API key, so the tool errors until the
    /// OS-keychain work lands (Issue #8).
    Brave,
    /// An OpenAI-compatible search endpoint. Reserved — same key gate as `Brave`.
    OpenAiCompatible,
}

impl SearchBackend {
    /// Every backend variant, for iterating presence/config over all of them.
    pub const ALL: [SearchBackend; 4] = [
        SearchBackend::Tavily,
        SearchBackend::SearxNg,
        SearchBackend::Brave,
        SearchBackend::OpenAiCompatible,
    ];

    /// Whether this backend needs an API key to function. Keyed backends are gated
    /// off until secret storage exists. Tavily accepts an *optional* key (raises the
    /// rate limit) but works keylessly, so it does not require one.
    pub fn requires_key(self) -> bool {
        matches!(self, SearchBackend::Brave | SearchBackend::OpenAiCompatible)
    }
}

/// Non-secret, persisted web-search settings. Serialized as JSON to the app config
/// dir and round-tripped across IPC to drive the settings panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct SearchConfig {
    pub backend: SearchBackend,
    /// Endpoint base URL. Required for SearXNG (e.g. `https://searx.example.org`);
    /// the path `/search?format=json` is appended by the tool. Unused for Tavily.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base_url: Option<String>,
    /// Whether an API key is stored for this backend (OS keychain). Derived from
    /// keychain presence by the host getter on every read (#1010) — never authoritative
    /// on disk, so `skip_deserializing` drops any persisted value and the getter
    /// recomputes it. Still serialized (disk + IPC) so the FE reads the live flag.
    #[serde(skip_deserializing)]
    pub has_key: bool,
}

/// FlowForge's out-of-the-box default: keyless Tavily, which needs no endpoint and
/// no key, so `web_search` works immediately on a fresh install.
impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            backend: SearchBackend::Tavily,
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

/// Whether an API key is stored (OS keychain) for one search backend (#1015). The
/// Settings key panel lists all backends; `present` is boolean-only — the secret
/// value never crosses this contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct SearchSecretPresence {
    pub backend: SearchBackend,
    pub present: bool,
}

#[cfg(test)]
mod tests;
