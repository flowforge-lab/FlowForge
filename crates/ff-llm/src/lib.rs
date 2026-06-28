//! LLM provider abstraction. M1 ships two providers behind a single trait:
//! [`OpenAiProvider`] (OpenAI-compatible SSE — candle-vllm, vLLM, LM Studio, OpenAI)
//! and [`OllamaProvider`] (Ollama-native NDJSON `/api/chat`). [`BedrockProvider`]
//! (AWS Converse) and [`AnthropicProvider`] (native Messages API) land behind the
//! same trait.

mod anthropic;
mod bedrock;
mod model_specs;
mod ollama;
mod openai;

use async_trait::async_trait;
use base64::Engine as _;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

pub use anthropic::AnthropicProvider;
pub use bedrock::{BedrockCreds, BedrockProvider};
pub use ollama::OllamaProvider;
pub use openai::OpenAiProvider;

use std::time::Duration;

/// Initial TCP/TLS connect budget for the SSE providers.
pub(crate) const CONNECT_TIMEOUT_SECS: u64 = 10;

/// Maximum mid-stream silence (no bytes received) tolerated before a read is
/// aborted. A stalled SSE stream -- headers sent, then bytes stop with no
/// `[DONE]` or connection close -- otherwise hangs forever (observed with
/// SiliconFlow GLM, ~31 min stuck). Tripping this surfaces a `Transport` error,
/// which [`LlmError::is_transient`] marks retryable, so the agent loop's
/// existing bounded retry recovers automatically. This is an idle-between-reads
/// timeout, NOT a total-request timeout: long legitimate reasoning streams that
/// keep emitting bytes are unaffected.
///
/// Set to 30s (down from 60s): a healthy hosted gateway's time-to-first-byte is
/// well under this, and with up to `MAX_PROVIDER_ATTEMPTS` retries a 60s idle
/// budget meant a single stall could silently burn ~3 min before recovery.
pub(crate) const IDLE_READ_TIMEOUT_SECS: u64 = 30;

/// Shared reqwest client for the SSE-based providers (OpenAI-compatible,
/// Ollama-native, Anthropic Messages). Bedrock builds its own client through the
/// AWS SDK, which carries its own timeouts. Falls back to a default client if the
/// builder fails so provider construction stays infallible.
///
/// The client is built **once** and cached process-wide (#B3): a `reqwest::Client`
/// is `Arc`-internally and clones share one connection pool, so reusing it across
/// provider builds lets a new turn reuse the previous turn's kept-alive TLS
/// connection instead of paying a cold TCP+TLS handshake every turn. The timeout
/// config is identical for every build, so a singleton is behavior-equivalent
/// apart from the (desirable) connection reuse.
pub(crate) fn build_streaming_http_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
                .read_timeout(Duration::from_secs(IDLE_READ_TIMEOUT_SECS))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new())
        })
        .clone()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    /// `None` for an assistant message that only carries tool calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Tool calls requested by an assistant message (OpenAI shape).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Set on a `role: "tool"` message to bind the result to its request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool name, set on a `role: "tool"` message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Files attached to the message (multimodal, #332). Empty for a plain text
    /// turn; skipped on the wire when empty, so a text-only request is unchanged.
    /// Providers map these to their own block formats in their own tickets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ff_core::Attachment>,
    /// Carrier for prior-turn reasoning text (#375). NEVER serialized through
    /// the derived path: each provider re-injects it into the wire under the
    /// dialect-specific field name (`reasoning_content` for SiliconFlow,
    /// `reasoning` for OpenRouter, omitted for vanilla OpenAI). `#[serde(skip)]`
    /// is load-bearing — `OpenAiProvider::message_to_wire` calls
    /// `serde_json::to_value(msg)`, which would otherwise leak this field
    /// through unchanged on every gateway.
    #[serde(skip)]
    pub reasoning: Option<String>,
}

impl ChatMessage {
    /// A plain text message (user/assistant/system).
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            attachments: Vec::new(),
            reasoning: None,
        }
    }

    /// A user message carrying text plus one or more attachments (#332).
    pub fn multimodal(
        role: impl Into<String>,
        content: impl Into<String>,
        attachments: Vec<ff_core::Attachment>,
    ) -> Self {
        Self {
            role: role.into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            attachments,
            reasoning: None,
        }
    }
}

/// A function/tool call as emitted by the model and echoed back in history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON-encoded arguments object (a string, per the OpenAI protocol).
    pub arguments: String,
}

