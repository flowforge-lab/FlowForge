use async_trait::async_trait;
use base64::Engine as _;
use futures_util::StreamExt;
use serde::Deserialize;

use crate::{ChatMessage, ChatRequest, Chunk, ChunkStream, LlmError, Provider, ToolCallDelta};

/// Talks to a local Ollama server (`http://localhost:11434`) over its native
/// NDJSON `/api/chat` stream.
///
/// Ollama also exposes an OpenAI-compatible `/v1` endpoint that [`OpenAiProvider`]
/// already speaks, so why keep a native provider? Because only native `/api/chat`
/// honors the thinking on/off toggle (`think: false`, wired to the connection's
/// `thinking` setting). The `/v1` shim ignores `think`/`enable_thinking` and
/// streams reasoning unconditionally, so routing Ollama through it would silently
/// force always-on reasoning. The native path is the only one that can turn
/// thinking off -- that is its reason to exist, and why it is not folded into the
/// OpenAI provider.
///
/// [`OpenAiProvider`]: crate::OpenAiProvider
pub struct OllamaProvider {
    base_url: String,
    client: reqwest::Client,
    /// Whether the connection's model accepts attachments (#332/#334). When
    /// false, attachments are stripped before serialization so a raw `attachments`
    /// field never reaches the wire. Defaults false; set via
    /// [`OllamaProvider::with_vision`].
    supports_vision: bool,
    /// The context window to request from Ollama, in tokens (#538). When `Some`,
    /// it is sent as `options.num_ctx` on every `/api/chat` request -- so the
    /// runtime serves exactly this window regardless of the server's
    /// `OLLAMA_CONTEXT_LENGTH` default -- and is also the value
    /// [`context_window`](OllamaProvider::context_window) reports (clamped to the
    /// model's trained ceiling) so the agent's compaction budget matches what
    /// Ollama actually serves. `None` leaves the request unset and the reported
    /// window conservative; see [`context_window`](OllamaProvider::context_window).
    num_ctx: Option<u64>,
    /// The *probed* served window (#602/#612), primed from `/api/ps` before a
    /// turn via [`set_context_budget`](OllamaProvider::set_context_budget). Unlike
    /// [`num_ctx`] this never reaches the wire -- it only feeds
    /// [`context_window`](OllamaProvider::context_window) so the compaction budget
    /// tracks the window the runtime is actually serving when no explicit
    /// `num_ctx` was requested. `None` falls through to the conservative default.
    budget_window: Option<u64>,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: crate::build_streaming_http_client(),
            supports_vision: false,
            num_ctx: None,
            budget_window: None,
        }
    }

    /// Declare whether the target model can accept image/document attachments.
    pub fn with_vision(mut self, supports_vision: bool) -> Self {
        self.supports_vision = supports_vision;
        self
    }

    /// Set the context window to request from Ollama (`options.num_ctx`), in
    /// tokens (#538). This both sizes the served window and the agent's reported
    /// [`context_window`], keeping the compaction budget aligned with what the
    /// runtime serves rather than the model's trained maximum.
    pub fn with_num_ctx(mut self, num_ctx: Option<u64>) -> Self {
        self.num_ctx = num_ctx;
        self
    }

    /// Prime the probed served window used by
    /// [`context_window`](OllamaProvider::context_window) when no explicit
    /// `num_ctx` was requested (#612). Builder-style mirror of
    /// [`Provider::set_context_budget`](crate::Provider::set_context_budget).
    pub fn with_budget_window(mut self, budget_window: Option<u64>) -> Self {
        self.budget_window = budget_window;
        self
    }

    /// Probe the *served* context window for `model` (#602): the trained ceiling
    /// (`/api/show`, falling back to the family table), the live `/api/ps` value
    /// for the loaded instance, and the effective window + source via
    /// [`resolve_served_window`]. Best-effort — a failed probe degrades to the
    /// conservative default rather than erroring, since this only drives a display
    /// hint, never a turn. `/api/ps` reports only *loaded* models, so before the
    /// first turn the source is `Default` until the model is resident.
    pub async fn served_window(&self, model: &str) -> ServedWindowProbe {
        let trained = self
            .probe_trained_window(model)
            .await
            .or_else(|| Some(crate::model_context_window(model)));
        let ps_served = self.probe_loaded_window(model).await;
        let (window, source) = resolve_served_window(self.num_ctx, ps_served, trained);
        ServedWindowProbe {
            window: Some(window),
            trained,
            source: Some(source),
        }
    }

    /// `GET /api/ps` -> the loaded instance's served `context_length`, or `None`
    /// when the model is not resident or the build omits the field.
    async fn probe_loaded_window(&self, model: &str) -> Option<u64> {
        let resp = self
            .client
            .get(format!("{}/api/ps", self.base_url))
            .send()
            .await
            .ok()?;
        let resp = crate::error_for_status_with_body(resp).await.ok()?;
        let list: PsList = resp.json().await.ok()?;
        list.models
            .into_iter()
            .find(|m| m.name == model || m.model.as_deref() == Some(model))
            .and_then(|m| m.context_length)
    }

    /// `POST /api/show` -> the trained ceiling from `model_info.*.context_length`,
    /// or `None` when the server is unreachable or the field is absent.
    async fn probe_trained_window(&self, model: &str) -> Option<u64> {
        let resp = self
            .client
            .post(format!("{}/api/show", self.base_url))
            .json(&serde_json::json!({ "model": model }))
            .send()
            .await
            .ok()?;
        let resp = crate::error_for_status_with_body(resp).await.ok()?;
        let show: ShowResponse = resp.json().await.ok()?;
        show.model_info
            .iter()
            .find(|(k, _)| k.ends_with(".context_length"))
            .and_then(|(_, v)| v.as_u64())
    }
}

