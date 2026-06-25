//! User-configurable LLM provider contract. These types ARE the settings IPC
//! surface, exported to TypeScript via `ts-rs`.
//!
//! Phase 1 ships the two local, credential-free backends (candle-vllm + Ollama).
//! Hosted providers and API keys land later behind the same enum; secret material
//! is NEVER part of this contract — keys live in the OS keychain and surface only
//! as the [`ProviderConfig::has_key`] boolean.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

fn default_thinking() -> bool {
    true
}

/// User-facing reasoning *depth* dial (#394/#395), mirroring the frontend
/// `Effort` (apps/desktop/src/store/model-config.ts: `low | medium | high`,
/// default `medium`). Orthogonal to the on/off gate (`ChatRequest::thinking`):
/// effort only matters when thinking is on, where it picks the reasoning token
/// budget every supported backend honors -- the SiliconFlow gateway
/// (`thinking_budget`, verified #394 across GLM-5.2 / Kimi-K2.7 / DeepSeek-V4-Pro),
/// Bedrock Converse and native Anthropic extended thinking (`budget_tokens`).
///
/// Lives in `ff-core` (not `ff-llm`) so it can be both a field on the
/// [`ProviderConnection`] settings contract (exported to TS) and consumed by the
/// providers in `ff-llm`, which re-export it -- `ff-llm` depends on `ff-core`, so
/// the type cannot live the other way round without a dependency cycle (#395).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum ReasoningEffort {
    /// Shallow reasoning -- a tight budget that still bounds runaway cost. 1024
    /// is the Anthropic/Bedrock documented minimum and recommended starting point.
    Low,
    /// The default. Caps the chain-of-thought well above every model's natural
    /// reasoning length (194-527 tokens for SiliconFlow GLM/Kimi/DeepSeek, #394),
    /// so it only bites runaway agentic loops -- which is what burned tokens.
    #[default]
    Medium,
    /// Deepest reasoning. A hard 8192 cap rather than uncapped, keeping #394's
    /// cost guard intact even at the top of the dial. On adaptive-effort models
    /// (Opus 4.6+, which deprecated `budget_tokens`) this maps to
    /// `output_config.effort = "high"` instead (see [`Self::effort_str`]).
    High,
}

impl ReasoningEffort {
    /// Reasoning/thinking token budget for this effort level. Uniform across
    /// every supported backend (SiliconFlow `thinking_budget`, Bedrock Converse
    /// and native Anthropic `budget_tokens`). All values are >= the 1024
    /// Anthropic/Bedrock minimum and <= the 32k model maximum.
    pub fn budget_tokens(self) -> u32 {
        match self {
            ReasoningEffort::Low => 1024,
            ReasoningEffort::Medium => 4096,
            ReasoningEffort::High => 8192,
        }
    }

    /// Effort label for adaptive-thinking models (Opus 4.6+, Sonnet 4.6+), which
    /// deprecated `budget_tokens` in favor of `output_config.effort`. We never
    /// emit `"max"` (Opus-4.6-only), so the dial maps cleanly onto the three
    /// portable levels.
    pub fn effort_str(self) -> &'static str {
        match self {
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
        }
    }
}

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
    /// AWS Bedrock. Hosted; credentials resolved backend-side (AWS profile, IAM
    /// keys, or a bearer API key) and the endpoint derived from the region.
    Bedrock,
    /// Hosted OpenAI, or any OpenAI-compatible hosted gateway (OpenRouter,
    /// Azure OpenAI, Together). Bearer API key from the OS keychain; speaks the
    /// same OpenAI-compatible `/chat/completions` + `/models` wire as the local
    /// candle-vLLM kind. The wire tag is pinned to `openai` (not the camelCase
    /// default `openAi`) so it matches `slug()` and the `vendor` descriptor.
    #[serde(rename = "openai")]
    OpenAi,
    /// SiliconFlow. Hosted, OpenAI-compatible (`.com` global / `.cn` China); a
    /// bearer API key in the keychain. Served by the OpenAI-compatible provider.
    SiliconFlow,
}

