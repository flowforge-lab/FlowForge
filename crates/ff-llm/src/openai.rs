use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;

use base64::Engine as _;

use crate::{
    ChatMessage, ChatRequest, Chunk, ChunkStream, LlmError, Provider, ReasoningWire,
    ToolCallContent, ToolCallDelta, WireDialect,
};

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
    /// Per-gateway wire-shape choices (#375). Defaults are no-ops for vanilla
    /// OpenAI / candle-vllm / Ollama-compat / LM Studio; SiliconFlow and
    /// OpenRouter override to re-inject prior reasoning on tool-call turns and
    /// (for SiliconFlow GLM/MiniMax) emit `content: ""` instead of omitting it.
    /// See `crate::wire_dialect`.
    dialect: WireDialect,
}

impl OpenAiProvider {
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            client: reqwest::Client::new(),
            supports_vision: false,
            dialect: WireDialect::default(),
        }
    }

    /// Declare whether the target model can accept image/document attachments.
    pub fn with_vision(mut self, supports_vision: bool) -> Self {
        self.supports_vision = supports_vision;
        self
    }

    /// Set the per-gateway wire dialect (#375). Defaults to no-ops; resolve via
    /// [`crate::wire_dialect`] at provider build time.
    pub fn with_dialect(mut self, dialect: WireDialect) -> Self {
        self.dialect = dialect;
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
    #[serde(default)]
    arguments: String,
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
                },
                None => Chunk::default(),
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
    fn supports_vision(&self) -> bool {
        self.supports_vision
    }

    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let messages = crate::messages_for_wire(&req.messages, self.supports_vision);
        let dialect = self.dialect;
        let wire_messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| message_to_wire(m, dialect))
            .collect();
        let mut body = serde_json::json!({
            "model": req.model,
            "messages": wire_messages,
            "stream": true,
        });
        if !req.tools.is_empty() {
            body["tools"] = serde_json::Value::Array(req.tools.clone());
            body["tool_choice"] = serde_json::json!("auto");
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
            .map_err(|e| LlmError::Transport(e.to_string()))?
            .error_for_status()
            .map_err(|e| LlmError::Api {
                status: e.status().map(|s| s.as_u16()).unwrap_or(0),
                message: e.to_string(),
            })?;

        // SSE frames are newline-delimited; reassemble lines across byte-chunk
        // boundaries, then decode each `data:` line into a Chunk.
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

        Ok(stream.flatten().boxed())
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
            .map_err(|e| LlmError::Transport(e.to_string()))?
            .error_for_status()
            .map_err(|e| LlmError::Api {
                status: e.status().map(|s| s.as_u16()).unwrap_or(0),
                message: e.to_string(),
            })?;
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
mod tests {
    use super::*;

    #[test]
    fn parses_content_delta() {
        let line = br#"data: {"choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let chunk = parse_sse_line(line).unwrap().unwrap();
        assert_eq!(chunk.delta, "Hello");
        assert!(!chunk.done);
    }

    #[test]
    fn finish_reason_marks_done() {
        let line = br#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        let chunk = parse_sse_line(line).unwrap().unwrap();
        assert_eq!(chunk.delta, "");
        assert!(chunk.done);
    }

    #[test]
    fn done_sentinel_terminates() {
        let chunk = parse_sse_line(b"data: [DONE]").unwrap().unwrap();
        assert!(chunk.done);
        assert_eq!(chunk.delta, "");
    }

    #[test]
    fn blank_and_non_data_lines_skipped() {
        assert!(parse_sse_line(b"").is_none());
        assert!(parse_sse_line(b": keep-alive comment").is_none());
        assert!(parse_sse_line(b"event: message").is_none());
    }

    #[test]
    fn parses_tool_call_delta() {
        let line = br#"data: {"choices":[{"delta":{"content":null,"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"bash","arguments":"{\"command\":\"ls\"}"}}]},"finish_reason":null}]}"#;
        let chunk = parse_sse_line(line).unwrap().unwrap();
        assert_eq!(chunk.tool_calls.len(), 1);
        let tc = &chunk.tool_calls[0];
        assert_eq!(tc.index, 0);
        assert_eq!(tc.id.as_deref(), Some("call_1"));
        assert_eq!(tc.name.as_deref(), Some("bash"));
        assert!(tc.arguments.contains("\"command\""));
    }

    /// #374: SiliconFlow GLM-5.2 streams the name only in the first fragment, then
    /// sends `function.name: ""` (an empty string, not null/omitted) on every
    /// continuation fragment. This must decode to `Some("")`, not `None`, so the
    /// agent accumulator can recognise it as an empty continuation to ignore rather
    /// than clobbering the real name. (A vanilla provider omits the field -> `None`.)
    #[test]
    fn decodes_empty_string_continuation_name_as_some_empty() {
        let line = br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"","arguments":"{\"city\":"}}]},"finish_reason":null}]}"#;
        let chunk = parse_sse_line(line).unwrap().unwrap();
        let tc = &chunk.tool_calls[0];
        assert_eq!(tc.name.as_deref(), Some(""));
        assert_eq!(tc.arguments, "{\"city\":");
    }

    /// A continuation fragment that omits `name` entirely decodes to `None`, the
    /// vanilla-OpenAI shape -- distinct from GLM's empty-string above.
    #[test]
    fn decodes_omitted_continuation_name_as_none() {
        let line = br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Paris\"}"}}]},"finish_reason":null}]}"#;
        let chunk = parse_sse_line(line).unwrap().unwrap();
        assert_eq!(chunk.tool_calls[0].name, None);
    }

    #[test]
    fn tool_calls_finish_reason_marks_done() {
        let line =
            br#"data: {"choices":[{"delta":{"content":null},"finish_reason":"tool_calls"}]}"#;
        let chunk = parse_sse_line(line).unwrap().unwrap();
        assert!(chunk.done);
    }

    /// Guard: the OpenAI wire protocol requires `tool_calls[].function.arguments`
    /// to be a JSON-encoded **string**. The Ollama-native provider converts this
    /// to an object at its own boundary; this provider must NOT -- an object here
    /// is a 400 (`cannot unmarshal object into ... of type string`). This test
    /// fails loudly if anyone "unifies" the two and breaks OpenAI-compatible
    /// servers (OpenAI, candle-vLLM, LM Studio, Ollama's /v1 shim).
    #[test]
    fn tool_call_arguments_serialize_as_a_string() {
        use crate::{ChatMessage, FunctionCall, ToolCall};
        let msg = ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_0".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "write_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
            }]),
            tool_call_id: None,
            name: None,

            attachments: Vec::new(),
            reasoning: None,
        };
        let v = serde_json::to_value([msg]).unwrap();
        let args = &v[0]["tool_calls"][0]["function"]["arguments"];
        assert!(
            args.is_string(),
            "OpenAI arguments must stay a string, got {args}"
        );
    }

    // ---- #336 BE-4: OpenAI image_url data-URI content blocks ----

    use ff_core::{Attachment, AttachmentKind, AttachmentSource};

    fn inline_b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn image_attachment(media_type: &str, source: AttachmentSource) -> Attachment {
        Attachment {
            kind: AttachmentKind::Image,
            media_type: media_type.into(),
            source,
            name: Some("shot.png".into()),
            bytes: 4,
        }
    }

    #[test]
    fn multimodal_message_emits_image_url_block() {
        let msg = ChatMessage::multimodal(
            "user",
            "look",
            vec![image_attachment(
                "image/png",
                AttachmentSource::Inline(inline_b64(&[0x89, 0x50, 0x4e, 0x47])),
            )],
        );
        let v = message_to_wire(&msg, WireDialect::default());
        let content = v["content"].as_array().expect("content is an array");
        assert_eq!(content.len(), 2, "text block then image block");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "look");
        assert_eq!(content[1]["type"], "image_url");
        let url = content[1]["image_url"]["url"].as_str().unwrap();
        assert!(
            url.starts_with("data:image/png;base64,"),
            "data URI prefix, got {url}"
        );
        assert!(
            v.get("attachments").is_none(),
            "internal field must not leak"
        );
    }

    #[test]
    fn text_only_message_keeps_plain_string_content() {
        let v = message_to_wire(
            &ChatMessage::text("user", "plain turn"),
            WireDialect::default(),
        );
        assert!(v["content"].is_string(), "content stays a plain string");
        assert_eq!(v["content"], "plain turn");
        assert!(v.get("attachments").is_none());
    }

    #[test]
    fn image_only_message_omits_text_block() {
        let msg = ChatMessage::multimodal(
            "user",
            "",
            vec![image_attachment(
                "image/jpeg",
                AttachmentSource::Inline(inline_b64(&[0xff, 0xd8, 0xff])),
            )],
        );
        let content = message_to_wire(&msg, WireDialect::default())["content"]
            .as_array()
            .expect("content is an array")
            .clone();
        assert_eq!(content.len(), 1, "only the image block");
        assert_eq!(content[0]["type"], "image_url");
    }

    #[test]
    fn path_image_is_read_and_base64_encoded() {
        let dir = std::env::temp_dir();
        let file = dir.join(format!("ff-openai-test-{}.png", std::process::id()));
        let bytes = [0x89u8, 0x50, 0x4e, 0x47, 0x0d, 0x0a];
        std::fs::write(&file, bytes).unwrap();
        let msg = ChatMessage::multimodal(
            "user",
            "",
            vec![image_attachment(
                "image/png",
                AttachmentSource::Path(file.to_string_lossy().into_owned()),
            )],
        );
        let url = message_to_wire(&msg, WireDialect::default())["content"][0]["image_url"]["url"]
            .as_str()
            .unwrap()
            .to_string();
        std::fs::remove_file(&file).ok();
        assert_eq!(url, format!("data:image/png;base64,{}", inline_b64(&bytes)));
    }

    #[test]
    fn document_attachment_is_skipped() {
        let msg = ChatMessage::multimodal(
            "user",
            "summarize",
            vec![Attachment {
                kind: AttachmentKind::Document,
                media_type: "application/pdf".into(),
                source: AttachmentSource::Inline(inline_b64(b"%PDF-1.4")),
                name: Some("doc.pdf".into()),
                bytes: 8,
            }],
        );
        let v = message_to_wire(&msg, WireDialect::default());
        assert!(v["content"].is_string(), "no image block -> plain string");
        assert!(v.get("attachments").is_none());
    }

    #[test]
    fn unsupported_image_type_is_skipped() {
        let msg = ChatMessage::multimodal(
            "user",
            "look",
            vec![image_attachment(
                "image/svg+xml",
                AttachmentSource::Inline(inline_b64(b"<svg/>")),
            )],
        );
        let v = message_to_wire(&msg, WireDialect::default());
        assert!(v["content"].is_string(), "unsupported type skipped");
    }

    #[test]
    fn unreadable_path_is_skipped() {
        let msg = ChatMessage::multimodal(
            "user",
            "look",
            vec![image_attachment(
                "image/png",
                AttachmentSource::Path("/nonexistent/ff/missing.png".into()),
            )],
        );
        let v = message_to_wire(&msg, WireDialect::default());
        assert!(v["content"].is_string(), "unreadable file skipped");
    }

    // ---- #311 PR-3a: list_models / test_connection probe (HTTP-level) ----

    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn models_body(ids: &[&str]) -> serde_json::Value {
        serde_json::json!({ "data": ids.iter().map(|id| serde_json::json!({ "id": id })).collect::<Vec<_>>() })
    }

    #[tokio::test]
    async fn test_connection_ok_on_reachable_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(models_body(&["gpt-4o"])))
            .mount(&server)
            .await;
        let provider = OpenAiProvider::new(server.uri(), Some("sk-test".into()));
        assert!(provider.test_connection("gpt-4o").await.is_ok());
    }

    #[tokio::test]
    async fn test_connection_surfaces_401_on_bad_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let provider = OpenAiProvider::new(server.uri(), Some("sk-bad".into()));
        match provider.test_connection("gpt-4o").await {
            Err(LlmError::Api { status, .. }) => assert_eq!(status, 401),
            other => panic!("expected Api 401, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_connection_surfaces_5xx() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let provider = OpenAiProvider::new(server.uri(), Some("sk-test".into()));
        match provider.test_connection("gpt-4o").await {
            Err(LlmError::Api { status, .. }) => assert_eq!(status, 500),
            other => panic!("expected Api 500, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_connection_ok_on_empty_catalog() {
        // Reachable + authenticated, but the server lists no models. A bare 200 is
        // still a successful probe -- the credential and URL are valid.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(models_body(&[])))
            .mount(&server)
            .await;
        let provider = OpenAiProvider::new(server.uri(), Some("sk-test".into()));
        assert!(provider.test_connection("gpt-4o").await.is_ok());
    }

    #[tokio::test]
    async fn list_models_parses_ids() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(models_body(&["a", "b"])))
            .mount(&server)
            .await;
        let provider = OpenAiProvider::new(server.uri(), None);
        assert_eq!(provider.list_models().await.unwrap(), vec!["a", "b"]);
    }

    #[tokio::test]
    async fn list_models_sends_bearer_when_key_present() {
        // The mock only matches when the Authorization header carries the key, so a
        // pass proves the bearer is plumbed onto the request.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("authorization", "Bearer sk-plumbed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(models_body(&["gpt-4o"])))
            .mount(&server)
            .await;
        let provider = OpenAiProvider::new(server.uri(), Some("sk-plumbed".into()));
        assert!(provider.test_connection("gpt-4o").await.is_ok());
    }

    // ---- #375 PR-2: per-gateway wire dialect ----

    use crate::{FunctionCall, ReasoningWire, ToolCall, ToolCallContent, WireDialect};

    fn assistant_tool_call_with_reasoning(reasoning: Option<&str>) -> ChatMessage {
        ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "search".into(),
                    arguments: "{}".into(),
                },
            }]),
            tool_call_id: None,
            name: None,
            attachments: Vec::new(),
            reasoning: reasoning.map(str::to_string),
        }
    }

    #[test]
    fn dialect_default_omits_reasoning_and_content() {
        let msg = assistant_tool_call_with_reasoning(Some("hidden CoT"));
        let v = message_to_wire(&msg, WireDialect::default());
        // Vanilla OpenAI must never see the reasoning carrier nor its replay.
        assert!(v.get("reasoning").is_none());
        assert!(v.get("reasoning_content").is_none());
        // Default Omit: no "content" key (serde drops the None).
        assert!(v.get("content").is_none());
    }

    #[test]
    fn dialect_reasoning_content_replays_on_tool_call_turn() {
        let msg = assistant_tool_call_with_reasoning(Some("because A then B"));
        let dialect = WireDialect {
            reasoning: ReasoningWire::ReasoningContent,
            tool_call_content: ToolCallContent::Omit,
        };
        let v = message_to_wire(&msg, dialect);
        assert_eq!(v["reasoning_content"], "because A then B");
        assert!(v.get("reasoning").is_none());
    }

    #[test]
    fn dialect_reasoning_field_replays_for_openrouter() {
        let msg = assistant_tool_call_with_reasoning(Some("step"));
        let dialect = WireDialect {
            reasoning: ReasoningWire::Reasoning,
            tool_call_content: ToolCallContent::Omit,
        };
        let v = message_to_wire(&msg, dialect);
        assert_eq!(v["reasoning"], "step");
        assert!(v.get("reasoning_content").is_none());
    }

    #[test]
    fn dialect_does_not_replay_when_no_reasoning_present() {
        let msg = assistant_tool_call_with_reasoning(None);
        let dialect = WireDialect {
            reasoning: ReasoningWire::ReasoningContent,
            tool_call_content: ToolCallContent::Omit,
        };
        let v = message_to_wire(&msg, dialect);
        assert!(v.get("reasoning_content").is_none());
        assert!(v.get("reasoning").is_none());
    }

    #[test]
    fn dialect_replays_only_on_tool_call_turn_not_on_plain_assistant() {
        // A plain assistant turn (no tool_calls) keeps its content; reasoning
        // is private to the model and never replayed back to it on a normal turn.
        let mut msg = ChatMessage::text("assistant", "the answer is 42");
        msg.reasoning = Some("...".into());
        let dialect = WireDialect {
            reasoning: ReasoningWire::ReasoningContent,
            tool_call_content: ToolCallContent::Omit,
        };
        let v = message_to_wire(&msg, dialect);
        assert!(v.get("reasoning_content").is_none());
        assert_eq!(v["content"], "the answer is 42");
    }

    #[test]
    fn dialect_empty_string_content_for_glm_minimax() {
        let msg = assistant_tool_call_with_reasoning(None);
        let dialect = WireDialect {
            reasoning: ReasoningWire::ReasoningContent,
            tool_call_content: ToolCallContent::EmptyString,
        };
        let v = message_to_wire(&msg, dialect);
        // GLM/MiniMax reject content: null with HTTP 400 code 20015; empty string is accepted.
        assert_eq!(v["content"], "");
    }

    #[test]
    fn dialect_empty_string_only_when_content_actually_missing() {
        // If the tool-call turn has accompanying text (a "thinking out loud" preamble),
        // the EmptyString rule must NOT clobber it.
        let mut msg = assistant_tool_call_with_reasoning(None);
        msg.content = Some("let me search".into());
        let dialect = WireDialect {
            reasoning: ReasoningWire::ReasoningContent,
            tool_call_content: ToolCallContent::EmptyString,
        };
        let v = message_to_wire(&msg, dialect);
        assert_eq!(v["content"], "let me search");
    }
}