impl Default for OllamaProvider {
    fn default() -> Self {
        Self::new("http://localhost:11434")
    }
}

/// Read the optional Ollama context-window override from the environment
/// (`FLOWFORGE_OLLAMA_NUM_CTX`), in tokens (#538). Hosts thread this into
/// [`OllamaProvider::with_num_ctx`] so the served window and the agent's
/// compaction budget agree. Invalid or zero values are ignored (treated as unset).
pub fn ollama_num_ctx_from_env() -> Option<u64> {
    std::env::var("FLOWFORGE_OLLAMA_NUM_CTX")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
}

/// The served context window for a model plus how it was determined (#602).
/// Carrier between the async probe ([`OllamaProvider::served_window`]) and the
/// host, which folds it onto `ResolvedModel`. All `None` for a provider that does
/// not expose a served-window probe.
#[derive(Debug, Clone, Default)]
pub struct ServedWindowProbe {
    /// Effective served window in tokens, or `None` when unknown.
    pub window: Option<u64>,
    /// Trained ceiling (`/api/show`), or the family-table fallback.
    pub trained: Option<u64>,
    /// Which input produced `window`.
    pub source: Option<ff_core::ContextWindowSource>,
}

/// Resolve the effective served window and its source from the three inputs, in
/// precedence order (#602): an explicit `FLOWFORGE_OLLAMA_NUM_CTX` (clamped to the
/// trained ceiling, since Ollama serves `min(num_ctx, trained)`) wins; otherwise
/// the live `/api/ps` value; otherwise the conservative
/// [`DEFAULT_CONTEXT_WINDOW_TOKENS`]. Pure so it is unit-tested without a server,
/// and so #598 can size its budget denominator from the same single source.
pub fn resolve_served_window(
    env_num_ctx: Option<u64>,
    ps_served: Option<u64>,
    trained: Option<u64>,
) -> (u64, ff_core::ContextWindowSource) {
    use ff_core::ContextWindowSource;
    if let Some(n) = env_num_ctx {
        let clamped = trained.map_or(n, |t| n.min(t));
        return (clamped, ContextWindowSource::Explicit);
    }
    if let Some(n) = ps_served {
        return (n, ContextWindowSource::Served);
    }
    (
        crate::DEFAULT_CONTEXT_WINDOW_TOKENS,
        ContextWindowSource::Default,
    )
}

/// `GET /api/ps` -> currently *loaded* model instances. `context_length` is the
/// window the runtime is actually serving for that instance; it is version-
/// dependent (older builds omit it) so it stays optional.
#[derive(Deserialize)]
struct PsList {
    #[serde(default)]
    models: Vec<PsEntry>,
}

#[derive(Deserialize)]
struct PsEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    context_length: Option<u64>,
}

