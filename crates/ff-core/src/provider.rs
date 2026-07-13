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

fn default_warmup_enabled() -> bool {
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

/// How widely model reasoning (chain-of-thought) is requested across a turn's
/// loop steps (#549). Orthogonal to the on/off gate (`ChatRequest::thinking` /
/// `ProviderConfig::thinking`) and to the *depth* dial ([`ReasoningEffort`]):
/// this controls *which steps* request reasoning, not whether reasoning is on or
/// how long it runs.
///
/// Background: a turn cannot know a-priori whether the current loop step will
/// dispatch more tools or emit the final answer (the model decides by returning
/// tool calls). So requesting reasoning only on a "final answer" step is not
/// expressible without requesting it on every step. The two honest points on
/// that tradeoff are:
/// - [`WrapUp`](Self::WrapUp): planning (first step) + the cap-forced wrap-up
///   step only -- the latency-optimized choice (#449). A turn that finishes
///   naturally before the cap answers with reasoning off, so its final answer
///   shows no Thought block.
/// - [`All`](Self::All): every step. A short turn's natural final answer now
///   carries reasoning. The persisted reasoning is always the *final* step's
///   (the whole turn shares one assistant message id, and each step overwrites
///   the row), so `All` does not bloat storage with mid-loop chains -- it only
///   trades mid-loop reasoning latency for final-answer visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum ReasoningVisibility {
    /// First step + cap-forced wrap-up only (#449 latency optimization).
    WrapUp,
    /// Every step, so a natural final answer carries reasoning (#549). The
    /// default when reasoning is enabled.
    #[default]
    All,
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
    /// OpenRouter. Hosted multi-provider gateway; bearer API key in the keychain.
    /// Model IDs use `provider/model` format (e.g. `anthropic/claude-sonnet-4-20250514`).
    /// Uses `ReasoningWire::Reasoning` (the `reasoning` field, not `reasoning_content`).
    #[serde(rename = "openRouter")]
    OpenRouter,
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
            ProviderKind::OpenRouter => "https://openrouter.ai/api/v1",
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
            ProviderKind::OpenRouter => "openrouter",
        }
    }

    /// Whether this is a local, credential-free backend running on the user's
    /// own machine (candle-vLLM or Ollama). Hosted kinds return `false`. Gates
    /// the composer warmup nudge (#61): warming a *hosted* endpoint would fire a
    /// billed request on every composer focus, so warmup is local-only.
    pub fn is_local(self) -> bool {
        matches!(self, ProviderKind::CandleVllm | ProviderKind::Ollama)
    }

    /// The out-of-box `thinking` default for a *fresh* connection of this kind
    /// (#633). Local kinds (candle-vLLM / Ollama) default reasoning **off**:
    /// hybrid-thinking models emit reasoning tokens before every answer, the
    /// dominant per-turn latency cost on local hardware, so fresh local
    /// connections are fast out of the box; the user re-enables per-connection
    /// via the model-picker / Settings Thinking toggle (#640) for hard tasks.
    /// Hosted kinds default **on** -- they don't carry that local latency cost.
    pub fn default_thinking(self) -> bool {
        !self.is_local()
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
    /// Which loop steps request reasoning (#549). Only bites when `thinking` is
    /// on. `#[serde(default)]` keeps legacy `provider.json` files loading as
    /// [`ReasoningVisibility::All`] (the natural final answer shows a Thought).
    #[serde(default)]
    pub reasoning_visibility: ReasoningVisibility,
    /// Whether the composer warmup nudge (#61) fires for this connection. Default
    /// `true` (no regression). Only meaningful for local kinds; the warmup command
    /// also gates on [`ProviderKind::is_local`]. Users disable it to avoid sustained
    /// GPU use (e.g. on laptop battery). `#[serde(default = "default_warmup_enabled")]`
    /// keeps pre-#61 registries loading as `true`.
    #[serde(default = "default_warmup_enabled")]
    pub warmup_enabled: bool,
    /// Served context window for local Ollama providers, in tokens (#538, #651).
    /// `None` ⇒ fall back to `FLOWFORGE_OLLAMA_NUM_CTX`, then the probed window,
    /// then the conservative default. Only meaningful for Ollama; clamped to the
    /// model's trained ceiling by the served-window resolution. Mirrors
    /// [`ProviderConnection::num_ctx`]. `#[serde(default)]` keeps pre-#651
    /// `provider.json` files loading as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub num_ctx: Option<u32>,
}

