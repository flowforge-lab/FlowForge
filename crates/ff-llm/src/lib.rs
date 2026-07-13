//! LLM provider abstraction. M1 ships two providers behind a single trait:
//! [`OpenAiProvider`] (OpenAI-compatible SSE — candle-vllm, vLLM, LM Studio, OpenAI)
//! and [`OllamaProvider`] (Ollama-native NDJSON `/api/chat`). [`BedrockProvider`]
//! (AWS Converse) and [`AnthropicProvider`] (native Messages API) land behind the
//! same trait.

mod anthropic;
mod bedrock;
mod extract;
mod model_specs;
mod ollama;
mod openai;
pub mod think_scanner;

use async_trait::async_trait;
use base64::Engine as _;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

pub use anthropic::AnthropicProvider;
pub use bedrock::{BedrockCreds, BedrockProvider};
pub use ollama::{
    ollama_num_ctx_from_env, resolve_served_window, OllamaProvider, ServedWindowProbe,
};
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

/// Idle-read budget for the local Ollama provider, far wider than
/// [`IDLE_READ_TIMEOUT_SECS`]. Ollama's `/api/chat` can sit silent well past 30s
/// before the first byte: a cold model load (multi-GB weights off disk) or a
/// large prompt's prefill both run before any token streams back, and reqwest's
/// `read_timeout` covers that wait, not just gaps between bytes.
///
/// Measured directly against a CPU-bound 35B-A3B model (`llama-server`'s own
/// `print_timing` log): a 2050-token prompt took 75s of prefill plus 53s to
/// generate 205 tokens -- 128s server-side, ~174s end-to-end including HTTP
/// overhead, for a prompt smaller than a typical pasted system prompt. A 180s
/// budget left only seconds of headroom and still failed on larger real-world
/// turns. Hosted-gateway SSE stalls (the reason [`IDLE_READ_TIMEOUT_SECS`]
/// exists at all) don't apply to a local daemon -- there is no hung-forever
/// failure mode to guard against, only genuinely slow hardware -- so Ollama
/// gets a generous 10-minute budget instead of sharing the hosted-API one.
pub(crate) const OLLAMA_IDLE_READ_TIMEOUT_SECS: u64 = 600;

/// Dedicated reqwest client for [`OllamaProvider`], separate from
/// [`build_streaming_http_client`] so a slow local prefill/model-load never
/// races the hosted-API stall guard. Same connect budget, its own process-wide
/// singleton so connections are still pooled across turns.
///
/// [`OllamaProvider`]: crate::OllamaProvider
pub(crate) fn build_ollama_http_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
                .read_timeout(Duration::from_secs(OLLAMA_IDLE_READ_TIMEOUT_SECS))
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
    /// When true, the provider places cache breakpoints on conversation messages
    /// (penultimate + index 0) so the growing history prefix is cached across turns.
    /// Only effective on Anthropic and Bedrock providers that support prompt caching.
    pub cache_messages: bool,
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
    /// Prompt prefix cache metrics from the provider (#766). Populated on the
    /// final chunk of providers that report usage (OpenAI-compatible with
    /// `stream_options.include_usage`, Anthropic `message_delta`). Zero when
    /// the provider doesn't report or caching didn't fire.
    pub cache_hit_tokens: u32,
    pub cache_miss_tokens: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("api error (status {status}): {message}")]
    Api { status: u16, message: String },
    /// The gateway rejected the request for exceeding a rate/usage limit (HTTP
    /// 429, or a 422 whose body matches a rate-limit signature). Unlike a generic
    /// `Api` error this is transient, and it carries the gateway's `Retry-After`
    /// delay when one was sent so the agent loop can wait out the window instead
    /// of burning its transport-blip retry budget in milliseconds (#571).
    #[error("rate limited{}: {message}", retry_after.map(|d| format!(" (retry after {}s)", d.as_secs())).unwrap_or_default())]
    RateLimited {
        retry_after: Option<std::time::Duration>,
        message: String,
    },
    #[error("decode error: {0}")]
    Decode(String),
}

impl LlmError {
    /// Whether the error is worth retrying. Transport blips (connection refused,
    /// timeout, reset), rate-limit windows, and overloaded/transient HTTP statuses
    /// (408, 429, 5xx) are transient; client errors (other 4xx) and decode
    /// failures are fatal and must surface immediately so the user fixes the
    /// request rather than retrying it.
    pub fn is_transient(&self) -> bool {
        match self {
            LlmError::Transport(_) => true,
            LlmError::RateLimited { .. } => true,
            LlmError::Api { status, .. } => {
                *status == 408 || *status == 429 || (500..=599).contains(status)
            }
            LlmError::Decode(_) => false,
        }
    }
}

