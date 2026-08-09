use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;

use base64::Engine as _;

use crate::{
    ChatMessage, ChatRequest, Chunk, ChunkStream, LlmError, Provider, ReasoningControl,
    ReasoningWire, ToolCallContent, ToolCallDelta, WireDialect,
};
use ff_core::ProviderKind;

/// Talks to any OpenAI-compatible `/v1/chat/completions` server over Server-Sent
/// Events. candle-vllm, vLLM, LM Studio, Ollama's `/v1` shim, and OpenAI itself all
/// speak this protocol, so switching backends is a `base_url` change with no code edits.
pub struct OpenAiProvider {
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
    /// Whether the connection's model accepts attachments (#332/#334). When
    /// false, attachments are stripped before serialization so a raw `attachments`
    /// field never reaches the wire (this provider serializes `ChatMessage`
    /// directly). Defaults false; set via [`OpenAiProvider::with_vision`].
    supports_vision: bool,
    /// Whether the connection's model accepts *document* attachments via the
    /// text-extraction fallback (#338 follow-up). The OpenAI chat-completions
    /// wire has no portable document block, so when this is true the adapter
    /// extracts each document's text client-side and folds it into the user
    /// message's `content` before serialization. Defaults false — the #338
    /// skip stays the no-extraction default; the host opts in via
    /// [`OpenAiProvider::with_documents`] from the resolved model capability.
    supports_documents: bool,
    /// Per-gateway wire-shape choices (#375). Defaults are no-ops for vanilla
    /// OpenAI / candle-vllm / Ollama-compat / LM Studio; SiliconFlow and
    /// OpenRouter override to re-inject prior reasoning on tool-call turns and
    /// (for SiliconFlow GLM/MiniMax) emit `content: ""` instead of omitting it.
    /// See `crate::wire_dialect`.
    dialect: WireDialect,
    /// Per-gateway reasoning-cost controls (#394). Default emits nothing; the
    /// SiliconFlow gateway caps reasoning tokens (per effort) / disables thinking.
    /// Resolved at build time via [`crate::reasoning_control`].
    reasoning: ReasoningControl,
    /// Which kind of backend this provider talks to (#888). Used by the agent
    /// loop to surface the `egress=local-only`-but-cloud-model warning when the
    /// resolved phenotype is `LocalOnly` but this connection is hosted. Defaults
    /// to [`ProviderKind::OpenAi`] (vanilla OpenAI / candle-vllm / LM Studio);
    /// SiliconFlow and OpenRouter override via [`OpenAiProvider::with_kind`]
    /// at construction time so the warning fires correctly.
    kind: ProviderKind,
}

impl OpenAiProvider {
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            client: crate::build_streaming_http_client(),
            supports_vision: false,
            supports_documents: false,
            dialect: WireDialect::default(),
            reasoning: ReasoningControl::default(),
            kind: ProviderKind::OpenAi,
        }
    }

    /// Declare which backend this provider actually talks to (#888). The OpenAI-
    /// compatible adapter serves every OpenAI-compatible endpoint — vanilla OpenAI,
    /// candle-vLLM, LM Studio, SiliconFlow, OpenRouter — so the host must override
    /// the default at construction. Local kinds (`CandleVllm`, `Ollama`) suppress
    /// the egress-mismatch warning; hosted kinds (`OpenAi`, `SiliconFlow`,
    /// `OpenRouter`) fire it under `egress = local-only`.
    pub fn with_kind(mut self, kind: ProviderKind) -> Self {
        self.kind = kind;
        self
    }

    /// Declare whether the target model can accept image attachments.
    pub fn with_vision(mut self, supports_vision: bool) -> Self {
        self.supports_vision = supports_vision;
        self
    }

    /// Declare whether the target model can accept *document* attachments via
    /// the text-extraction fallback (#338 follow-up). When true, document
    /// attachments are extracted to text and folded into the user message's
    /// prompt context; when false (the default), they are stripped at the
    /// capability strip (the #338 skip). Mirrors [`BedrockProvider::with_documents`].
    pub fn with_documents(mut self, supports_documents: bool) -> Self {
        self.supports_documents = supports_documents;
        self
    }

    /// Set the per-gateway wire dialect (#375). Defaults to no-ops; resolve via
    /// [`crate::wire_dialect`] at provider build time.
    pub fn with_dialect(mut self, dialect: WireDialect) -> Self {
        self.dialect = dialect;
        self
    }

    /// Set the per-gateway reasoning-cost controls (#394). Defaults to a no-op;
    /// resolve via [`crate::reasoning_control`] at provider build time.
    pub fn with_reasoning_control(mut self, reasoning: ReasoningControl) -> Self {
        self.reasoning = reasoning;
        self
    }

    /// Local candle-vllm server (FlowForge M1 default, no credentials).
    pub fn candle_vllm() -> Self {
        Self::new("http://localhost:8000/v1", None)
    }

    /// Hosted OpenAI API (requires a bearer key).
    pub fn openai(api_key: impl Into<String>) -> Self {
        Self::new("https://api.openai.com/v1", Some(api_key.into()))
    }

    /// Ollama's OpenAI-compatible shim (distinct from its native NDJSON `/api/chat`).
    pub fn ollama_compat() -> Self {
        Self::new("http://localhost:11434/v1", None)
    }
}