/// `POST /api/show` -> model metadata. The trained ceiling lives in `model_info`
/// under an architecture-prefixed key (e.g. `"llama.context_length"`), so we scan
/// for any `*.context_length` rather than hard-coding the architecture.
#[derive(Deserialize)]
struct ShowResponse {
    #[serde(default)]
    model_info: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct OllamaChunk {
    #[serde(default)]
    message: OllamaMessage,
    #[serde(default)]
    done: bool,
    /// "length" when generation stopped on the `num_predict` cap rather than a
    /// natural end of turn, mirroring OpenAI `finish_reason` (#528).
    #[serde(default)]
    done_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct OllamaMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    thinking: String,
    /// Native Ollama tool calls. Unlike the OpenAI stream these arrive complete
    /// (not split across chunks), and `arguments` is a JSON object, not a string.
    /// Whether a call carries an `id` is version-dependent (older builds omit it;
    /// 0.30.x supplies one), so we ignore any server id and synthesize our own —
    /// see [`parse_ollama_line`].
    #[serde(default)]
    tool_calls: Vec<OllamaToolCall>,
}

#[derive(Deserialize)]
struct OllamaToolCall {
    #[serde(default)]
    function: OllamaFunction,
}

#[derive(Deserialize, Default)]
struct OllamaFunction {
    #[serde(default)]
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

#[derive(Deserialize)]
struct TagList {
    #[serde(default)]
    models: Vec<TagEntry>,
}

#[derive(Deserialize)]
struct TagEntry {
    name: String,
}

/// Parse one NDJSON line into an optional chunk.
///
/// Ollama returns each tool call complete in a single message (no fragment
/// reassembly). We always synthesize a stable, unique id per call (`tool_idx`
/// runs across the whole stream) rather than trusting the server's: some Ollama
/// versions omit the `id` entirely, others (0.30.x) supply one, so synthesizing
/// keeps the agent's id scheme uniform regardless. The `arguments` object is
/// JSON-encoded into the string shape the agent and OpenAI history expect.
fn parse_ollama_line(line: &[u8], tool_idx: &mut u32) -> Option<Result<Chunk, LlmError>> {
    if line.is_empty() {
        return None;
    }
    match serde_json::from_slice::<OllamaChunk>(line) {
        Ok(c) => {
            let tool_calls = c
                .message
                .tool_calls
                .into_iter()
                .map(|tc| {
                    let index = *tool_idx;
                    *tool_idx += 1;
                    ToolCallDelta {
                        index,
                        id: Some(format!("call_{index}")),
                        name: Some(tc.function.name),
                        arguments: if tc.function.arguments.is_null() {
                            String::new()
                        } else {
                            tc.function.arguments.to_string()
                        },
                    }
                })
                .collect();
            Some(Ok(Chunk {
                delta: c.message.content,
                reasoning_delta: c.message.thinking,
                tool_calls,
                done: c.done,
                truncated: c.done_reason.as_deref() == Some("length"),
            }))
        }
        Err(e) => Some(Err(LlmError::Decode(e.to_string()))),
    }
}

/// Serialize chat history for Ollama's native `/api/chat`, converting each tool
/// call's `arguments` from the OpenAI wire shape (a JSON-encoded **string**) to
/// the JSON **object** Ollama requires.
///
/// `ff-agent` stores arguments OpenAI-style (a string), which is what the OpenAI
/// providers need. Ollama's `/api/chat`, by contrast, rejects a string with
/// `400 ("Value looks like object, but can't find closing '}' symbol")`, so a
/// multi-turn tool conversation failed on the turn that echoes the prior tool
/// call back. We adapt only at this boundary; the stored/OpenAI representation is
/// untouched. Arguments that are empty, unparseable, or parse to a non-object
/// JSON value (e.g. an array or scalar) become `{}` — Ollama requires an object.
/// Base64-encode one image attachment for Ollama's `images: [base64, ...]` field,
/// or `None` (with a warning) when it can't be sent: an unsupported media type, an
/// unreadable file, or undecodable inline data. Unlike OpenAI's data URI, Ollama
/// takes the **bare** base64 with no `data:` prefix. Skipping rather than failing
/// keeps one bad attachment from dropping the whole turn (trust boundary, #334/#337).
fn ollama_image_base64(a: &ff_core::Attachment) -> Option<String> {
    if crate::image_media_type(&a.media_type).is_none() {
        tracing::warn!(media_type = %a.media_type, "skipping image attachment: unsupported media type for Ollama");
        return None;
    }
    let bytes = crate::attachment_bytes(a)
        .map_err(|e| tracing::warn!(error = %e, "skipping image attachment"))
        .ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn ollama_messages(messages: &[ChatMessage]) -> Result<serde_json::Value, LlmError> {
    let mut value = serde_json::to_value(messages).map_err(|e| LlmError::Decode(e.to_string()))?;
    let Some(arr) = value.as_array_mut() else {
        return Ok(value);
    };
    for (src, msg) in messages.iter().zip(arr.iter_mut()) {
        let Some(obj) = msg.as_object_mut() else {
            continue;
        };
        // Drop the internal attachments field so it never reaches Ollama (mirrors the
        // OpenAI adapter); serde already skips an empty Vec, so a text-only turn has no
        // key here and stays byte-identical. Then emit image attachments as Ollama's
        // `images: [base64, ...]` sibling field, leaving `content` a plain string.
        obj.remove("attachments");
        let images: Vec<String> = src
            .attachments
            .iter()
            .filter_map(|a| match a.kind {
                ff_core::AttachmentKind::Image => ollama_image_base64(a),
                ff_core::AttachmentKind::Document => {
                    tracing::warn!(media_type = %a.media_type, "skipping document attachment: Ollama chat has no portable document block (#338)");
                    None
                }
            })
            .collect();
        if !images.is_empty() {
            obj.insert("images".into(), serde_json::json!(images));
        }

        let Some(calls) = obj.get_mut("tool_calls").and_then(|v| v.as_array_mut()) else {
            continue;
        };
        for call in calls {
            if let Some(args) = call
                .get_mut("function")
                .and_then(|f| f.get_mut("arguments"))
            {
                if let Some(raw) = args.as_str() {
                    // Ollama requires an object; coerce anything that doesn't
                    // parse to one (empty, malformed, or a non-object value) to `{}`.
                    *args = match serde_json::from_str::<serde_json::Value>(raw) {
                        Ok(v) if v.is_object() => v,
                        _ => serde_json::json!({}),
                    };
                }
            }
        }
    }
    Ok(value)
}

#[async_trait]
impl Provider for OllamaProvider {
    fn supports_vision(&self) -> bool {
        self.supports_vision
    }

    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let messages = crate::messages_for_wire(&req.messages, self.supports_vision, false);
        let mut body = serde_json::json!({
            "model": req.model,
            "messages": ollama_messages(&messages)?,
            "stream": true,
            "think": req.thinking,
        });
        // Advertise the agent's tools so the model can call them. Without this
        // the model never sees the tools and (correctly) claims it cannot act.
        if !req.tools.is_empty() {
            body["tools"] = serde_json::Value::Array(req.tools.clone());
        }
        // Pin the served context window (#538). Ollama serves `min(num_ctx,
        // trained_max)`; without this it falls back to the server's
        // `OLLAMA_CONTEXT_LENGTH` default, which the agent cannot see and which
        // may not match the budget it sized from `context_window`.
        if let Some(n) = self.num_ctx {
            body["options"] = serde_json::json!({ "num_ctx": n });
        }

        let resp = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        let resp = crate::error_for_status_with_body(resp).await?;

        // Ollama streams newline-delimited JSON objects. Reassemble lines across
        // byte-chunk boundaries, parsing each complete line into a Chunk. The
        // scan state carries the line buffer plus a running tool-call index so
        // synthesized ids stay unique across chunks.
        let stream = resp
            .bytes_stream()
            .scan((Vec::<u8>::new(), 0u32), |(buf, tool_idx), item| {
                let out = match item {
                    Ok(bytes) => {
                        buf.extend_from_slice(&bytes);
                        let mut chunks = Vec::new();
                        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                            let line: Vec<u8> = buf.drain(..=pos).collect();
                            let line = &line[..line.len().saturating_sub(1)];
                            if let Some(chunk) = parse_ollama_line(line, tool_idx) {
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

    /// `GET {base_url}/api/tags` -> `{ "models": [ { "name": ... } ] }`.
    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        let resp = self
            .client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        let resp = crate::error_for_status_with_body(resp).await?;
        let list: TagList = resp
            .json()
            .await
            .map_err(|e| LlmError::Decode(e.to_string()))?;
        Ok(list.models.into_iter().map(|m| m.name).collect())
    }

    /// The window Ollama will actually serve, not the model's trained maximum
    /// (#538/#602/#612). Precedence mirrors [`resolve_served_window`] exactly, so
    /// the budget this sizes equals the window the model chip displays:
    /// 1. an explicit `num_ctx` (clamped to the trained ceiling, since Ollama
    ///    serves `min(num_ctx, trained)`);
    /// 2. otherwise the probed served window from `/api/ps`
    ///    ([`budget_window`], primed via [`set_context_budget`]);
    /// 3. otherwise the conservative [`DEFAULT_CONTEXT_WINDOW_TOKENS`].
    ///
    /// Falling back to the default rather than the trained max keeps the budget
    /// under the real window: Ollama serves `min(OLLAMA_CONTEXT_LENGTH, trained)`
    /// from a server default, so reporting the trained max would over-size the
    /// budget and overflow. Before the first turn `/api/ps` lists no loaded model,
    /// so the budget falls to the default until the model is resident -- a safe
    /// under-fill. Under-filling never truncates context; over-filling does.
    ///
    /// [`DEFAULT_CONTEXT_WINDOW_TOKENS`]: crate::DEFAULT_CONTEXT_WINDOW_TOKENS
    /// [`set_context_budget`]: crate::Provider::set_context_budget
    fn context_window(&self, model: &str) -> u64 {
        self.num_ctx
            .map(|n| n.min(crate::model_context_window(model)))
            .or(self.budget_window)
            .unwrap_or(crate::DEFAULT_CONTEXT_WINDOW_TOKENS)
    }

    fn set_context_budget(&mut self, window: Option<u64>) {
        self.budget_window = window;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_plain_content_chunk() {
        let mut idx = 0;
        let line = br#"{"message":{"content":"hello"},"done":false}"#;
        let chunk = parse_ollama_line(line, &mut idx).unwrap().unwrap();
        assert_eq!(chunk.delta, "hello");
        assert!(chunk.tool_calls.is_empty());
        assert!(!chunk.done);
        assert_eq!(idx, 0);
    }

    #[test]
    fn parses_a_reasoning_chunk() {
        let mut idx = 0;
        let line = br#"{"message":{"thinking":"hmm"},"done":false}"#;
        let chunk = parse_ollama_line(line, &mut idx).unwrap().unwrap();
        assert_eq!(chunk.reasoning_delta, "hmm");
    }

    #[test]
    fn parses_a_tool_call_with_synthesized_id_and_json_args() {
        let mut idx = 0;
        let line = br#"{"message":{"content":"","tool_calls":[
            {"function":{"name":"create_file","arguments":{"path":"a.txt","body":"hi"}}}
        ]},"done":false}"#;
        let chunk = parse_ollama_line(line, &mut idx).unwrap().unwrap();
        assert_eq!(chunk.tool_calls.len(), 1);
        let call = &chunk.tool_calls[0];
        assert_eq!(call.index, 0);
        assert_eq!(call.id.as_deref(), Some("call_0"));
        assert_eq!(call.name.as_deref(), Some("create_file"));
        // arguments round-trip back into a JSON object the agent can parse.
        let v: serde_json::Value = serde_json::from_str(&call.arguments).unwrap();
        assert_eq!(v["path"], "a.txt");
        assert_eq!(v["body"], "hi");
        assert_eq!(idx, 1, "tool index advances for the next call");
    }

    #[test]
    fn tool_call_index_is_unique_across_calls_and_chunks() {
        let mut idx = 0;
        let first = br#"{"message":{"tool_calls":[{"function":{"name":"a","arguments":{}}}]},"done":false}"#;
        let second =
            br#"{"message":{"tool_calls":[{"function":{"name":"b","arguments":{}}}]},"done":true}"#;
        let c1 = parse_ollama_line(first, &mut idx).unwrap().unwrap();
        let c2 = parse_ollama_line(second, &mut idx).unwrap().unwrap();
        assert_eq!(c1.tool_calls[0].index, 0);
        assert_eq!(c1.tool_calls[0].id.as_deref(), Some("call_0"));
        assert_eq!(c2.tool_calls[0].index, 1);
        assert_eq!(c2.tool_calls[0].id.as_deref(), Some("call_1"));
        assert!(c2.done);
    }

    #[test]
    fn empty_line_yields_no_chunk() {
        let mut idx = 0;
        assert!(parse_ollama_line(b"", &mut idx).is_none());
    }

    #[test]
    fn malformed_json_is_a_decode_error() {
        let mut idx = 0;
        let res = parse_ollama_line(b"{not json", &mut idx).unwrap();
        assert!(matches!(res, Err(LlmError::Decode(_))));
    }

    use crate::{FunctionCall, ToolCall};

    fn assistant_call(arguments: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_0".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "write_file".into(),
                    arguments: arguments.into(),
                },
            }]),
            tool_call_id: None,
            name: None,

            attachments: Vec::new(),
            reasoning: None,
        }
    }