impl ProviderKind {
    /// The built-in endpoint used when [`ProviderConfig::base_url`] is `None`.
    pub fn default_base_url(self) -> &'static str {
        match self {
            ProviderKind::CandleVllm => "http://localhost:8000/v1",
            ProviderKind::Ollama => "http://localhost:11434",
            // Bedrock has no fixed endpoint — the provider derives
            // `bedrock-runtime.<region>.amazonaws.com` from the connection region.
            // This default is only a placeholder for the rare base_url-less probe.
            ProviderKind::Bedrock => "https://bedrock-runtime.us-east-1.amazonaws.com",
            ProviderKind::OpenAi => "https://api.openai.com/v1",
            // Global endpoint; `.cn` users override base_url with
            // https://api.siliconflow.cn/v1.
            ProviderKind::SiliconFlow => "https://api.siliconflow.com/v1",
        }
    }

    /// Stable slug for this kind, used as the fallback [`ConnectionId`] when a
    /// connection is created with no vendor or display name.
    pub fn slug(self) -> &'static str {
        match self {
            ProviderKind::CandleVllm => "candle-vllm",
            ProviderKind::Ollama => "ollama",
            ProviderKind::Bedrock => "bedrock",
            ProviderKind::OpenAi => "openai",
            ProviderKind::SiliconFlow => "siliconflow",
        }
    }
}

/// How a Bedrock connection authenticates. Every mode resolves credentials
/// backend-side; secret material (secret access key, session token, bearer API
/// key) lives in the OS keychain and never on this contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum BedrockAuth {
    /// Resolve by precedence from whatever is configured: API key > profile >
    /// IAM keys (#320). The default for new connections. See [`BedrockAuth::resolve_auto`].
    Auto,
    /// A named AWS profile from `~/.aws/config` (cred chain).
    Profile,
    /// Static IAM keys: access key id (non-secret, on the connection) plus a
    /// secret access key and optional session token (both in the keychain).
    IamKeys,
    /// A Bedrock bearer API key (in the keychain); skips SigV4.
    ApiKey,
}

impl BedrockAuth {
    /// The concrete auth mode [`BedrockAuth::Auto`] picks from what a connection has
    /// configured, in precedence order: a bearer **API key** wins over SigV4 (the
    /// universal Bedrock-client convention), then a **profile**, then static **IAM
    /// keys**. Profile-over-IAM is deliberate: static IAM keys are long-lived (AWS
    /// discourages them), whereas a profile is typically SSO/role-backed. With
    /// nothing configured it falls back to [`BedrockAuth::Profile`] so the probe
    /// surfaces the real, actionable auth failure rather than a silent no-op.
    pub fn resolve_auto(has_api_key: bool, has_profile: bool, has_iam_keys: bool) -> BedrockAuth {
        if has_api_key {
            BedrockAuth::ApiKey
        } else if has_profile {
            BedrockAuth::Profile
        } else if has_iam_keys {
            BedrockAuth::IamKeys
        } else {
            BedrockAuth::Profile
        }
    }
}

/// A piece of secret material stored in the OS keychain for a connection. Used as
/// the discriminator on the write-only `set_provider_secret` / `clear_provider_secret`
/// commands; the value itself is never part of any contract or command response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum SecretKind {
    /// OpenAI / Bedrock bearer API key.
    ApiKey,
    /// AWS IAM secret access key.
    SecretAccessKey,
    /// AWS session token (temporary credentials).
    SessionToken,
}

impl SecretKind {
    /// Every secret kind, for recomputing a connection's `has_key` flag.
    pub const ALL: [SecretKind; 3] = [
        SecretKind::ApiKey,
        SecretKind::SecretAccessKey,
        SecretKind::SessionToken,
    ];