#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    /// OpenAI `tools` entries. Empty = a plain chat turn.
    pub tools: Vec<serde_json::Value>,
    /// When true, request provider reasoning/thinking streams when supported (#181).
    pub thinking: bool,
    /// Output-token ceiling for this turn. `Some` pins the provider's `max_tokens`
    /// so a large tool-call payload (the whole-file `write` body is an argument)
    /// plus any thinking cannot be cut off mid-JSON (#550, the gateway-path sibling
    /// of the Bedrock #529 pin). `None` leaves the provider default. Honored on the
    /// OpenAI/gateway path; Bedrock and Anthropic pin their own ceilings internally.
    pub max_tokens: Option<u32>,
}

/// One incremental tool-call fragment within a stream. Servers may split a single
/// call across chunks, so fragments are accumulated by `index`.
#[derive(Debug, Clone, Default)]
pub struct ToolCallDelta {
    pub index: u32,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: String,
}

/// One streamed increment of an assistant response.
#[derive(Debug, Clone, Default)]
pub struct Chunk {
    pub delta: String,
    pub reasoning_delta: String,
    pub tool_calls: Vec<ToolCallDelta>,
    pub done: bool,
    /// The provider stopped because the output token cap was reached
    /// (Bedrock `MaxTokens` / OpenAI `finish_reason = "length"`). Any
    /// in-flight tool-call arguments may be cut off mid-JSON, so the agent
    /// should report truncation rather than "invalid JSON" (#528).
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("api error (status {status}): {message}")]
    Api { status: u16, message: String },
    #[error("decode error: {0}")]
    Decode(String),
}

impl LlmError {
    /// Whether the error is worth retrying. Transport blips (connection refused,
    /// timeout, reset) and overloaded/transient HTTP statuses (408, 429, 5xx) are
    /// transient; client errors (other 4xx) and decode failures are fatal and must
    /// surface immediately so the user fixes the request rather than retrying it.
    pub fn is_transient(&self) -> bool {
        match self {
            LlmError::Transport(_) => true,
            LlmError::Api { status, .. } => {
                *status == 408 || *status == 429 || (500..=599).contains(status)
            }
            LlmError::Decode(_) => false,
        }
    }
}

/// Like `reqwest::Response::error_for_status`, but on a non-2xx response it
/// reads the body into `LlmError::Api.message` instead of discarding it. The
/// surfaced body is what makes a provider's `{"code":...,"message":...}` 400
/// diagnosable rather than a bare "api error (status 400)". On success the
/// response is returned untouched and its body is left unread, so callers can
/// still consume it as a stream.
pub(crate) async fn error_for_status_with_body(
    resp: reqwest::Response,
) -> Result<reqwest::Response, LlmError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let code = status.as_u16();
    let mut message = resp.text().await.unwrap_or_default().trim().to_string();
    const MAX: usize = 2048;
    if message.len() > MAX {
        let end = (0..=MAX)
            .rev()
            .find(|&i| message.is_char_boundary(i))
            .unwrap_or(0);
        message.truncate(end);
        message.push_str("...[truncated]");
    }
    if message.is_empty() {
        message = status
            .canonical_reason()
            .unwrap_or("request failed")
            .to_string();
    }
    Err(LlmError::Api {
        status: code,
        message,
    })
}

pub type ChunkStream = BoxStream<'static, Result<Chunk, LlmError>>;

/// How a gateway expects prior-turn reasoning to be replayed on the assistant
/// turn that requested tool calls (#375). Default is `None` — vanilla OpenAI,
/// candle-vllm, LM Studio, Ollama strip reasoning silently and never want it
/// echoed. Hosted reasoning gateways (SiliconFlow, OpenRouter) reject or
/// degrade thinking mode when reasoning is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningWire {
    /// Drop reasoning on the wire (vanilla OpenAI / candle-vllm / Ollama / LM Studio).
    #[default]
    None,
    /// Re-inject as `reasoning_content` (SiliconFlow gateway).
    ReasoningContent,
    /// Re-inject as `reasoning` (OpenRouter gateway).
    Reasoning,
}

/// How a gateway represents an assistant tool-call turn whose `content` field
/// is empty. SiliconFlow's GLM/MiniMax models reject `content: null` (HTTP 400
/// `code 20015`) and require an empty string or omission; vanilla OpenAI, every
/// other SiliconFlow model, and OpenRouter accept all three forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolCallContent {
    /// Omit the field when content is empty (`#[serde(skip_serializing_if)]`
    /// already does this — the default).
    #[default]
    Omit,
    /// Emit `"content": ""` when content is empty (SiliconFlow GLM/MiniMax).
    EmptyString,
}

/// Per-connection wire-dialect choices for the OpenAI-compatible adapter. Pure
/// data; resolved once at provider build time and threaded through
/// `OpenAiProvider::message_to_wire`. Defaults are no-ops for every shipping
/// connection — only hosted reasoning gateways override them.
#[derive(Debug, Clone, Copy, Default)]
pub struct WireDialect {
    pub reasoning: ReasoningWire,
    pub tool_call_content: ToolCallContent,
}