    fn args_value(messages: &serde_json::Value) -> &serde_json::Value {
        &messages[0]["tool_calls"][0]["function"]["arguments"]
    }

    #[test]
    fn outbound_string_arguments_become_an_object() {
        // The exact shape ff-agent echoes back; Ollama 400s on a string.
        let msgs = vec![assistant_call(
            r#"{"path":"~/hello.rs","body":"fn main(){}"}"#,
        )];
        let out = ollama_messages(&msgs).unwrap();
        let args = args_value(&out);
        assert!(
            args.is_object(),
            "arguments must be a JSON object, got {args}"
        );
        assert_eq!(args["path"], "~/hello.rs");
        assert_eq!(args["body"], "fn main(){}");
    }

    #[test]
    fn outbound_empty_or_invalid_arguments_become_empty_object() {
        // Empty/whitespace/malformed, plus valid-but-non-object JSON (array,
        // scalar, string) — Ollama requires an object, so all map to `{}`.
        for raw in ["", "not json", "   ", "[1,2]", "42", r#""hi""#, "null"] {
            let out = ollama_messages(&[assistant_call(raw)]).unwrap();
            let args = args_value(&out);
            assert!(args.is_object(), "{raw:?} should map to an object");
            assert_eq!(
                args.as_object().unwrap().len(),
                0,
                "{raw:?} -> empty object"
            );
        }
    }