    /// Stable slug used in the keychain account name (`<connectionId>:<slug>`).
    pub fn slug(self) -> &'static str {
        match self {
            SecretKind::ApiKey => "apiKey",
            SecretKind::SecretAccessKey => "secretAccessKey",
            SecretKind::SessionToken => "sessionToken",
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
    /// When true, request and surface model reasoning/thinking streams (#181).
    #[serde(default = "default_thinking")]
    pub thinking: bool,
    /// Reasoning *depth* dial (#395). Only bites when `thinking` is on; picks the
    /// per-backend reasoning token budget in the host's `build_provider`. Mirrors
    /// [`ProviderConnection::reasoning_effort`]. `#[serde(default)]` keeps legacy
    /// `provider.json` files (and the CLI's) loading as [`ReasoningEffort::Medium`].
    #[serde(default)]
    pub reasoning_effort: ReasoningEffort,
}

/// FlowForge's out-of-the-box default: local candle-vllm serving Qwen3-4B.
impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            kind: ProviderKind::CandleVllm,
            base_url: None,
            model: "Qwen3-4B-Instruct-2507".to_string(),
            has_key: false,
            thinking: true,
            reasoning_effort: ReasoningEffort::default(),
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
    /// When true, request and surface model reasoning/thinking streams (#181).
    #[serde(default = "default_thinking")]
    pub thinking: bool,
    /// Reasoning *depth* dial for this connection (#395). Only bites when
    /// `thinking` is on; picks the per-backend reasoning token budget in
    /// `build_provider`. `#[serde(default)]` keeps pre-#395 registries loading as
    /// [`ReasoningEffort::Medium`].
    #[serde(default)]
    pub reasoning_effort: ReasoningEffort,
    /// Whether this connection's model can accept image/document attachments
    /// (multimodal, #332). Drives the composer attach-button gate (FE-4) and the
    /// backend safety strip (a non-vision connection never emits a raw attachments
    /// field). Defaults false; opted in per connection in settings.
    #[serde(default)]
    pub supports_vision: bool,
    /// Whether this connection's model can accept *document* attachments
    /// (PDF/DOCX/CSV/…, #504). Distinct from [`supports_vision`]: a Bedrock
    /// text-only model reads documents but has no vision, and an OpenAI vision
    /// model cannot take documents. Drives the composer doc attach-gate and the
    /// backend `Document` strip independently of vision. Defaults false.
    #[serde(default)]
    pub supports_documents: bool,
    /// AWS region for a Bedrock connection (e.g. `"us-east-1"`); the provider
    /// derives `bedrock-runtime.<region>.amazonaws.com` from it. `None` for
    /// non-Bedrock kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub region: Option<String>,
    /// Bedrock credential mode. `None` for non-Bedrock kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auth_mode: Option<BedrockAuth>,
    /// AWS named profile for [`BedrockAuth::Profile`] (reads `~/.aws/config`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub aws_profile: Option<String>,
    /// AWS access key id for [`BedrockAuth::IamKeys`]. A non-secret identifier; the
    /// paired secret access key and session token live in the keychain, never here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub access_key_id: Option<String>,
}

impl ProviderConnection {
    /// The endpoint this connection resolves to (override or built-in default).
    pub fn resolved_base_url(&self) -> &str {
        self.base_url
            .as_deref()
            .unwrap_or_else(|| self.kind.default_base_url())
    }
}

/// Whether `(kind, model)` is known to accept image/document attachments. Pure
/// capability lookup, conservative by design: returning `false` only means we
/// don't know -- the FE gate (#408 / FE-4) and provider safety strip both fail
/// closed on unknowns, so a wrong `false` only forces the user to rely on an
/// explicit override.
///
/// Keep the map narrow. Adding a model here un-gates attachments app-wide for
/// every connection on that model, so favor known-good families over loose
/// substring matches.
pub fn model_supports_vision(kind: ProviderKind, model: &str) -> bool {
    // Data-driven (#466): the per-provider vision families now live in
    // `model-specs.default.json` as rules carrying `provider` + `supports_vision`,
    // and the provider-scoped, fail-closed lookup lives in `model_specs`. Keeping
    // this thin wrapper preserves the call sites (`normalize_capabilities`, upsert)
    // and the public signature.
    crate::model_specs::supports_vision_in(crate::model_specs::bundled_rules(), kind, model)
}

