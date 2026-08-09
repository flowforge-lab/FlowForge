use async_trait::async_trait;
use base64::Engine as _;
use futures_util::StreamExt;
use serde::Deserialize;

use crate::{ChatMessage, ChatRequest, Chunk, ChunkStream, LlmError, Provider, ToolCallDelta};
use ff_core::ProviderKind;

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
    /// Whether the connection's model accepts *document* attachments via the
    /// text-extraction fallback (#338 follow-up). Ollama's native `/api/chat`
    /// has no document block, so when this is true the adapter extracts each
    /// document's text and folds it into the user message's `content` before
    /// serialization. Defaults false (the #338 skip); the host opts in via
    /// [`OllamaProvider::with_documents`] from the resolved model capability.
    supports_documents: bool,
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
    /// How long Ollama keeps the model resident after a request, sent as the
    /// top-level `keep_alive` field on `/api/chat`. Ollama's own default is 5
    /// minutes, after which the model unloads and the next turn pays a full
    /// reload from disk -- measured at ~68s for the 24 GB qwen3.6 MoE, turning a
    /// 3s reply into a 70s one just for having paused to read. An interactive
    /// desktop chat routinely idles past 5 minutes between turns, so the default
    /// is widened to keep the model warm across normal think/read pauses.
    /// `None` omits the field (Ollama's 5-minute default); see
    /// [`default_keep_alive`].
    keep_alive: Option<String>,
    /// Which kind of backend this provider talks to (#888). Always
    /// [`ProviderKind::Ollama`] for the native adapter; exposed via the
    /// `with_kind` builder so the host doesn't have to special-case it. The
    /// agent loop reads this to gate the `egress=local-only`-but-cloud-model
    /// warning -- Ollama is local, so the warning stays silent.
    kind: ProviderKind,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: crate::build_ollama_http_client(),
            supports_vision: false,
            supports_documents: false,
            num_ctx: None,
            budget_window: None,
            keep_alive: default_keep_alive(),
            kind: ProviderKind::Ollama,
        }
    }

    /// Override the backend kind (#888). The native Ollama adapter always
    /// talks to Ollama, but the builder stays symmetric with the other
    /// providers' `with_kind` so the host's `build_provider` flow needs no
    /// special branch for local kinds.
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
    /// the text-extraction fallback (#338 follow-up). When true, documents are
    /// extracted to text and folded into the user message's prompt context;
    /// when false (the default), they are stripped at the capability strip
    /// (the #338 skip). Mirrors [`crate::BedrockProvider::with_documents`].
    pub fn with_documents(mut self, supports_documents: bool) -> Self {
        self.supports_documents = supports_documents;
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

    /// Override how long Ollama keeps the model resident (`keep_alive`). `None`
    /// omits the field, falling back to Ollama's 5-minute default. See
    /// [`default_keep_alive`] for the value [`new`](OllamaProvider::new) applies.
    pub fn with_keep_alive(mut self, keep_alive: Option<String>) -> Self {
        self.keep_alive = keep_alive;
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
        let show = self.probe_show(model).await;
        let trained = show
            .as_ref()
            .and_then(ShowResponse::trained_window)
            .or_else(|| Some(crate::model_context_window(model)));
        // `None` when the probe failed (server down); the host then leaves the
        // name-based gate untouched. `Some(false)` is a definitive "no vision".
        let supports_vision = show.as_ref().map(ShowResponse::supports_vision);
        let ps_served = self.probe_loaded_window(model).await;
        let (window, source) = resolve_served_window(self.num_ctx, ps_served, trained);
        ServedWindowProbe {
            window: Some(window),
            trained,
            source: Some(source),
            supports_vision,
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

    /// `POST /api/show` -> the model's metadata (trained window + capability
    /// tags), or `None` when the server is unreachable or returns an error.
    /// One round-trip backs both the served-window probe and the vision-capability
    /// lookup (#625).
    async fn probe_show(&self, model: &str) -> Option<ShowResponse> {
        let resp = self
            .client
            .post(format!("{}/api/show", self.base_url))
            .json(&serde_json::json!({ "model": model }))
            .send()
            .await
            .ok()?;
        let resp = crate::error_for_status_with_body(resp).await.ok()?;
        resp.json().await.ok()
    }

    /// Whether the Ollama daemon reports this model as vision-capable (#625).
    /// `false` when the server is unreachable or the model has no vision
    /// capability, so the caller ORs it with the name-based gate -- which is the
    /// offline / probe-failure floor, never overridden downward by this probe.
    pub async fn probe_supports_vision(&self, model: &str) -> bool {
        self.probe_show(model)
            .await
            .map(|s| s.supports_vision())
            .unwrap_or(false)
    }
}

/// Whether any message in the turn carries an image attachment (#625). Used to
/// skip the `/api/show` vision probe on text-only turns.
fn messages_have_image(messages: &[ChatMessage]) -> bool {
    messages.iter().any(|m| {
        m.attachments
            .iter()
            .any(|a| a.kind == ff_core::AttachmentKind::Image)
    })
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

/// The `keep_alive` value [`OllamaProvider::new`] applies, so the model stays
/// warm across the read/think pauses of an interactive chat instead of unloading
/// on Ollama's 5-minute default and reloading (~68s for a 24 GB MoE) on the next
/// turn. Overridable via `FLOWFORGE_OLLAMA_KEEP_ALIVE`, which takes any value
/// Ollama accepts: a duration (`"30m"`, `"1h"`), a number of seconds (`"600"`),
/// or `"-1"` to keep the model resident until it is explicitly stopped. An empty
/// value opts back out to Ollama's own default (the field is omitted). Defaults
/// to `"30m"`.
fn default_keep_alive() -> Option<String> {
    match std::env::var("FLOWFORGE_OLLAMA_KEEP_ALIVE") {
        Ok(v) if v.trim().is_empty() => None,
        Ok(v) => Some(v.trim().to_string()),
        Err(_) => Some("30m".to_string()),
    }
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
    /// Whether the Ollama daemon reports this model as vision-capable (#625),
    /// or `None` when the probe could not run. The host ORs `Some(true)` onto the
    /// name-based vision gate; `None`/`Some(false)` leave the gate as-is.
    pub supports_vision: Option<bool>,
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
    /// Capability tags the daemon reports for this model (e.g. `"vision"`,
    /// `"tools"`, `"thinking"`) (#625). Absent on older Ollama builds, hence the
    /// `default`.
    #[serde(default)]
    capabilities: Vec<String>,
}

impl ShowResponse {
    /// Trained context ceiling: the architecture-prefixed `*.context_length`
    /// entry in `model_info`, or `None` when the field is absent.
    fn trained_window(&self) -> Option<u64> {
        self.model_info
            .iter()
            .find(|(k, _)| k.ends_with(".context_length"))
            .and_then(|(_, v)| v.as_u64())
    }

    /// Whether the daemon advertises image input for this model (#625).
    fn supports_vision(&self) -> bool {
        self.capabilities.iter().any(|c| c == "vision")
    }
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
                ..Chunk::default()
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
        crate::promote_mode_switch_marker(src, obj);
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
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        // Strip images without vision and documents without document support;
        // surviving documents are folded into the user message's `content` as
        // extracted text (#338 follow-up), since Ollama's wire has no document
        // block. The fold clones only when a message carries documents.
        // Vision is a floor-OR-probe decision (#625): the name-based hint set via
        // `with_vision` is the floor; when it says no *and* the turn actually
        // carries an image, ask the daemon directly so a genuinely multimodal
        // model (e.g. a qwen3-vl MoE) is not stripped by a stale name allow-list.
        // The probe is skipped on text-only turns and when vision is already
        // granted, so it adds no latency to the common path.
        let supports_vision = if self.supports_vision {
            true
        } else if messages_have_image(&req.messages) {
            self.probe_supports_vision(&req.model).await
        } else {
            false
        };
        let stripped =
            crate::messages_for_wire(&req.messages, supports_vision, self.supports_documents);
        let messages: Vec<ChatMessage> = if self.supports_documents {
            crate::extract::fold_documents_into_text(&stripped)
        } else {
            stripped.into_owned()
        };
        let messages = crate::enforce_user_terminated(messages);
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
        // Keep the model resident across the pauses of an interactive chat so a
        // turn after a short idle doesn't pay a full model reload (see
        // [`default_keep_alive`]). Omitted when `None` (Ollama's own default).
        if let Some(keep_alive) = &self.keep_alive {
            body["keep_alive"] = serde_json::Value::String(keep_alive.clone());
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

    fn supports_vision(&self) -> bool {
        self.supports_vision
    }

    fn supports_documents(&self) -> bool {
        self.supports_documents
    }

    fn kind(&self) -> ProviderKind {
        self.kind
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

    /// Ollama warmth is residency, not GPU clock: `keep_alive` (#634) holds the
    /// model in memory, and a warmup ping mainly (re)loads it and resets that TTL.
    /// One drained chunk confirms the load started, so the candle-vLLM 32-step ramp
    /// would just burn tokens here (#61).
    fn warmup_ramp_steps(&self) -> u8 {
        1
    }
}

#[cfg(test)]
mod tests;