    #[test]
    fn outbound_messages_without_tool_calls_are_unchanged() {
        let msgs = vec![ChatMessage::text("user", "hi")];
        let out = ollama_messages(&msgs).unwrap();
        assert_eq!(out, serde_json::to_value(&msgs).unwrap());
        assert!(out[0].get("tool_calls").is_none());
    }

    #[test]
    fn outbound_converts_every_call_in_a_multi_call_message() {
        let msg = ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![
                ToolCall {
                    id: "call_0".into(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: "a".into(),
                        arguments: r#"{"x":1}"#.into(),
                    },
                },
                ToolCall {
                    id: "call_1".into(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: "b".into(),
                        arguments: r#"{"y":2}"#.into(),
                    },
                },
            ]),
            tool_call_id: None,
            name: None,

            attachments: Vec::new(),
            reasoning: None,
        };
        let out = ollama_messages(&[msg]).unwrap();
        assert_eq!(out[0]["tool_calls"][0]["function"]["arguments"]["x"], 1);
        assert_eq!(out[0]["tool_calls"][1]["function"]["arguments"]["y"], 2);
    }

    use ff_core::{Attachment, AttachmentKind, AttachmentSource};

    fn inline_b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn image(media_type: &str, source: AttachmentSource) -> Attachment {
        Attachment {
            kind: AttachmentKind::Image,
            media_type: media_type.into(),
            source,
            name: Some("shot.png".into()),
            bytes: 4,
        }
    }

    const PNG: [u8; 4] = [0x89, 0x50, 0x4e, 0x47];

    #[test]
    fn image_attachment_emits_bare_base64_in_images_array() {
        let b64 = inline_b64(&PNG);
        let msgs = vec![ChatMessage::multimodal(
            "user",
            "look at this",
            vec![image("image/png", AttachmentSource::Inline(b64.clone()))],
        )];
        let out = ollama_messages(&msgs).unwrap();
        let m = &out[0];
        assert!(
            m.get("attachments").is_none(),
            "internal field must not leak"
        );
        assert!(m["content"].is_string(), "content stays a plain string");
        let images = m["images"].as_array().expect("images array present");
        assert_eq!(images.len(), 1);
        let entry = images[0].as_str().unwrap();
        assert_eq!(entry, b64, "bare base64, byte-identical to the payload");
        assert!(
            !entry.starts_with("data:"),
            "Ollama takes bare base64, not a data URI"
        );
    }

    #[test]
    fn document_attachment_is_skipped_no_images() {
        let msgs = vec![ChatMessage::multimodal(
            "user",
            "read this",
            vec![Attachment {
                kind: AttachmentKind::Document,
                media_type: "application/pdf".into(),
                source: AttachmentSource::Inline(inline_b64(b"%PDF-1.4")),
                name: Some("doc.pdf".into()),
                bytes: 8,
            }],
        )];
        let out = ollama_messages(&msgs).unwrap();
        assert!(
            out[0].get("images").is_none(),
            "no image block for a document"
        );
        assert!(out[0].get("attachments").is_none());
    }

    #[test]
    fn unsupported_image_type_is_skipped() {
        let msgs = vec![ChatMessage::multimodal(
            "user",
            "svg here",
            vec![image(
                "image/svg+xml",
                AttachmentSource::Inline(inline_b64(b"<svg/>")),
            )],
        )];
        let out = ollama_messages(&msgs).unwrap();
        assert!(
            out[0].get("images").is_none(),
            "unsupported type produces no images"
        );
    }

    #[test]
    fn text_only_turn_is_byte_identical() {
        let msgs = vec![ChatMessage::text("user", "hi")];
        let out = ollama_messages(&msgs).unwrap();
        assert_eq!(out, serde_json::to_value(&msgs).unwrap());
        assert!(out[0].get("images").is_none());
        assert!(out[0].get("attachments").is_none());
    }

    /// Folded from the #368 review: one bad attachment must not drop the turn -- the
    /// valid image still lands in `images`, the unreadable one is skipped.
    #[test]
    fn mixed_valid_and_unreadable_keeps_the_good_image() {
        let msgs = vec![ChatMessage::multimodal(
            "user",
            "two images",
            vec![
                image("image/png", AttachmentSource::Inline(inline_b64(&PNG))),
                image(
                    "image/png",
                    AttachmentSource::Path("/nonexistent/flowforge/missing.png".into()),
                ),
            ],
        )];
        let out = ollama_messages(&msgs).unwrap();
        let images = out[0]["images"].as_array().expect("images present");
        assert_eq!(images.len(), 1, "only the readable image survives");
        assert_eq!(images[0].as_str().unwrap(), inline_b64(&PNG));
    }

    /// Folded from the #368 review: exercise the strip (messages_for_wire) and reshape
    /// (ollama_messages) layers together. Vision off => no images and no leaked field;
    /// vision on => images present.
    #[test]
    fn strip_then_reshape_composition() {
        let msgs = vec![ChatMessage::multimodal(
            "user",
            "look",
            vec![image(
                "image/png",
                AttachmentSource::Inline(inline_b64(&PNG)),
            )],
        )];

        let off = ollama_messages(&crate::messages_for_wire(&msgs, false, false)).unwrap();
        assert!(
            off[0].get("images").is_none(),
            "vision off: image stripped before reshape"
        );
        assert!(
            off[0].get("attachments").is_none(),
            "vision off: no leaked field"
        );

        let on = ollama_messages(&crate::messages_for_wire(&msgs, true, false)).unwrap();
        assert!(
            on[0]["images"].as_array().is_some_and(|a| a.len() == 1),
            "vision on: image emitted"
        );
        assert!(on[0].get("attachments").is_none());
    }

    fn req(model: &str) -> ChatRequest {
        ChatRequest {
            model: model.to_string(),
            messages: vec![],
            tools: vec![],
            thinking: false,
            max_tokens: None,
        }
    }

    #[test]
    fn context_window_reports_configured_num_ctx() {
        // A configured num_ctx is the served window the agent budgets against,
        // not the model's trained maximum (#538).
        let p = OllamaProvider::default().with_num_ctx(Some(131_072));
        assert_eq!(p.context_window("Qwen/Qwen3.6-35B-A3B"), 131_072);
    }

    #[test]
    fn context_window_clamps_num_ctx_to_trained_ceiling() {
        // num_ctx above the model's trained window cannot be served, so the
        // reported window is clamped to the family-lookup ceiling.
        let p = OllamaProvider::default().with_num_ctx(Some(9_999_999));
        assert_eq!(
            p.context_window("Qwen/Qwen3.6-35B-A3B"),
            crate::model_context_window("Qwen/Qwen3.6-35B-A3B"),
        );
    }

    #[test]
    fn context_window_unset_is_conservative() {
        // With no num_ctx the served window is the server's OLLAMA_CONTEXT_LENGTH
        // default, which the agent cannot see; report the conservative default so
        // the budget under-fills rather than overflowing (#538).
        let p = OllamaProvider::default();
        assert_eq!(
            p.context_window("Qwen/Qwen3.6-35B-A3B"),
            crate::DEFAULT_CONTEXT_WINDOW_TOKENS,
        );
    }

    #[test]
    fn context_window_uses_probed_budget_when_no_num_ctx() {
        // With no explicit num_ctx, the probed served window (#612) becomes the
        // budget denominator -- so a user who raised OLLAMA_CONTEXT_LENGTH budgets
        // against the window the runtime actually serves, not the 32k default.
        let p = OllamaProvider::default().with_budget_window(Some(131_072));
        assert_eq!(p.context_window("Qwen/Qwen3.6-35B-A3B"), 131_072);
    }

    #[test]
    fn context_window_explicit_num_ctx_overrides_probed_budget() {
        // An explicit num_ctx is the served window the request pins, so it wins
        // over a probed budget -- and is still clamped to the trained ceiling.
        let trained = crate::model_context_window("Qwen/Qwen3.6-35B-A3B");
        let p = OllamaProvider::default()
            .with_num_ctx(Some(8_192))
            .with_budget_window(Some(131_072));
        assert_eq!(p.context_window("Qwen/Qwen3.6-35B-A3B"), 8_192);
        let clamped = OllamaProvider::default()
            .with_num_ctx(Some(9_999_999))
            .with_budget_window(Some(131_072));
        assert_eq!(clamped.context_window("Qwen/Qwen3.6-35B-A3B"), trained);
    }

    #[test]
    fn context_window_falls_to_default_when_neither_set() {
        // No num_ctx and an unprimed/empty probe (cold start, before /api/ps lists
        // the model) falls to the conservative default -- a safe under-fill.
        let mut p = OllamaProvider::default();
        Provider::set_context_budget(&mut p, None);
        assert_eq!(
            p.context_window("Qwen/Qwen3.6-35B-A3B"),
            crate::DEFAULT_CONTEXT_WINDOW_TOKENS,
        );
    }

    #[test]
    fn set_context_budget_threads_probed_window_into_budget() {
        // The Provider-trait setter the host calls before a turn primes the same
        // budget the builder does, so the wiring in lib.rs reaches context_window.
        let mut p = OllamaProvider::default();
        Provider::set_context_budget(&mut p, Some(262_144));
        assert_eq!(p.context_window("moonshotai/Kimi-K2.7-Code"), 262_144);
    }

    #[test]
    fn resolve_served_window_prefers_explicit_clamped_to_trained() {
        use ff_core::ContextWindowSource;
        // Explicit env wins and is reported as-is when within the trained ceiling.
        assert_eq!(
            resolve_served_window(Some(131_072), Some(8_192), Some(262_144)),
            (131_072, ContextWindowSource::Explicit),
        );
        // Explicit above the trained ceiling clamps (Ollama serves min()).
        assert_eq!(
            resolve_served_window(Some(9_999_999), None, Some(262_144)),
            (262_144, ContextWindowSource::Explicit),
        );
    }

    #[test]
    fn resolve_served_window_uses_ps_value_without_explicit() {
        use ff_core::ContextWindowSource;
        assert_eq!(
            resolve_served_window(None, Some(40_960), Some(262_144)),
            (40_960, ContextWindowSource::Served),
        );
    }

    #[test]
    fn resolve_served_window_falls_back_to_conservative_default() {
        use ff_core::ContextWindowSource;
        assert_eq!(
            resolve_served_window(None, None, Some(262_144)),
            (
                crate::DEFAULT_CONTEXT_WINDOW_TOKENS,
                ContextWindowSource::Default
            ),
        );
    }

    #[test]
    fn ps_list_parses_context_length_present_and_absent() {
        let with: PsList =
            serde_json::from_str(r#"{"models":[{"name":"qwen3.6:35b","context_length":131072}]}"#)
                .unwrap();
        assert_eq!(with.models[0].context_length, Some(131_072));

        // Older Ollama builds omit context_length; the field must stay optional.
        let without: PsList =
            serde_json::from_str(r#"{"models":[{"name":"qwen3.6:35b"}]}"#).unwrap();
        assert_eq!(without.models[0].context_length, None);
    }

    #[tokio::test]
    async fn chat_stream_sends_num_ctx_when_configured() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("{\"message\":{\"content\":\"\"},\"done\":true}\n"),
            )
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(server.uri()).with_num_ctx(Some(131_072));
        let mut stream = provider.chat_stream(req("qwen3.6:35b-a3b")).await.unwrap();
        while stream.next().await.is_some() {}

        let reqs = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body["options"]["num_ctx"], 131_072);
    }

    #[tokio::test]
    async fn chat_stream_omits_num_ctx_when_unset() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("{\"message\":{\"content\":\"\"},\"done\":true}\n"),
            )
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(server.uri());
        let mut stream = provider.chat_stream(req("qwen3.6:35b-a3b")).await.unwrap();
        while stream.next().await.is_some() {}

        let reqs = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert!(
            body.get("options").is_none(),
            "no num_ctx => no options key"
        );
    }
}