/// Resolve a wire dialect from a connection's `(kind, vendor, model)`. Pure,
/// table-driven; called once at provider build time so the per-turn hot path
/// only sees a `Copy` struct. The mapping is documented in
/// `docs/rfcs/0015-provider-wire-dialects.md` §4.
pub fn wire_dialect(kind: ff_core::ProviderKind, vendor: Option<&str>, model: &str) -> WireDialect {
    use ff_core::ProviderKind as K;
    let model_lc = model.to_ascii_lowercase();
    let is_glm_or_minimax = model_lc.contains("glm") || model_lc.contains("minimax");
    let vendor_lc = vendor.map(|v| v.to_ascii_lowercase());
    let is_openrouter = vendor_lc.as_deref() == Some("openrouter");

    match kind {
        K::SiliconFlow => WireDialect {
            reasoning: ReasoningWire::ReasoningContent,
            tool_call_content: if is_glm_or_minimax {
                ToolCallContent::EmptyString
            } else {
                ToolCallContent::Omit
            },
        },
        // OpenRouter rides the OpenAi kind today; detect by vendor descriptor.
        K::OpenAi if is_openrouter => WireDialect {
            reasoning: ReasoningWire::Reasoning,
            tool_call_content: ToolCallContent::Omit,
        },
        K::CandleVllm | K::Ollama | K::Bedrock | K::OpenAi => WireDialect::default(),
    }
}

/// Reasoning *depth* dial (#394/#395). Defined in `ff-core` so it can be both a
/// [`ProviderConnection`](ff_core::ProviderConnection) settings field (exported to
/// TS) and consumed here by the providers; `ff-llm` depends on `ff-core`, so the
/// type cannot live in `ff-llm` without a dependency cycle. Re-exported so existing
/// `ff_llm::ReasoningEffort` / `crate::ReasoningEffort` paths keep resolving.
pub use ff_core::ReasoningEffort;

/// Per-gateway reasoning-cost controls for the OpenAI-compatible wire (#394).
/// Resolved once at provider build time, like [`WireDialect`]. The default emits
/// nothing, preserving vanilla OpenAI / candle-vllm / LM Studio / OpenRouter
/// behavior. Native-reasoning providers (Bedrock, Anthropic) take a
/// [`ReasoningEffort`] directly instead -- they have no gateway ambiguity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningControl {
    /// Emit no reasoning parameters.
    #[default]
    None,
    /// SiliconFlow gateway knobs (verified #394 across GLM-5.2, Kimi-K2.7-Code
    /// and DeepSeek-V4-Pro): `enable_thinking: false` turns reasoning fully off,
    /// and `thinking_budget` hard-caps reasoning tokens to the effort budget.
    /// Which knob is sent depends on [`ChatRequest::thinking`].
    SiliconFlow { effort: ReasoningEffort },
}

/// SiliconFlow models that run in a forced/always-on reasoning mode and reject
/// `enable_thinking` toggling (e.g. DeepSeek-R1, QwQ/QvQ). They get no controls
/// so a thinking-off turn never trips a 400; everything else on the gateway
/// honors the knobs (verified #394).
fn is_forced_reasoning_model(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.contains("-r1") || m.contains("qwq") || m.contains("qvq")
}

/// Resolve OpenAI-wire reasoning controls from a connection's `(kind, model)`
/// and the user's effort dial. Scoped to the SiliconFlow gateway -- the only
/// OpenAI-compatible one verified to honor `enable_thinking` / `thinking_budget`
/// as hard controls (#394) -- excluding forced-reasoning models; everything else
/// emits nothing.
pub fn reasoning_control(
    kind: ff_core::ProviderKind,
    model: &str,
    effort: ReasoningEffort,
) -> ReasoningControl {
    use ff_core::ProviderKind as K;
    match kind {
        K::SiliconFlow if !is_forced_reasoning_model(model) => {
            ReasoningControl::SiliconFlow { effort }
        }
        _ => ReasoningControl::None,
    }
}