impl Default for OpenAiProvider {
    fn default() -> Self {
        Self::candle_vllm()
    }
}

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    /// Usage stats from the final stream chunk (requires `stream_options.include_usage`).
    #[serde(default)]
    usage: Option<StreamUsage>,
}

#[derive(Deserialize)]
struct StreamUsage {
    #[serde(default)]
    prompt_tokens: u32,
    /// Completion (output) token count for the round-trip (#931).
    #[serde(default)]
    completion_tokens: u32,
    /// Legacy SiliconFlow field for cache hit tokens.
    #[serde(default)]
    prompt_cache_hit_tokens: Option<u32>,
    /// Legacy SiliconFlow field for cache miss tokens.
    #[serde(default)]
    prompt_cache_miss_tokens: Option<u32>,
    /// OpenAI-standard nested details.
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: Delta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Vec<StreamToolCall>,
}

#[derive(Deserialize, Default)]
struct StreamToolCall {
    #[serde(default)]
    index: u32,
    id: Option<String>,
    #[serde(default)]
    function: StreamFunction,
}

#[derive(Deserialize, Default)]
struct StreamFunction {
    name: Option<String>,
    // SiliconFlow `.com` sends `"arguments": null` in the opening tool-call delta
    // fragment (id/name only). `#[serde(default)]` covers a missing field but not an
    // explicit null, so map both to "" (#493).
    #[serde(default, deserialize_with = "null_to_empty_string")]
    arguments: String,
}