/// FlowForge's out-of-the-box default: local candle-vllm serving Qwen3-4B.
impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            kind: ProviderKind::CandleVllm,
            base_url: None,
            model: "Qwen3-4B-Instruct-2507".to_string(),
            has_key: false,
            thinking: ProviderKind::CandleVllm.default_thinking(),
            reasoning_effort: ReasoningEffort::default(),
            reasoning_visibility: ReasoningVisibility::default(),
            warmup_enabled: true,
            num_ctx: None,
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
    /// True when the registry believes a key is stored (`has_key`) but the OS
    /// keychain no longer returns it — most often because an app rebuild changed
    /// the code-signing identity and the keychain ACL denied the new binary.
    /// Lets the UI prompt "re-enter your key" instead of failing auth silently.
    /// Computed on the read path; never persisted as `true`.
    #[serde(default)]
    pub secret_missing: bool,
    /// When true, request and surface model reasoning/thinking streams (#181).
    #[serde(default = "default_thinking")]
    pub thinking: bool,
    /// Reasoning *depth* dial for this connection (#395). Only bites when
    /// `thinking` is on; picks the per-backend reasoning token budget in
    /// `build_provider`. `#[serde(default)]` keeps pre-#395 registries loading as
    /// [`ReasoningEffort::Medium`].
    #[serde(default)]
    pub reasoning_effort: ReasoningEffort,
    /// Which loop steps request reasoning for this connection (#549). Only bites
    /// when `thinking` is on. `#[serde(default)]` keeps pre-#549 registries
    /// loading as [`ReasoningVisibility::All`].
    #[serde(default)]
    pub reasoning_visibility: ReasoningVisibility,
    /// Whether the composer warmup nudge (#61) fires for this connection. Default
    /// `true` (no regression). Only meaningful for local kinds; the warmup command
    /// also gates on [`ProviderKind::is_local`]. Users disable it to avoid sustained
    /// GPU use (e.g. on laptop battery). `#[serde(default = "default_warmup_enabled")]`
    /// keeps pre-#61 registries loading as `true`.
    #[serde(default = "default_warmup_enabled")]
    pub warmup_enabled: bool,
    /// Served context window for local Ollama connections, in tokens (#538, #651).
    /// `None` ⇒ fall back to `FLOWFORGE_OLLAMA_NUM_CTX`, then the probed window,
    /// then the conservative default. Only meaningful for Ollama; clamped to the
    /// model's trained ceiling by the served-window resolution. `#[serde(default)]`
    /// keeps pre-#651 registries loading as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub num_ctx: Option<u32>,
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
    /// Fast model for compaction/flush LLM calls (#756). When set, memory flush
    /// and abstractive summarization use this model instead of the session model.
    /// Example: `"global.anthropic.claude-haiku-4-5-20251001-v1:0"` (Bedrock), `"gpt-4o-mini"` (OpenAI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub compaction_model: Option<String>,
    /// Context budget (in tokens) at which compaction engages (#756). When set,
    /// overrides the default `model_window * 0.8`. Maps to the UI's
    /// "Summarization threshold" slider. `None` = use the computed default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub compaction_budget: Option<u64>,
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
/// #504). Universal across providers as of the #338 follow-up: Bedrock's
/// Converse API carries a native `DocumentBlock`, while the OpenAI-compatible
/// and Ollama wire formats gain document support via a client-side
/// text-extraction fallback (the adapter extracts the document's text and
/// folds it into the user message's prompt context). The `model` argument is
/// reserved for a future data-driven split (mirrors the #466 vision migration)
/// should a non-Bedrock provider narrow document support per model.
pub fn model_supports_documents(_kind: ProviderKind, _model: &str) -> bool {
    true
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
    /// Registry schema version, bumped when a persisted-shape migration must run
    /// exactly once on load (see [`ProviderRegistry::migrate`]). `#[serde(default)]`
    /// makes a pre-versioning `provider-registry.json` load as `0`, which triggers
    /// the pending migrations. Backend-internal; the frontend ignores it.
    #[serde(default)]
    pub schema_version: u32,
}