/// Drop attachments a provider cannot carry (the capability strip, #332/#334,
/// #504). Vision and documents gate independently: a `Document` survives only
/// when `supports_documents`, an `Image` only when `supports_vision`, so a
/// Bedrock text-only model keeps PDFs while shedding images. Borrows on the
/// common path (every attachment is allowed) and only clones when a strip is
/// actually needed, so a text-only turn is zero-cost. Surviving attachments are
/// reshaped into the API's real content blocks by the per-provider adapter
/// (#335/#336/#337).
pub(crate) fn messages_for_wire(
    messages: &[ChatMessage],
    supports_vision: bool,
    supports_documents: bool,
) -> std::borrow::Cow<'_, [ChatMessage]> {
    use ff_core::AttachmentKind;
    let kept = |a: &ff_core::Attachment| match a.kind {
        AttachmentKind::Image => supports_vision,
        AttachmentKind::Document => supports_documents,
    };
    let needs_strip = messages
        .iter()
        .flat_map(|m| m.attachments.iter())
        .any(|a| !kept(a));
    if !needs_strip {
        std::borrow::Cow::Borrowed(messages)
    } else {
        std::borrow::Cow::Owned(
            messages
                .iter()
                .map(|m| {
                    let mut m = m.clone();
                    m.attachments.retain(&kept);
                    m
                })
                .collect(),
        )
    }
}

/// Materialize an attachment as raw bytes: read a `Path` from disk, or base64-decode
/// an `Inline` payload. Shared by the per-provider adapters (#335/#336/#337) -- Bedrock
/// sends these raw, the OpenAI-compatible adapter re-encodes them into a data URI.
pub(crate) fn attachment_bytes(a: &ff_core::Attachment) -> Result<Vec<u8>, String> {
    match &a.source {
        ff_core::AttachmentSource::Path(path) => {
            std::fs::read(path).map_err(|e| format!("read {path}: {e}"))
        }
        ff_core::AttachmentSource::Inline(b64) => base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("decode inline base64: {e}")),
    }
}

/// Canonical image media type a vision model accepts, or `None` for an unsupported
/// type. `supports_vision` being true doesn't guarantee every format is taken
/// (trust-boundary, #334), so an unrecognized type is skipped rather than sent.
/// Shared by the OpenAI-compatible (data URI) and Ollama (bare base64) adapters
/// (#336/#337); the returned canonical type is what a data URI should advertise.
pub(crate) fn image_media_type(media_type: &str) -> Option<&'static str> {
    match media_type.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some("image/png"),
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        _ => None,
    }
}

/// Conservative context-window fallback (in tokens) for a model whose family we
/// don't recognize. Defined in [`ff_core::model_specs`] (the schema's owner) and
/// re-exported here so existing `ff_llm::DEFAULT_CONTEXT_WINDOW_TOKENS` callers
/// keep working.
pub use ff_core::DEFAULT_CONTEXT_WINDOW_TOKENS;

/// Best-effort context window (in tokens) for a model id. Resolved from a
/// data-driven, layered rule set (see [`model_specs`]): bundled defaults seeded
/// from live probes, overlaid by an optional user `model-specs.json`. Matching
/// is a case-insensitive family substring, not the exact id, so new point
/// releases inherit the right window without a code change. Used to size the
/// agent's compaction budget so a large-window model isn't force-compacted at a
/// tiny fixed ceiling (and a small one isn't allowed to overflow). The window is
/// a property of the *model*, not the transport, so this is shared across
/// providers; a provider with a quirky deployment can still override
/// [`Provider::context_window`].
///
/// Values are the raw served context windows (verified against the SiliconFlow API
/// on 2026-06-24 for the open-weight families, official docs for Claude/OpenAI).
/// The agent applies its own headroom (`CONTEXT_BUDGET_SAFETY`) on top, so these
/// are stored undiscounted -- discounting here would double-count. Unknown
/// families fall through to [`DEFAULT_CONTEXT_WINDOW_TOKENS`].
pub fn model_context_window(model: &str) -> u64 {
    model_specs::lookup(model)
}