/// Upper bound on how many bytes of an error body are read off the wire before
/// the stream is stopped. Comfortably above the 2 KB `MAX` truncation applied
/// afterwards, so the surfaced message is identical for any realistic error
/// body while bounding peak memory. (#517 nit 2)
const ERROR_BODY_READ_LIMIT: usize = 4096;

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
    // Headers must be read before the body is consumed.
    let retry_after = parse_retry_after(resp.headers());
    // Read the body with a hard byte ceiling rather than `resp.text()`, which
    // buffers the entire body into memory before any truncation. A pathological
    // multi-GB error body (only a risk if this is ever pointed at an untrusted
    // gateway) would otherwise be fully buffered before the 2 KB cap below. The
    // read stops after `ERROR_BODY_READ_LIMIT` bytes — comfortably above the
    // `MAX` truncation point — so the surfaced message is identical for any
    // realistic error body while bounding peak memory. (#517 nit 2)
    let mut message = read_bounded_body(resp, ERROR_BODY_READ_LIMIT)
        .await
        .trim()
        .to_string();
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
    // 429 is always a rate limit. 422 is rate-limited only when its body matches a
    // known limit signature (SiliconFlow returns 422 for some quota errors, #571);
    // a generic 422 (e.g. malformed request) stays a fatal `Api` error.
    if code == 429 || (code == 422 && is_rate_limit_body(&message)) {
        return Err(LlmError::RateLimited {
            retry_after,
            message,
        });
    }
    Err(LlmError::Api {
        status: code,
        message,
    })
}

/// Read at most `limit` bytes from a response body, lossy-decoding as UTF-8.
/// Stops reading once `limit` is reached, so an oversized body is never fully
/// buffered into memory. Mirrors the bounded-accumulate pattern used by
/// `ff_tools::web_fetch`. (#517 nit 2)
async fn read_bounded_body(resp: reqwest::Response, limit: usize) -> String {
    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                buf.extend_from_slice(&bytes);
                if buf.len() >= limit {
                    buf.truncate(limit);
                    break;
                }
            }
            // A transport error mid-body is treated like an early EOF: we surface
            // whatever we have so far rather than discarding it.
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Parse a `Retry-After` header into a delay. Supports both forms from RFC 9110:
/// delta-seconds (`Retry-After: 30`) and an HTTP-date (`Retry-After: Wed, 21 Oct
/// 2026 07:28:00 GMT`), the latter converted to a delay from now (saturating at
/// zero for a past date). Returns `None` when the header is absent or unparseable.
pub(crate) fn parse_retry_after(
    headers: &reqwest::header::HeaderMap,
) -> Option<std::time::Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let raw = raw.trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(std::time::Duration::from_secs(secs));
    }
    let when = httpdate::parse_http_date(raw).ok()?;
    Some(
        when.duration_since(std::time::SystemTime::now())
            .unwrap_or(std::time::Duration::ZERO),
    )
}

/// Whether an error body looks like a rate/usage-limit rejection, used to
/// classify ambiguous 422s. Conservative substring match (case-insensitive) on
/// the signatures gateways use; a false negative just falls back to a fatal
/// `Api` error, a false positive only grants a retry.
pub(crate) fn is_rate_limit_body(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    b.contains("rate limit")
        || b.contains("rate_limit")
        || b.contains("ratelimit")
        || b.contains("too many requests")
        || b.contains("tpm")
        || b.contains("rpm")
        || b.contains("quota")
        // Bare "exceeded" is too broad — "maximum context length exceeded" is a
        // common non-retryable 422 — so require it to co-occur with a limit term.
        || (b.contains("exceeded") && b.contains("limit"))
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
    /// If true, the model inlines chain-of-thought as `<think>...</think>` in the
    /// content stream instead of using the `reasoning_content` field. The stream
    /// parser will split these tags into the reasoning channel. (#729)
    pub think_tags: bool,
}