/// Deserialize a string that may arrive missing or explicitly `null` as `""`.
/// OpenAI-compatible gateways differ on the opening tool-call delta: some omit
/// `arguments`, some send `""`, and SiliconFlow sends `null` (#493).
fn null_to_empty_string<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize)]
struct ModelList {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

/// Parse one raw SSE line into an optional chunk.
///
/// Returns `None` for lines that carry no payload (blank lines, comments, non-`data:`
/// fields). `data: [DONE]` yields a terminal empty chunk.
fn parse_sse_line(line: &[u8]) -> Option<Result<Chunk, LlmError>> {
    let line = std::str::from_utf8(line).ok()?.trim();
    let payload = line.strip_prefix("data:")?.trim();

    if payload.is_empty() {
        return None;
    }
    if payload == "[DONE]" {
        return Some(Ok(Chunk {
            done: true,
            ..Chunk::default()
        }));
    }

    match serde_json::from_str::<StreamChunk>(payload) {
        Ok(parsed) => {
            // Extract cache metrics from usage (final chunk or inline).
            let (cache_hit, cache_miss) = parsed
                .usage
                .as_ref()
                .map(|u| {
                    // Prefer OpenAI-standard nested field; fall back to legacy SF fields.
                    let hit = u
                        .prompt_tokens_details
                        .as_ref()
                        .and_then(|d| d.cached_tokens)
                        .or(u.prompt_cache_hit_tokens)
                        .unwrap_or(0);
                    let miss = u
                        .prompt_cache_miss_tokens
                        .unwrap_or(u.prompt_tokens.saturating_sub(hit));
                    (hit, miss)
                })
                .unwrap_or((0, 0));

            let (input_tokens, output_tokens) = parsed
                .usage
                .as_ref()
                .map(|u| (u.prompt_tokens, u.completion_tokens))
                .unwrap_or((0, 0));

            let chunk = match parsed.choices.into_iter().next() {
                Some(c) => Chunk {
                    delta: c.delta.content.unwrap_or_default(),
                    reasoning_delta: c
                        .delta
                        .reasoning_content
                        .or(c.delta.reasoning)
                        .unwrap_or_default(),
                    tool_calls: c
                        .delta
                        .tool_calls
                        .into_iter()
                        .map(|tc| ToolCallDelta {
                            index: tc.index,
                            id: tc.id,
                            name: tc.function.name,
                            arguments: tc.function.arguments,
                        })
                        .collect(),
                    done: c.finish_reason.is_some(),
                    truncated: c.finish_reason.as_deref() == Some("length"),
                    cache_hit_tokens: cache_hit,
                    cache_miss_tokens: cache_miss,
                    input_tokens,
                    output_tokens,
                },
                None => Chunk {
                    cache_hit_tokens: cache_hit,
                    cache_miss_tokens: cache_miss,
                    input_tokens,
                    output_tokens,
                    ..Chunk::default()
                },
            };
            Some(Ok(chunk))
        }
        Err(e) => Some(Err(LlmError::Decode(e.to_string()))),
    }
}

/// Build an `image_url` data URI (`data:<media_type>;base64,<b64>`) for one image
/// attachment, or `None` (with a warning) when it can't be sent: an unsupported
/// media type, an unreadable file, or undecodable inline data. Skipping rather than
/// failing keeps one bad attachment from dropping the whole turn.
fn image_data_uri(a: &ff_core::Attachment) -> Option<String> {
    let Some(media_type) = crate::image_media_type(&a.media_type) else {
        tracing::warn!(media_type = %a.media_type, "skipping image attachment: unsupported media type for OpenAI");
        return None;
    };
    let bytes = crate::attachment_bytes(a)
        .map_err(|e| tracing::warn!(error = %e, "skipping image attachment"))
        .ok()?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Some(format!("data:{media_type};base64,{b64}"))
}

/// Reshape a message into its OpenAI wire object. A message with image attachments
/// gets the array `content` shape -- a text block (when non-empty) followed by one
/// `image_url` block per image; every other message keeps its plain-string
/// `content`, byte-identical to the text-only path. The internal `attachments`
/// field is always removed so it never leaks onto the wire. Document attachments
/// aren't representable as `image_url` and are skipped here (degrade handling, #338).
///
/// `dialect` carries the per-gateway tweaks (#375): re-inject prior reasoning
/// under the gateway's field name on assistant tool-call turns, and (for
/// SiliconFlow GLM/MiniMax) materialize an empty `content: ""` instead of
/// letting `serde(skip_serializing_if = "Option::is_none")` drop the key.
fn message_to_wire(msg: &ChatMessage, dialect: WireDialect) -> serde_json::Value {
    let mut value = serde_json::to_value(msg).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = value.as_object_mut() {
        obj.remove("attachments");
        crate::promote_mode_switch_marker(msg, obj);
    }
    let image_uris: Vec<String> = msg
        .attachments
        .iter()
        .filter_map(|a| match a.kind {
            ff_core::AttachmentKind::Image => image_data_uri(a),
            ff_core::AttachmentKind::Document => {
                tracing::warn!(media_type = %a.media_type, "skipping document attachment: OpenAI chat has no portable document block (#338)");
                None
            }
        })
        .collect();
    if !image_uris.is_empty() {
        let mut content = Vec::new();
        if let Some(text) = msg.content.as_deref().filter(|t| !t.is_empty()) {
            content.push(serde_json::json!({ "type": "text", "text": text }));
        }
        for uri in image_uris {
            content.push(serde_json::json!({ "type": "image_url", "image_url": { "url": uri } }));
        }
        if let Some(obj) = value.as_object_mut() {
            obj.insert("content".into(), serde_json::Value::Array(content));
        }
    }

    apply_dialect(&mut value, msg, dialect);
    value
}

/// Apply per-gateway wire dialect to an already-shaped message value (#375).
/// Two independent hooks, both gated on "this is an assistant message that
/// only carries tool calls":
/// 1. Reasoning replay: when the model sent reasoning the previous turn AND
///    this turn's history slot is the same assistant message that requested
///    tool calls, echo the reasoning back under the dialect's field name
///    (`reasoning_content` for SiliconFlow, `reasoning` for OpenRouter).
/// 2. Empty-content shape: SiliconFlow GLM/MiniMax reject `content: null`
///    on the tool-call turn (HTTP 400, code 20015) and require either an
///    empty string or omission. Default `Omit` lets `serde(skip_serializing_if)`
///    drop the field; `EmptyString` puts it back as `""`.
fn apply_dialect(value: &mut serde_json::Value, msg: &ChatMessage, dialect: WireDialect) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let is_assistant_tool_call = msg.role == "assistant" && msg.tool_calls.is_some();
    if !is_assistant_tool_call {
        return;
    }