/// A resolved `(connection, model)` pair -- the unit of model selection at every
/// tier (session / phenotype / global) in RFC 0005 §11. `connection` picks the
/// endpoint + credentials; `model` is the model to run on it. Capabilities (e.g.
/// vision) are derived from the connection's `kind` + this `model` via
/// [`model_supports_vision`], never stored on the selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ModelSelection {
    pub connection: ConnectionId,
    pub model: String,
}

/// A *resolved* model selection plus the capabilities derived from it (RFC 0005
/// §11.3). `connection` + `model` are the resolved pair (after the session,
/// phenotype, then global precedence); `supports_vision` and `supports_documents`
/// are derived from the resolved `(kind, model)` via [`model_supports_vision`] and
/// [`model_supports_documents`], never stored on a connection. This single-sources
/// attachment capability at the resolution point so a per-session model override is
/// gated by the model it actually runs, not the connection's default model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ResolvedModel {
    pub connection: ConnectionId,
    pub model: String,
    pub supports_vision: bool,
    pub supports_documents: bool,
    /// Effective served context window in tokens (#602): the window the runtime
    /// will actually serve, not the model's trained maximum. `None` for non-Ollama
    /// connections and when no probe ran. This is the denominator the compaction
    /// budget is sized from; #598 must forward this same number. Stored as `u32`
    /// (windows are tiny relative to the range) so the binding is `number`, not the
    /// `bigint` ts-rs emits for `u64`, matching the FE `ServedWindow.window`.
    pub context_window: Option<u32>,
    /// Trained context ceiling (`/api/show` `context_length`), or `None` when
    /// unknown. The "trained X" half of the chip readout.
    pub trained_context_window: Option<u32>,
    /// Which input produced [`context_window`](Self::context_window) (#602), or
    /// `None` when no served window is known.
    pub context_window_source: Option<ContextWindowSource>,
}

/// How the effective served context window was determined, in precedence order
/// (#602). `Explicit` = the `FLOWFORGE_OLLAMA_NUM_CTX` override; `Served` = probed
/// from the live Ollama runtime via `/api/ps`; `Default` = the conservative
/// [`crate::DEFAULT_CONTEXT_WINDOW_TOKENS`] fallback (model not loaded or the probe
/// failed). Serializes to the matching camelCase string the FE union expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum ContextWindowSource {
    Explicit,
    Served,
    Default,
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
        match self.connections.iter_mut().find(|c| c.id == conn.id) {
            Some(slot) => *slot = conn.clone(),
            None => self.connections.push(conn.clone()),
        }
        conn
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

/// Current [`ProviderRegistry`] schema version. Bump when adding a one-time
/// on-load migration; a persisted registry with a lower `schema_version` runs the
/// pending migrations in [`ProviderRegistry::migrate`].
///
/// - `1` (#633): default reasoning/thinking **off** for local connections and
///   flip existing local connections once, so fresh installs are fast on local
///   models and pre-#633 connections stop behaving differently from new ones.
/// - `2` (#807): seed an OpenRouter connection for existing users.
pub const REGISTRY_SCHEMA_VERSION: u32 = 2;

impl ProviderRegistry {
    /// Apply any pending one-time migrations to a loaded registry, in memory. Called
    /// on the load path; the bumped `schema_version` (and any flipped values) persist
    /// on the next registry mutation via the lazy-save path -- construction itself
    /// never writes (a contract the load tests assert). Idempotent: a registry
    /// already at [`REGISTRY_SCHEMA_VERSION`] is left untouched, so a user who
    /// re-enables thinking after the migration is never re-flipped.
    /// Parse a persisted registry, salvaging what we can rather than failing closed.
    ///
    /// A strict parse is tried first. If that fails (e.g. one connection carries a
    /// field this build cannot deserialize, or a future variant was added), we fall
    /// back to a per-connection salvage: parse the top level loosely, keep every
    /// `connections[]` entry that still deserializes into a [`ProviderConnection`],
    /// and preserve `active` (or the first surviving connection if the recorded
    /// active id no longer resolves). Returns `None` only when *nothing* is
    /// salvageable — the sole case where the caller falls back to the factory
    /// default. This is what stops a single bad/forward-incompatible field from
    /// wiping every configured connection back to the built-in Candle default.
    pub fn parse_lenient(raw: &str) -> Option<Self> {
        if let Ok(registry) = serde_json::from_str::<Self>(raw) {
            return Some(registry);
        }
        let value: serde_json::Value = serde_json::from_str(raw).ok()?;
        let connections: Vec<ProviderConnection> = value
            .get("connections")?
            .as_array()?
            .iter()
            .filter_map(|c| serde_json::from_value::<ProviderConnection>(c.clone()).ok())
            .collect();
        if connections.is_empty() {
            return None;
        }
        let recorded_active = value
            .get("active")
            .and_then(|a| a.as_str())
            .map(str::to_string);
        let active = recorded_active
            .filter(|id| connections.iter().any(|c| &c.id == id))
            .unwrap_or_else(|| connections[0].id.clone());
        let schema_version = value
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32;
        Some(Self {
            connections,
            active,
            schema_version,
        })
    }