/// Resolve a wire dialect from a connection's `(kind, vendor, model)`. Pure,
/// table-driven; called once at provider build time so the per-turn hot path
/// only sees a `Copy` struct. The mapping is documented in
/// `docs/rfcs/0015-provider-wire-dialects.md` §4.
pub fn wire_dialect(
    kind: ff_core::ProviderKind,
    _vendor: Option<&str>,
    model: &str,
) -> WireDialect {
    use ff_core::ProviderKind as K;
    let model_lc = model.to_ascii_lowercase();
    let is_glm_or_minimax = model_lc.contains("glm") || model_lc.contains("minimax");

    let is_minimax = model_lc.contains("minimax");

    match kind {
        K::SiliconFlow => WireDialect {
            reasoning: ReasoningWire::ReasoningContent,
            tool_call_content: if is_glm_or_minimax {
                ToolCallContent::EmptyString
            } else {
                ToolCallContent::Omit
            },
            // MiniMax inlines reasoning as <think> tags in content (#729).
            // GLM/Kimi/DeepSeek use reasoning_content correctly — no splitting.
            think_tags: is_minimax,
        },
        // OpenRouter: reasoning field (not reasoning_content), no think tags.
        K::OpenRouter => WireDialect {
            reasoning: ReasoningWire::Reasoning,
            tool_call_content: ToolCallContent::Omit,
            think_tags: false,
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

    /// The model's context window in tokens -- the denominator the agent sizes
    /// its compaction budget from, so a capable large-window model isn't
    /// penalized by a fixed ceiling. Defaults to the shared
    /// [`model_context_window`] family lookup; a provider with a non-standard
    /// deployment may override. For backends that serve a window smaller than the
    /// trained maximum (e.g. Ollama's `OLLAMA_CONTEXT_LENGTH`), the host primes
    /// the served value via [`set_context_budget`](Provider::set_context_budget)
    /// before each turn; until that value is known the override reports a
    /// conservative default so the budget under-fills rather than overflows.
    fn context_window(&self, model: &str) -> u64 {
        model_context_window(model)
    }

    /// Prime the served context window the agent should budget against, when the
    /// host has probed a value the provider cannot know synchronously (#612).
    /// Defaults to a no-op: providers whose [`context_window`](Provider::context_window)
    /// is self-contained ignore it. Ollama overrides it to feed the `/api/ps`
    /// probed window into its budget when no explicit `num_ctx` was requested.
    fn set_context_budget(&mut self, _window: Option<u64>) {}

    /// Whether the active model accepts image attachments. Hosts read this to
    /// warn the user when a turn's image attachments will be stripped before they
    /// reach the model (#338) -- the capability strip itself is silent, so this is
    /// what turns a silent drop into a visible notice. Defaults true; the concrete
    /// providers override to report their connection's configured flag.
    fn supports_vision(&self) -> bool {
        true
    }

    /// Whether the active model accepts *document* attachments. Independent of
    /// [`supports_vision`](Self::supports_vision): a text-only model may still
    /// read documents (Bedrock via native `DocumentBlock`, OpenAI/Ollama via the
    /// text-extraction fallback #338 follow-up). Hosts read this alongside
    /// `supports_vision` to scope the `AttachmentsDropped` notice to only the
    /// attachments truly dropped. Defaults false (fail-closed); the concrete
    /// providers override to report their connection's configured flag.
    fn supports_documents(&self) -> bool {
        false
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
    /// Decode steps `warmup` drains before dropping the stream. The default 32 is
    /// candle-vLLM's empirical GPU-clock ramp on Apple Silicon (#55): fewer leaves
    /// the device half-clocked and the first real turn still stalls. Providers whose
    /// warmth is residency-based rather than clock-based override this to a light
    /// touch -- Ollama keeps the model resident via `keep_alive` (#634), so a single
    /// chunk already confirms the load started and resets the TTL; draining 32 tokens
    /// there is pure waste (#61).
    fn warmup_ramp_steps(&self) -> u8 {
        32
    }

    async fn warmup(&self, model: &str) -> Result<(), LlmError> {
        let req = ChatRequest {
            model: model.to_string(),
            messages: vec![ChatMessage::text("user", "ok")],
            tools: Vec::new(),
            thinking: false,
            max_tokens: None,
            cache_messages: false,
        };
        let mut stream = self.chat_stream(req).await?;
        // Drain the provider's ramp depth (`warmup_ramp_steps`), then drop the
        // stream at end of scope, which aborts the request so the server stops
        // generating early. candle-vLLM needs ~32 steps to reach full GPU clock;
        // Ollama overrides to a light residency touch (#61).
        for _ in 0..self.warmup_ramp_steps() {
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
mod tests;