/// Whether `(kind, model)` can accept *document* attachments (PDF/DOCX/CSV/…,
/// #504). Provider-scoped: only Bedrock's Converse API carries a portable
/// `DocumentBlock`; the OpenAI-compatible and Ollama wire formats have no
/// document block (#338), so they fail closed here. The `model` argument is
/// reserved for a future data-driven split (mirrors the #466 vision migration)
/// once a non-Bedrock provider gains native document support.
pub fn model_supports_documents(kind: ProviderKind, _model: &str) -> bool {
    matches!(kind, ProviderKind::Bedrock)
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

    /// Derive a stable slug id for a new connection: `vendor || display_name ||
    /// kind`, lowercased, with non-alphanumeric runs collapsed to `-` and the
    /// ends trimmed; deduped against existing ids with a `-N` suffix (N from 2).
    /// Mirrors the mock's client-side rule so an id is identical offline and live.
    pub fn derive_id(&self, conn: &ProviderConnection) -> ConnectionId {
        let source = conn
            .vendor
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| Some(conn.display_name.as_str()).filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(|| conn.kind.slug());
        let base = slugify(source);
        if !self.connections.iter().any(|c| c.id == base) {
            return base;
        }
        let mut n = 2;
        loop {
            let candidate = format!("{base}-{n}");
            if !self.connections.iter().any(|c| c.id == candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Add or replace a connection (keyed by `id`, deriving one when blank).
    /// Returns the stored connection so callers see the resolved id.
    pub fn upsert(&mut self, mut conn: ProviderConnection) -> ProviderConnection {
        if conn.id.trim().is_empty() {
            conn.id = self.derive_id(&conn);
        }
        // OR-upgrade `supports_vision` from the model capability map so a
        // freshly-saved connection on a known vision-capable model un-gates
        // attachments without a manual settings toggle. Never downgrades an
        // explicit `true`. See [`model_supports_vision`].
        conn.supports_vision =
            conn.supports_vision || model_supports_vision(conn.kind, &conn.model);
        conn.supports_documents =
            conn.supports_documents || model_supports_documents(conn.kind, &conn.model);
        match self.connections.iter_mut().find(|c| c.id == conn.id) {
            Some(slot) => *slot = conn.clone(),
            None => self.connections.push(conn.clone()),
        }
        conn
    }

    /// Apply [`model_supports_vision`] to every connection, OR'"'"'ing the inferred
    /// capability into the stored flag. Idempotent and never downgrades an
    /// explicit `true`. Callers should run this once after loading a persisted
    /// registry so older saves (which never set `supportsVision`) reflect the
    /// model'"'"'s actual capability for the FE attach gate (#408).
    pub fn normalize_capabilities(&mut self) {
        for conn in &mut self.connections {
            conn.supports_vision =
                conn.supports_vision || model_supports_vision(conn.kind, &conn.model);
            conn.supports_documents =
                conn.supports_documents || model_supports_documents(conn.kind, &conn.model);
        }
    }

    /// Remove a connection by id. `Err` when it is the last one. If the removed
    /// connection was active, `active` falls back to the first remaining one.
    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        if self.connections.len() <= 1 {
            return Err("cannot remove the last connection".to_string());
        }
        let Some(idx) = self.connections.iter().position(|c| c.id == id) else {
            return Ok(());
        };
        self.connections.remove(idx);
        if self.active == id {
            self.active = self.connections[0].id.clone();
        }
        Ok(())
    }

    /// Select the active connection by id. `Err` on an unknown id.
    pub fn set_active(&mut self, id: &str) -> Result<(), String> {
        if !self.connections.iter().any(|c| c.id == id) {
            return Err(format!("unknown connection: {id}"));
        }
        self.active = id.to_string();
        Ok(())
    }
}

/// Lowercase, collapse non-alphanumeric runs to `-`, trim leading/trailing `-`.
/// Empty input (or all-punctuation) falls back to `"connection"`.
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = true; // suppress a leading dash
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "connection".to_string()
    } else {
        out
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
            thinking: true,
            reasoning_effort: ReasoningEffort::default(),
            supports_vision: false,
            supports_documents: false,
            region: None,
            auth_mode: None,
            aws_profile: None,
            access_key_id: None,
        };
        let ollama = ProviderConnection {
            id: "ollama".to_string(),
            kind: ProviderKind::Ollama,
            display_name: "Ollama".to_string(),
            vendor: None,
            base_url: None,
            model: "llama3.2".to_string(),
            has_key: false,
            thinking: true,
            reasoning_effort: ReasoningEffort::default(),
            supports_vision: false,
            supports_documents: false,
            region: None,
            auth_mode: None,
            aws_profile: None,
            access_key_id: None,
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
    fn siliconflow_kind_defaults_global_endpoint_and_slug() {
        assert_eq!(
            ProviderKind::SiliconFlow.default_base_url(),
            "https://api.siliconflow.com/v1"
        );
        assert_eq!(ProviderKind::SiliconFlow.slug(), "siliconflow");
    }

    #[test]
    fn siliconflow_kind_serializes_camel_case() {
        let json = serde_json::to_string(&ProviderKind::SiliconFlow).unwrap();
        assert_eq!(json, "\"siliconFlow\"");
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

    fn blank_conn(display: &str, vendor: Option<&str>, kind: ProviderKind) -> ProviderConnection {
        ProviderConnection {
            id: String::new(),
            kind,
            display_name: display.to_string(),
            vendor: vendor.map(str::to_string),
            base_url: None,
            model: "m".to_string(),
            has_key: false,
            thinking: true,
            reasoning_effort: ReasoningEffort::default(),
            supports_vision: false,
            supports_documents: false,
            region: None,
            auth_mode: None,
            aws_profile: None,
            access_key_id: None,
        }
    }

    #[test]
    fn derive_id_dedupes_against_seeded_connection() {
        let reg = ProviderRegistry::default();
        // "ollama" is already seeded, so a new "Ollama" derives ollama-2.
        let id = reg.derive_id(&blank_conn("Ollama", None, ProviderKind::Ollama));
        assert_eq!(id, "ollama-2");
    }

    #[test]
    fn derive_id_prefers_vendor_then_display_then_kind() {
        let reg = ProviderRegistry {
            connections: vec![],
            active: String::new(),
        };
        assert_eq!(
            reg.derive_id(&blank_conn(
                "My Display",
                Some("OpenRouter"),
                ProviderKind::CandleVllm
            )),
            "openrouter"
        );
        assert_eq!(
            reg.derive_id(&blank_conn("LM Studio", None, ProviderKind::CandleVllm)),
            "lm-studio"
        );
        // Blank vendor + blank display -> kind slug.
        assert_eq!(
            reg.derive_id(&blank_conn("   ", None, ProviderKind::Ollama)),
            "ollama"
        );
    }

    #[test]
    fn upsert_appends_with_derived_id_then_edits_in_place() {
        let mut reg = ProviderRegistry::default();
        let stored = reg.upsert(blank_conn(
            "OpenRouter",
            Some("openrouter"),
            ProviderKind::CandleVllm,
        ));
        assert_eq!(stored.id, "openrouter");
        assert_eq!(reg.connections.len(), 3);
        // Editing the same id replaces in place (no new entry).
        let edited = reg.upsert(ProviderConnection {
            model: "new-model".to_string(),
            ..stored.clone()
        });
        assert_eq!(edited.model, "new-model");
        assert_eq!(reg.connections.len(), 3);
        assert_eq!(
            reg.connections
                .iter()
                .find(|c| c.id == "openrouter")
                .unwrap()
                .model,
            "new-model"
        );
    }

    #[test]
    fn remove_rejects_last_and_reassigns_active() {
        let mut reg = ProviderRegistry::default();
        reg.set_active("ollama").unwrap();
        reg.remove("ollama").unwrap();
        assert_eq!(reg.connections.len(), 1);
        assert_eq!(reg.active, "candle-vllm");
        // Now only one remains -> reject.
        assert!(reg.remove("candle-vllm").is_err());
    }

    #[test]
    fn remove_unknown_is_noop() {
        let mut reg = ProviderRegistry::default();
        reg.remove("does-not-exist").unwrap();
        assert_eq!(reg.connections.len(), 2);
    }

    #[test]
    fn set_active_rejects_unknown() {
        let mut reg = ProviderRegistry::default();
        assert!(reg.set_active("nope").is_err());
        assert_eq!(reg.active, "candle-vllm");
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
            thinking: true,
            reasoning_effort: ReasoningEffort::default(),
            supports_vision: false,
            supports_documents: false,
            region: None,
            auth_mode: None,
            aws_profile: None,
            access_key_id: None,
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

    #[test]
    fn bedrock_kind_slug_and_base_url() {
        assert_eq!(ProviderKind::Bedrock.slug(), "bedrock");
        assert_eq!(
            ProviderKind::Bedrock.default_base_url(),
            "https://bedrock-runtime.us-east-1.amazonaws.com"
        );
        let k: ProviderKind = serde_json::from_str("\"bedrock\"").unwrap();
        assert_eq!(k, ProviderKind::Bedrock);
    }

    #[test]
    fn openai_kind_slug_and_base_url() {
        assert_eq!(ProviderKind::OpenAi.slug(), "openai");
        assert_eq!(
            ProviderKind::OpenAi.default_base_url(),
            "https://api.openai.com/v1"
        );
    }

    #[test]
    fn openai_kind_wire_tag_is_pinned_not_camel_case() {
        // The variant is pinned to "openai" via #[serde(rename)], NOT the
        // camelCase default "openAi". Round-trips and matches slug()/vendor.
        assert_eq!(
            serde_json::to_string(&ProviderKind::OpenAi).unwrap(),
            "\"openai\""
        );
        let k: ProviderKind = serde_json::from_str("\"openai\"").unwrap();
        assert_eq!(k, ProviderKind::OpenAi);
    }

    #[test]
    fn bedrock_auth_serializes_camel_case() {
        for (variant, wire) in [
            (BedrockAuth::Auto, "\"auto\""),
            (BedrockAuth::Profile, "\"profile\""),
            (BedrockAuth::IamKeys, "\"iamKeys\""),
            (BedrockAuth::ApiKey, "\"apiKey\""),
        ] {
            assert_eq!(serde_json::to_string(&variant).unwrap(), wire);
            assert_eq!(serde_json::from_str::<BedrockAuth>(wire).unwrap(), variant);
        }
    }

    #[test]
    fn resolve_auto_prefers_api_key_then_profile_then_iam_keys() {
        use BedrockAuth::*;
        // (has_api_key, has_profile, has_iam_keys) => expected winner
        assert_eq!(BedrockAuth::resolve_auto(true, true, true), ApiKey);
        assert_eq!(BedrockAuth::resolve_auto(false, true, true), Profile);
        assert_eq!(BedrockAuth::resolve_auto(false, false, true), IamKeys);
        assert_eq!(BedrockAuth::resolve_auto(true, false, false), ApiKey);
        assert_eq!(BedrockAuth::resolve_auto(false, true, false), Profile);
        // Nothing configured falls back to Profile so the probe surfaces the failure.
        assert_eq!(BedrockAuth::resolve_auto(false, false, false), Profile);
    }

    #[test]
    fn secret_kind_slug_all_and_serde() {
        assert_eq!(SecretKind::ALL.len(), 3);
        assert_eq!(SecretKind::ApiKey.slug(), "apiKey");
        assert_eq!(SecretKind::SecretAccessKey.slug(), "secretAccessKey");
        assert_eq!(SecretKind::SessionToken.slug(), "sessionToken");
        for kind in SecretKind::ALL {
            let wire = serde_json::to_string(&kind).unwrap();
            assert_eq!(wire, format!("\"{}\"", kind.slug()));
            assert_eq!(serde_json::from_str::<SecretKind>(&wire).unwrap(), kind);
        }
    }

    #[test]
    fn connection_skips_none_bedrock_fields_and_round_trips() {
        let conn = blank_conn("Local", None, ProviderKind::Ollama);
        let json = serde_json::to_string(&conn).unwrap();
        assert!(!json.contains("region"), "None region is skipped");
        assert!(!json.contains("authMode"), "None auth_mode is skipped");
        assert!(!json.contains("awsProfile"));
        assert!(!json.contains("accessKeyId"));
        let back: ProviderConnection = serde_json::from_str(&json).unwrap();
        assert_eq!(conn, back);
    }

    #[test]
    fn supports_vision_defaults_false_and_round_trips() {
        let conn = blank_conn("Local", None, ProviderKind::Ollama);
        assert!(!conn.supports_vision);
        // Absent in legacy JSON -> defaults false on deserialize.
        let mut v: serde_json::Value = serde_json::to_value(&conn).unwrap();
        v.as_object_mut().unwrap().remove("supportsVision");
        let back: ProviderConnection = serde_json::from_value(v).unwrap();
        assert!(!back.supports_vision);
        // Set true -> serializes and round-trips.
        let vision = ProviderConnection {
            supports_vision: true,
            supports_documents: false,
            ..conn
        };
        let json = serde_json::to_string(&vision).unwrap();
        assert!(json.contains("\"supportsVision\":true"));
        let back: ProviderConnection = serde_json::from_str(&json).unwrap();
        assert!(back.supports_vision);
    }

    #[test]
    fn model_supports_vision_covers_known_families() {
        // Modern Claude on Bedrock: 3.x and 4.x, plus the named Mythos/Fable.
        for m in [
            "us.anthropic.claude-opus-4-8",
            "us.anthropic.claude-opus-4-5",
            "us.anthropic.claude-sonnet-4-6",
            "anthropic.claude-3-5-sonnet-20241022-v2:0",
            "us.anthropic.claude-3-opus-20240229-v1:0",
            "claude-mythos-5",
            "claude-fable-5",
        ] {
            assert!(
                model_supports_vision(ProviderKind::Bedrock, m),
                "Bedrock vision: {m}"
            );
        }
        // OpenAI vision-capable.
        for m in [
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4.1",
            "gpt-4-turbo",
            "gpt-5",
            "o1",
            "o3-mini",
        ] {
            assert!(
                model_supports_vision(ProviderKind::OpenAi, m),
                "OpenAI vision: {m}"
            );
        }
        // Ollama vision tags.
        for m in ["llava:7b", "llama3.2-vision", "moondream", "qwen2-vl:7b"] {
            assert!(
                model_supports_vision(ProviderKind::Ollama, m),
                "Ollama vision: {m}"
            );
        }
        // SiliconFlow VL/4V suffixes.
        for m in ["Qwen/Qwen2-VL-7B-Instruct", "zai-org/GLM-4V-9B"] {
            assert!(
                model_supports_vision(ProviderKind::SiliconFlow, m),
                "SiliconFlow vision: {m}"
            );
        }
        // Negative coverage: text-only stays text-only.
        for (k, m) in [
            (ProviderKind::Ollama, "llama3.2"),
            (ProviderKind::OpenAi, "gpt-3.5-turbo"),
            (ProviderKind::Bedrock, "meta.llama3-70b-instruct-v1:0"),
            (ProviderKind::SiliconFlow, "deepseek-ai/DeepSeek-V3"),
            (ProviderKind::CandleVllm, "anything"),
        ] {
            assert!(
                !model_supports_vision(k, m),
                "expected text-only: {k:?} {m}"
            );
        }
    }

    #[test]
    fn model_supports_documents_is_bedrock_only() {
        // Bedrock's Converse API carries a portable DocumentBlock regardless of
        // the model's vision capability (a text-only Claude still reads PDFs).
        for m in [
            "us.anthropic.claude-opus-4-8",
            "meta.llama3-70b-instruct-v1:0",
            "anything",
        ] {
            assert!(
                model_supports_documents(ProviderKind::Bedrock, m),
                "Bedrock documents: {m}"
            );
        }
        // No portable document block on the OpenAI-compatible or Ollama wire (#338).
        for (k, m) in [
            (ProviderKind::OpenAi, "gpt-4o"),
            (ProviderKind::SiliconFlow, "Qwen/Qwen2-VL-7B-Instruct"),
            (ProviderKind::Ollama, "llava:7b"),
            (ProviderKind::CandleVllm, "anything"),
        ] {
            assert!(
                !model_supports_documents(k, m),
                "expected no documents: {k:?} {m}"
            );
        }
    }

    #[test]
    fn registry_normalize_capabilities_upgrades_false_and_keeps_true() {
        let mut reg = ProviderRegistry {
            active: "bed".into(),
            connections: vec![
                ProviderConnection {
                    id: "bed".into(),
                    kind: ProviderKind::Bedrock,
                    model: "us.anthropic.claude-opus-4-8".into(),
                    supports_vision: false,
                    supports_documents: false,
                    ..blank_conn("Bedrock", Some("aws"), ProviderKind::Bedrock)
                },
                ProviderConnection {
                    id: "oll".into(),
                    kind: ProviderKind::Ollama,
                    model: "llama3.2".into(),
                    supports_vision: false,
                    supports_documents: false,
                    ..blank_conn("Ollama", None, ProviderKind::Ollama)
                },
                ProviderConnection {
                    id: "candle".into(),
                    kind: ProviderKind::CandleVllm,
                    model: "anything".into(),
                    supports_vision: true,
                    supports_documents: false,
                    ..blank_conn("Candle", None, ProviderKind::CandleVllm)
                },
            ],
        };
        reg.normalize_capabilities();
        // Inferred upgrade.
        assert!(reg.connections[0].supports_vision, "Opus 4.8 ⇒ vision");
        // Unknown stays false.
        assert!(!reg.connections[1].supports_vision, "llama3.2 stays text");
        // Explicit true preserved despite no inference.
        assert!(
            reg.connections[2].supports_vision,
            "explicit true preserved"
        );
        // Idempotent.
        let before = reg.clone();
        reg.normalize_capabilities();
        assert_eq!(reg, before);
    }

    #[test]
    fn upsert_or_upgrades_vision_from_model_id() {
        let mut reg = ProviderRegistry {
            active: "x".into(),
            connections: vec![],
        };
        let stored = reg.upsert(ProviderConnection {
            id: "bed".into(),
            kind: ProviderKind::Bedrock,
            model: "us.anthropic.claude-opus-4-8".into(),
            supports_vision: false,
            supports_documents: false,
            ..blank_conn("Bedrock", Some("aws"), ProviderKind::Bedrock)
        });
        assert!(
            stored.supports_vision,
            "saved Opus 4.8 ⇒ supportsVision=true"
        );
    }

    #[test]
    fn connection_with_bedrock_fields_round_trips() {
        let conn = ProviderConnection {
            region: Some("us-west-2".into()),
            auth_mode: Some(BedrockAuth::IamKeys),
            access_key_id: Some("AKIAEXAMPLE".into()),
            ..blank_conn("Bedrock", Some("aws"), ProviderKind::Bedrock)
        };
        let json = serde_json::to_string(&conn).unwrap();
        assert!(json.contains("\"region\":\"us-west-2\""));
        assert!(json.contains("\"authMode\":\"iamKeys\""));
        assert!(json.contains("\"accessKeyId\":\"AKIAEXAMPLE\""));
        let back: ProviderConnection = serde_json::from_str(&json).unwrap();
        assert_eq!(conn, back);
    }
}