    let field = match dialect.reasoning {
        ReasoningWire::None => None,
        ReasoningWire::ReasoningContent => Some("reasoning_content"),
        ReasoningWire::Reasoning => Some("reasoning"),
    };
    if let (Some(reasoning), Some(field)) =
        (msg.reasoning.as_deref().filter(|s| !s.is_empty()), field)
    {
        obj.insert(
            field.into(),
            serde_json::Value::String(reasoning.to_string()),
        );
    }

    if dialect.tool_call_content == ToolCallContent::EmptyString
        && msg.content.as_deref().is_none_or(str::is_empty)
        && !obj.contains_key("content")
    {
        obj.insert("content".into(), serde_json::Value::String(String::new()));
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        // The capability strip: drops images when `supports_vision` is false and
        // documents when `supports_documents` is false. When documents are
        // supported, they survive the strip and are then folded into the user
        // message's `content` as extracted text (#338 follow-up) — the OpenAI
        // wire has no portable document block, so this fold IS the wire path.
        let stripped =
            crate::messages_for_wire(&req.messages, self.supports_vision, self.supports_documents);
        // `messages_for_wire` borrows when no strip is needed; the fold clones
        // only when a message actually carries documents, so a text-only turn is
        // still zero-allocation on the borrow path.
        let messages: Vec<ChatMessage> = if self.supports_documents {
            crate::extract::fold_documents_into_text(&stripped)
        } else {
            stripped.into_owned()
        };
        let messages = crate::enforce_user_terminated(messages);
        let dialect = self.dialect;
        let wire_messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| message_to_wire(m, dialect))
            .collect();
        let mut body = serde_json::json!({
            "model": req.model,
            "messages": wire_messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        if !req.tools.is_empty() {
            body["tools"] = serde_json::Value::Array(req.tools.clone());
            body["tool_choice"] = serde_json::json!("auto");
        }
        // Pin the output ceiling so a large tool-call payload (plus any thinking)
        // is not cut off at the gateway's small default cap (#550). `max_tokens` is
        // the OpenAI-standard field accepted by every compatible backend (including
        // SiliconFlow), so it is safe to send whenever the caller sets one.
        if let Some(max_tokens) = req.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        // Reasoning-cost controls (#394). Only the SiliconFlow gateway emits
        // these; vanilla OpenAI / candle-vllm / LM Studio / OpenRouter would
        // reject unknown fields, so the default ReasoningControl::None is silent.
        match self.reasoning {
            ReasoningControl::None => {}
            ReasoningControl::SiliconFlow { effort } => {
                if req.thinking {
                    body["thinking_budget"] = serde_json::json!(effort.budget_tokens());
                } else {
                    body["enable_thinking"] = serde_json::json!(false);
                }
            }
        }

        let mut builder = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&body);
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        let resp = crate::error_for_status_with_body(resp).await?;

        // SSE frames are newline-delimited; reassemble lines across byte-chunk
        // boundaries, then decode each `data:` line into a Chunk.
        let think_tags = dialect.think_tags;
        let stream = resp.bytes_stream().scan(Vec::<u8>::new(), |buf, item| {
            let out = match item {
                Ok(bytes) => {
                    buf.extend_from_slice(&bytes);
                    let mut chunks = Vec::new();
                    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = buf.drain(..=pos).collect();
                        let line = &line[..line.len().saturating_sub(1)];
                        if let Some(chunk) = parse_sse_line(line) {
                            chunks.push(chunk);
                        }
                    }
                    chunks
                }
                Err(e) => vec![Err(LlmError::Transport(e.to_string()))],
            };
            std::future::ready(Some(futures_util::stream::iter(out)))
        });