/// Output-token ceiling for a turn, sized to the model's context window minus the
/// estimated input and a safety buffer, capped at a generous ceiling. Pinning this
/// on the gateway path keeps a large tool-call payload (plus any thinking) from
/// being truncated at the provider's small default output cap (#550).
///
/// Returns `None` when the remaining headroom is too small to be worth pinning: a
/// tiny cap helps nothing, and relieving genuine context pressure is compaction's
/// job, not this knob's. The buffer matches SiliconFlow's guidance to reserve
/// headroom below the window; the ceiling mirrors the Bedrock adaptive pin (#529).
pub fn budgeted_max_output_tokens(model: &str, input_tokens: u64) -> Option<u32> {
    /// Headroom reserved below the context window for prompt growth and overhead.
    const OUTPUT_SAFETY_BUFFER: u64 = 10_240;
    /// Generous upper bound; matches `ADAPTIVE_THINKING_MAX_TOKENS` on the Bedrock path.
    const MAX_OUTPUT_CEIL: u64 = 32_768;
    /// Below this, skip the pin and let the provider default stand.
    const MIN_USEFUL_OUTPUT: u64 = 2_048;

    let ctx = model_context_window(model);
    let headroom = ctx
        .saturating_sub(input_tokens)
        .saturating_sub(OUTPUT_SAFETY_BUFFER);
    let capped = headroom.min(MAX_OUTPUT_CEIL);
    (capped >= MIN_USEFUL_OUTPUT).then_some(capped as u32)
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError>;

    /// The model's context window in tokens, used by the agent to size its
    /// compaction budget so a capable large-window model isn't penalized by a
    /// fixed ceiling. Defaults to the shared [`model_context_window`] family
    /// lookup; a provider with a non-standard deployment may override.
    fn context_window(&self, model: &str) -> u64 {
        model_context_window(model)
    }

    /// Whether the active model accepts image/document attachments. Hosts read
    /// this to warn the user when a turn's attachments will be stripped before
    /// they reach the model (#338) -- the capability strip itself is silent, so
    /// this is what turns a silent drop into a visible notice. Defaults true; the
    /// concrete providers override to report their connection's configured flag.
    fn supports_vision(&self) -> bool {
        true
    }

    /// Best-effort list of model ids the server currently has loaded. Used by the
    /// provider settings panel to populate the model picker; callers treat any
    /// error (server down, endpoint unsupported) as "no suggestions". Providers
    /// without a discovery endpoint keep the default (no suggestions).
    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        Ok(Vec::new())
    }

    /// Best-effort nudge to wake the server before the first real turn. Fires a
    /// tiny request and drains a few decode steps, then drops the stream (which
    /// aborts the request). On a local GPU backend this spins the device up out
    /// of its idle power state and JIT-compiles the decode kernels, so the
    /// user's first message does not pay the cold-start ramp. Callers ignore the
    /// result: warmup must never block a turn or surface an error to the UI.
    ///
    /// The default works for any streaming provider; `model` is the id the
    /// server expects (local backends generally ignore it).
    async fn warmup(&self, model: &str) -> Result<(), LlmError> {
        let req = ChatRequest {
            model: model.to_string(),
            messages: vec![ChatMessage::text("user", "ok")],
            tools: Vec::new(),
            thinking: false,
            max_tokens: None,
        };
        let mut stream = self.chat_stream(req).await?;
        // Draining ~32 decode steps is what it empirically takes for an idle
        // Apple-Silicon GPU to reach its full clock; fewer leaves it half-ramped
        // and the next real turn still stalls. Dropping the stream at end of
        // scope aborts the request so the server stops generating early.
        for _ in 0..32u8 {
            match stream.next().await {
                Some(Ok(chunk)) if !chunk.done => continue,
                _ => break,
            }
        }
        Ok(())
    }

    /// Probe that the endpoint is reachable and the active credentials work,
    /// backing the settings "Test Connection" button. The default is a no-op
    /// `Ok` (local backends need no auth); hosted providers override with a real
    /// round-trip. `model` is the connection's configured model, for providers
    /// whose probe needs one (e.g. a Bedrock converse-probe).
    async fn test_connection(&self, _model: &str) -> Result<(), LlmError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[test]
    fn model_context_window_maps_known_families_and_defaults() {
        // Family substrings (case-insensitive), not exact ids, so point releases inherit.
        // Values are the raw served windows probed on 2026-06-24.
        assert_eq!(model_context_window("zai-org/GLM-5.2"), 1_048_576);
        assert_eq!(model_context_window("zai-org/GLM-5"), 202_752);
        assert_eq!(model_context_window("zai-org/GLM-4.5"), 131_072);
        assert_eq!(
            model_context_window("deepseek-ai/DeepSeek-V4-Pro"),
            1_000_000
        );
        assert_eq!(model_context_window("deepseek-ai/DeepSeek-V3.2"), 163_840);
        assert_eq!(model_context_window("moonshotai/Kimi-K2.7-Code"), 262_144);
        assert_eq!(model_context_window("MiniMaxAI/MiniMax-M3"), 700_000);
        assert_eq!(model_context_window("MiniMaxAI/MiniMax-M2.5"), 196_608);
        assert_eq!(model_context_window("anthropic.claude-opus-4"), 200_000);
        assert_eq!(model_context_window("gpt-4o-mini"), 128_000);
        // Unknown family falls back to the conservative default.
        assert_eq!(
            model_context_window("some-local-7b"),
            DEFAULT_CONTEXT_WINDOW_TOKENS
        );
    }

    #[test]
    fn budgeted_max_output_tokens_pins_the_ceiling_for_a_large_window() {
        // GLM-5.2 has a 1M window; light input leaves plenty of room, so the cap is
        // the generous ceiling rather than the full (huge) headroom.
        assert_eq!(
            budgeted_max_output_tokens("zai-org/GLM-5.2", 5_000),
            Some(32_768)
        );
    }

    #[test]
    fn budgeted_max_output_tokens_scales_down_as_context_fills() {
        // gpt-4o-mini: 128k window, 100k input. 128_000 - 100_000 - 10_240 = 17_760,
        // which is below the ceiling, so the pin tracks the remaining headroom.
        assert_eq!(
            budgeted_max_output_tokens("gpt-4o-mini", 100_000),
            Some(17_760)
        );
    }

    #[test]
    fn budgeted_max_output_tokens_is_none_when_headroom_is_tiny() {
        // gpt-4o-mini with input near the window: 128_000 - 126_000 - 10_240 saturates
        // below MIN_USEFUL_OUTPUT, so we skip the pin and let the provider default
        // stand (relieving real context pressure is compaction's job).
        assert_eq!(budgeted_max_output_tokens("gpt-4o-mini", 126_000), None);
    }

    #[test]
    fn budgeted_max_output_tokens_never_exceeds_a_small_window() {
        // A small unknown-family model (default window) with the safety buffer alone
        // consuming most of it must not be pinned above what it can serve; here the
        // remaining headroom is below the useful floor, so None.
        let ctx = model_context_window("some-local-7b");
        let near_full = ctx.saturating_sub(1_000);
        assert_eq!(budgeted_max_output_tokens("some-local-7b", near_full), None);
    }

    /// GLM-4.5-Air must NOT inherit a generic `glm` window: its served cap (98,304)
    /// is below the budget the old flat 128K rule produced (128_000 * 0.8 = 102,400),
    /// which would have pushed the agent's budget *above* the real window and let the
    /// request overflow before compaction ever engaged. The more specific rule wins.
    #[test]
    fn glm_4_5_air_is_not_oversized_by_generic_glm_rule() {
        assert_eq!(model_context_window("zai-org/GLM-4.5-Air"), 98_304);
        assert_ne!(model_context_window("zai-org/GLM-4.5-Air"), 131_072);
    }

    /// Regression guard: no rule may report a window larger than the cap the
    /// provider actually serves. A budget computed from an oversized window never
    /// triggers compaction in time, so this catches the GLM-4.5-Air class of bug
    /// for every family we have probed.
    #[test]
    fn no_family_window_exceeds_probed_served_cap() {
        // (model id, served `max_prompt_tokens` measured against the live API)
        let probed: &[(&str, u64)] = &[
            ("zai-org/GLM-5.2", 1_048_576),
            ("zai-org/GLM-5.1", 202_752),
            ("zai-org/GLM-5", 202_752),
            ("zai-org/GLM-5V-Turbo", 202_752),
            ("zai-org/GLM-4.5-Air", 98_304),
            ("deepseek-ai/DeepSeek-V4-Pro", 1_000_000),
            ("deepseek-ai/DeepSeek-V4-Flash", 1_048_576),
            ("deepseek-ai/DeepSeek-V3.2", 163_840),
            ("moonshotai/Kimi-K2.7-Code", 262_144),
            ("MiniMaxAI/MiniMax-M3", 700_000),
            ("MiniMaxAI/MiniMax-M2.5", 196_608),
        ];
        for (model, served) in probed {
            assert!(
                model_context_window(model) <= *served,
                "{model}: reported window {} exceeds served cap {served}",
                model_context_window(model),
            );
        }
    }

    #[test]
    fn context_window_trait_default_delegates_to_family_lookup() {
        let p = OpenAiProvider::candle_vllm();
        assert_eq!(p.context_window("zai-org/GLM-5.2"), 1_048_576);
        assert_eq!(p.context_window("unknown"), DEFAULT_CONTEXT_WINDOW_TOKENS);
    }

    fn img_attachment() -> ff_core::Attachment {
        ff_core::Attachment {
            kind: ff_core::AttachmentKind::Image,
            media_type: "image/png".into(),
            source: ff_core::AttachmentSource::Inline("aGk=".into()),
            name: None,
            bytes: 2,
        }
    }

    #[test]
    fn messages_for_wire_strips_attachments_when_no_vision() {
        let msg = ChatMessage::multimodal("user", "see this", vec![img_attachment()]);
        let stripped = messages_for_wire(std::slice::from_ref(&msg), false, false);
        assert!(stripped[0].attachments.is_empty());
        // A stripped, text-only message serializes without an `attachments` key.
        let v = serde_json::to_value(&stripped[0]).unwrap();
        assert!(v.get("attachments").is_none());
    }

    #[test]
    fn messages_for_wire_keeps_attachments_when_vision() {
        let msg = ChatMessage::multimodal("user", "see this", vec![img_attachment()]);
        let kept = messages_for_wire(std::slice::from_ref(&msg), true, false);
        assert_eq!(kept[0].attachments.len(), 1);
    }

    #[test]
    fn messages_for_wire_borrows_text_only_path() {
        let msgs = vec![ChatMessage::text("user", "hi")];
        // No attachments anywhere -> borrowed (zero-copy), regardless of the flag.
        assert!(matches!(
            messages_for_wire(&msgs, false, false),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    fn doc_attachment() -> ff_core::Attachment {
        ff_core::Attachment {
            kind: ff_core::AttachmentKind::Document,
            media_type: "application/pdf".into(),
            source: ff_core::AttachmentSource::Inline("aGk=".into()),
            name: Some("report.pdf".into()),
            bytes: 2,
        }
    }

    #[test]
    fn messages_for_wire_gates_image_and_document_independently() {
        let msg = ChatMessage::multimodal(
            "user",
            "see these",
            vec![img_attachment(), doc_attachment()],
        );

        let vision_only = messages_for_wire(std::slice::from_ref(&msg), true, false);
        assert_eq!(vision_only[0].attachments.len(), 1);
        assert_eq!(
            vision_only[0].attachments[0].kind,
            ff_core::AttachmentKind::Image
        );

        let docs_only = messages_for_wire(std::slice::from_ref(&msg), false, true);
        assert_eq!(docs_only[0].attachments.len(), 1);
        assert_eq!(
            docs_only[0].attachments[0].kind,
            ff_core::AttachmentKind::Document
        );

        let neither = messages_for_wire(std::slice::from_ref(&msg), false, false);
        assert!(neither[0].attachments.is_empty());

        assert!(matches!(
            messages_for_wire(std::slice::from_ref(&msg), true, true),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    struct EndlessProvider {
        polled: Arc<AtomicUsize>,
        first_role: Mutex<Option<String>>,
    }

    #[async_trait]
    impl Provider for EndlessProvider {
        async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
            *self.first_role.lock().unwrap() = req.messages.first().map(|m| m.role.clone());
            let polled = self.polled.clone();
            let stream = futures_util::stream::unfold(0usize, move |i| {
                let polled = polled.clone();
                async move {
                    polled.fetch_add(1, Ordering::SeqCst);
                    let chunk = Chunk {
                        delta: "x".into(),
                        reasoning_delta: String::new(),
                        tool_calls: vec![],
                        done: false,
                        truncated: false,
                    };
                    Some((Ok(chunk), i + 1))
                }
            });
            Ok(stream.boxed())
        }
    }

    #[tokio::test]
    async fn test_connection_default_is_ok() {
        let provider = EndlessProvider {
            polled: Arc::new(AtomicUsize::new(0)),
            first_role: Mutex::new(None),
        };
        provider.test_connection("test-model").await.unwrap();
    }

    #[tokio::test]
    async fn warmup_sends_one_user_turn_and_stops_early() {
        let provider = EndlessProvider {
            polled: Arc::new(AtomicUsize::new(0)),
            first_role: Mutex::new(None),
        };
        provider.warmup("test-model").await.unwrap();
        assert_eq!(provider.first_role.lock().unwrap().as_deref(), Some("user"));
        // Bounded: warmup never drains the endless stream.
        assert!(
            provider.polled.load(Ordering::SeqCst) <= 32,
            "warmup drained too many chunks"
        );
    }

    #[test]
    fn transient_errors_are_retryable() {
        assert!(LlmError::Transport("connection refused".into()).is_transient());
        for status in [408u16, 429, 500, 502, 503, 504] {
            assert!(
                LlmError::Api {
                    status,
                    message: "x".into()
                }
                .is_transient(),
                "status {status} should be transient"
            );
        }
    }

    #[test]
    fn client_and_decode_errors_are_fatal() {
        for status in [400u16, 401, 403, 404, 422] {
            assert!(
                !LlmError::Api {
                    status,
                    message: "x".into()
                }
                .is_transient(),
                "status {status} should be fatal"
            );
        }
        assert!(!LlmError::Decode("bad json".into()).is_transient());
    }

    // ---- #375 PR-2: wire-dialect selector + carrier hygiene ----

    #[test]
    fn wire_dialect_defaults_for_local_and_vanilla_gateways() {
        use ff_core::ProviderKind as K;
        for kind in [K::CandleVllm, K::Ollama, K::Bedrock] {
            let d = wire_dialect(kind, None, "any-model");
            assert_eq!(d.reasoning, ReasoningWire::None, "{kind:?}");
            assert_eq!(d.tool_call_content, ToolCallContent::Omit, "{kind:?}");
        }
        // Vanilla OpenAI (no vendor descriptor) is also a no-op.
        let d = wire_dialect(K::OpenAi, None, "gpt-4o-mini");
        assert_eq!(d.reasoning, ReasoningWire::None);
        assert_eq!(d.tool_call_content, ToolCallContent::Omit);
    }

    #[test]
    fn wire_dialect_siliconflow_replays_reasoning_content() {
        // Confirmed empirically against api.siliconflow.com: DeepSeek thinking
        // mode returns intermittent HTTP 400 (code 20015) without this echo.
        let d = wire_dialect(
            ff_core::ProviderKind::SiliconFlow,
            None,
            "deepseek-ai/DeepSeek-V4-Pro",
        );
        assert_eq!(d.reasoning, ReasoningWire::ReasoningContent);
        assert_eq!(d.tool_call_content, ToolCallContent::Omit);
    }

    #[test]
    fn wire_dialect_siliconflow_glm_minimax_use_empty_string() {
        // Confirmed empirically: GLM-5.2 returns 20015 "content cannot be null"
        // when an assistant tool-call message omits content; "" is accepted.
        for model in ["zai-org/GLM-5.2", "MiniMax/MiniMax-M3"] {
            let d = wire_dialect(ff_core::ProviderKind::SiliconFlow, None, model);
            assert_eq!(d.reasoning, ReasoningWire::ReasoningContent, "{model}");
            assert_eq!(d.tool_call_content, ToolCallContent::EmptyString, "{model}");
        }
    }

    #[test]
    fn wire_dialect_openrouter_replays_reasoning_field() {
        // OpenRouter rides the OpenAi kind today; vendor descriptor selects the dialect.
        let d = wire_dialect(
            ff_core::ProviderKind::OpenAi,
            Some("openrouter"),
            "anthropic/claude-3.7-sonnet:thinking",
        );
        assert_eq!(d.reasoning, ReasoningWire::Reasoning);
        assert_eq!(d.tool_call_content, ToolCallContent::Omit);
    }

    #[test]
    fn reasoning_control_targets_all_siliconflow_except_forced_reasoning() {
        use ff_core::ProviderKind as K;
        // Verified #394: GLM-5.2, Kimi-K2.7-Code and DeepSeek-V4-Pro all honor
        // the gateway knobs. The effort dial selects the cap, default Medium.
        for model in [
            "zai-org/GLM-5.2",
            "moonshotai/Kimi-K2.7-Code",
            "deepseek-ai/DeepSeek-V4-Pro",
        ] {
            assert_eq!(
                reasoning_control(K::SiliconFlow, model, ReasoningEffort::Medium),
                ReasoningControl::SiliconFlow {
                    effort: ReasoningEffort::Medium
                },
                "{model}"
            );
        }
        // The effort dial flows through unchanged.
        assert_eq!(
            reasoning_control(K::SiliconFlow, "zai-org/GLM-5.2", ReasoningEffort::Low),
            ReasoningControl::SiliconFlow {
                effort: ReasoningEffort::Low
            }
        );
        // Forced-reasoning models (DeepSeek-R1, QwQ) reject enable_thinking, so
        // they are left alone.
        for model in ["deepseek-ai/DeepSeek-R1", "Qwen/QwQ-32B"] {
            assert_eq!(
                reasoning_control(K::SiliconFlow, model, ReasoningEffort::Medium),
                ReasoningControl::None,
                "{model}"
            );
        }
        // Other gateways never emit SiliconFlow-specific params.
        assert_eq!(
            reasoning_control(K::OpenAi, "gpt-4o", ReasoningEffort::High),
            ReasoningControl::None
        );
        assert_eq!(
            reasoning_control(
                K::CandleVllm,
                "any-glm-named-local",
                ReasoningEffort::Medium
            ),
            ReasoningControl::None
        );
    }

    #[test]
    fn reasoning_effort_budgets_match_frontend_dial() {
        assert_eq!(ReasoningEffort::Low.budget_tokens(), 1024);
        assert_eq!(ReasoningEffort::Medium.budget_tokens(), 4096);
        assert_eq!(ReasoningEffort::High.budget_tokens(), 8192);
        assert_eq!(ReasoningEffort::default(), ReasoningEffort::Medium);
        // Every level sits in the Anthropic/Bedrock valid range [1024, 32000).
        for e in [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
        ] {
            assert!((1024..32_000).contains(&e.budget_tokens()), "{e:?}");
        }
    }

    #[test]
    fn chat_message_reasoning_is_never_serialized_through_derive() {
        // The carrier MUST be #[serde(skip)] -- openai::message_to_wire calls
        // serde_json::to_value(msg) and would otherwise leak this field on every
        // gateway, breaking vanilla OpenAI which rejects unknown fields.
        let mut msg = ChatMessage::text("assistant", "");
        msg.reasoning = Some("chain of thought".to_string());
        let v = serde_json::to_value(&msg).unwrap();
        assert!(
            v.get("reasoning").is_none(),
            "reasoning leaked through derive: {v}"
        );
        assert!(v.get("reasoning_content").is_none());
    }
}