    pub fn migrate(&mut self) {
        if self.schema_version < 1 {
            // #633: local reasoning defaults off. Flip existing local connections
            // once so they match fresh ones; hosted connections keep their value.
            for conn in &mut self.connections {
                if conn.kind.is_local() {
                    conn.thinking = false;
                }
            }
        }
        if self.schema_version < 2 {
            // #807: seed an OpenRouter connection for existing users who don't
            // already have one (e.g. from a manual openai+vendor:openrouter setup
            // that the lenient parser preserved as kind=OpenRouter).
            if !self
                .connections
                .iter()
                .any(|c| c.kind == ProviderKind::OpenRouter)
            {
                self.connections.push(Self::default_openrouter_connection());
            }
        }
        self.schema_version = REGISTRY_SCHEMA_VERSION;
    }
}

impl ProviderRegistry {
    /// The default OpenRouter connection, shared by `Default::default()` and the v2
    /// migration so both produce the exact same shape.
    fn default_openrouter_connection() -> ProviderConnection {
        ProviderConnection {
            id: "openrouter".to_string(),
            kind: ProviderKind::OpenRouter,
            display_name: "OpenRouter".to_string(),
            vendor: None,
            base_url: None,
            model: "anthropic/claude-sonnet-4-20250514".to_string(),
            has_key: false,
            secret_missing: false,
            thinking: ProviderKind::OpenRouter.default_thinking(),
            reasoning_effort: ReasoningEffort::default(),
            reasoning_visibility: ReasoningVisibility::default(),
            warmup_enabled: true,
            num_ctx: None,
            region: None,
            auth_mode: None,
            aws_profile: None,
            access_key_id: None,
            compaction_model: None,
            compaction_budget: None,
        }
    }
}

/// FlowForge's out-of-the-box registry: local candle-vLLM (active), a ready
/// keyless Ollama, and a keyless OpenRouter — the user supplies an API key to
/// activate the hosted providers.
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
            secret_missing: false,
            thinking: ProviderKind::CandleVllm.default_thinking(),
            reasoning_effort: ReasoningEffort::default(),
            reasoning_visibility: ReasoningVisibility::default(),
            warmup_enabled: true,
            num_ctx: None,
            region: None,
            auth_mode: None,
            aws_profile: None,
            access_key_id: None,
            compaction_model: None,
            compaction_budget: None,
        };
        let ollama = ProviderConnection {
            id: "ollama".to_string(),
            kind: ProviderKind::Ollama,
            display_name: "Ollama".to_string(),
            vendor: None,
            base_url: None,
            model: "llama3.2".to_string(),
            has_key: false,
            secret_missing: false,
            thinking: ProviderKind::Ollama.default_thinking(),
            reasoning_effort: ReasoningEffort::default(),
            reasoning_visibility: ReasoningVisibility::default(),
            warmup_enabled: true,
            num_ctx: None,
            region: None,
            auth_mode: None,
            aws_profile: None,
            access_key_id: None,
            compaction_model: None,
            compaction_budget: None,
        };
        Self {
            active: candle.id.clone(),
            connections: vec![candle, ollama, Self::default_openrouter_connection()],
            schema_version: REGISTRY_SCHEMA_VERSION,
        }
    }
}

#[cfg(test)]
mod tests;