        // Layer 1+2: split <think> tags from content into reasoning_delta (#729).
        // Layer 1 (forced): known think-tag models (MiniMax on SiliconFlow).
        // Layer 2 (auto-detect): any model whose first content starts with <think>.
        let mut scanner = if think_tags {
            crate::think_scanner::ThinkScanner::forced()
        } else {
            crate::think_scanner::ThinkScanner::auto_detect()
        };

        let stream = stream.flatten().map(move |result| {
            let mut chunk = result?;
            if !chunk.delta.is_empty() {
                let scan = scanner.push(&chunk.delta);
                chunk.delta = scan.content;
                if !scan.reasoning.is_empty() {
                    // Append to any existing reasoning_delta (unlikely but safe).
                    if chunk.reasoning_delta.is_empty() {
                        chunk.reasoning_delta = scan.reasoning;
                    } else {
                        chunk.reasoning_delta.push_str(&scan.reasoning);
                    }
                }
            }
            // On stream end, flush the scanner buffer.
            if chunk.done {
                let flush = scanner.flush();
                if !flush.content.is_empty() {
                    chunk.delta.push_str(&flush.content);
                }
                if !flush.reasoning.is_empty() {
                    chunk.reasoning_delta.push_str(&flush.reasoning);
                }
            }
            Ok(chunk)
        });

        Ok(stream.boxed())
    }

    fn supports_vision(&self) -> bool {
        self.supports_vision
    }

    fn supports_documents(&self) -> bool {
        self.supports_documents
    }

    fn kind(&self) -> ff_core::ProviderKind {
        self.kind
    }

    /// `GET {base_url}/models` -> `{ "data": [ { "id": ... } ] }`.
    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        let mut builder = self.client.get(format!("{}/models", self.base_url));
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        let resp = crate::error_for_status_with_body(resp).await?;
        let list: ModelList = resp
            .json()
            .await
            .map_err(|e| LlmError::Decode(e.to_string()))?;
        Ok(list.data.into_iter().map(|m| m.id).collect())
    }

    /// Probe the endpoint by listing models. A hosted, key-gated server (e.g.
    /// SiliconFlow) returns 401 on a bad key, surfacing as an [`LlmError::Api`]; a
    /// local server returns its catalog. Zero token cost, and the lightest call
    /// that exercises both the URL and the credentials -- so the settings "Test
    /// Connection" button means something for every OpenAI-compatible backend.
    ///
    /// Unlike `BedrockProvider::test_connection`, which fires a chat round-trip
    /// because a Bedrock token may be allowed to converse yet lack
    /// `bedrock:ListInferenceProfiles`, an OpenAI-compatible key gates `/models`
    /// and `/chat/completions` identically -- so `list_models` is a valid
    /// auth + reachability probe with no token cost, and no chat probe is needed.
    async fn test_connection(&self, _model: &str) -> Result<(), LlmError> {
        self.list_models().await.map(|_| ())
    }
}

#[cfg(test)]
mod tests;
