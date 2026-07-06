use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    ChatMessage, ChatRequest, Chunk, ChunkStream, LlmError, Provider, ReasoningEffort,
    ToolCallDelta,
};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_MAX_TOKENS: u32 = 4096;
/// Tokens reserved for the visible answer above the thinking budget. Anthropic's
/// `max_tokens` caps thinking + answer combined and must exceed `budget_tokens`,
/// so on a thinking turn we bump it to `budget + this` when it would be too low.
const ANTHROPIC_ANSWER_HEADROOM: u32 = 4096;

/// Native Anthropic Messages API provider (`POST /v1/messages`, server-sent events).
///
/// Distinct from Claude-via-Bedrock: that path uses SigV4/bearer auth and the
/// Converse wire shape; this one uses `x-api-key` + `anthropic-version` headers and
/// Anthropic's own Messages block model. The transport mirrors [`crate::OpenAiProvider`]
/// (reqwest + manual SSE line reassembly); the history<->block translation mirrors the
/// Bedrock Converse mapping but stays self-contained -- no shared code path across
/// transports.
pub struct AnthropicProvider {
    base_url: String,
    api_key: String,
    max_tokens: u32,
    /// Reasoning depth dial (#394). Drives `thinking.budget_tokens` when a turn
    /// requests thinking; defaults to [`ReasoningEffort::Medium`].
    reasoning_effort: ReasoningEffort,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Hosted Anthropic API (requires an `sk-ant-...` key).
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: api_key.into(),
            max_tokens: DEFAULT_MAX_TOKENS,
            reasoning_effort: ReasoningEffort::default(),
            client: crate::build_streaming_http_client(),
        }
    }

    /// Override the base URL (for a proxy or a test server).
    pub fn with_base_url(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let mut p = Self::new(api_key);
        p.base_url = base_url.into();
        p
    }

    /// Override the `max_tokens` cap sent on every turn. Anthropic requires this
    /// field; the default is [`DEFAULT_MAX_TOKENS`].
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Set the reasoning depth dial (#394). Applies only on thinking turns,
    /// where it caps `thinking.budget_tokens` to the effort budget (clamped below
    /// `max_tokens`). Defaults to [`ReasoningEffort::Medium`].
    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = effort;
        self
    }

    /// Emitted Anthropic request body — the exact JSON `chat_stream` POSTs to
    /// `/v1/messages`.  Factored out (#395 acceptance) so the provider's private
    /// `reasoning_effort` dial is assertable without a live Anthropic call.
    fn emitted_body_for(&self, req: &ChatRequest) -> Value {
        to_anthropic_request(req, self.max_tokens, self.reasoning_effort)
    }
}

impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("max_tokens", &self.max_tokens)
            .finish()
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StreamEvent {
    ContentBlockStart {
        #[serde(default)]
        index: u32,
        content_block: ContentBlockStart,
    },
    ContentBlockDelta {
        #[serde(default)]
        index: u32,
        delta: BlockDelta,
    },
    MessageStop,
    Error {
        error: ApiErrorBody,
    },
    /// `message_start`, `content_block_stop`, `message_delta`, `ping`, and any future
    /// event type carry no streamable payload.
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlockStart {
    ToolUse {
        id: String,
        name: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BlockDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    /// `signature_delta` and any future delta type.
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct ApiErrorBody {
    message: String,
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

/// Map one decoded SSE event to a [`Chunk`], or `None` for events that carry no
/// streamable payload. Mirrors the Bedrock `event_to_chunk` translation.
fn event_to_chunk(event: StreamEvent) -> Option<Result<Chunk, LlmError>> {
    match event {
        StreamEvent::ContentBlockStart {
            index,
            content_block,
        } => match content_block {
            // A tool-use block announces its id + name before any argument bytes.
            ContentBlockStart::ToolUse { id, name } => Some(Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index,
                    id: Some(id),
                    name: Some(name),
                    arguments: String::new(),
                }],
                ..Chunk::default()
            })),
            ContentBlockStart::Other => None,
        },
        StreamEvent::ContentBlockDelta { index, delta } => match delta {
            BlockDelta::TextDelta { text } => Some(Ok(Chunk {
                delta: text,
                ..Chunk::default()
            })),
            // Partial JSON fragments accumulate by index; the agent layer parses the
            // joined string once the call completes.
            BlockDelta::InputJsonDelta { partial_json } => Some(Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index,
                    arguments: partial_json,
                    ..ToolCallDelta::default()
                }],
                ..Chunk::default()
            })),
            BlockDelta::ThinkingDelta { thinking } => Some(Ok(Chunk {
                reasoning_delta: thinking,
                ..Chunk::default()
            })),
            BlockDelta::Other => None,
        },
        StreamEvent::MessageStop => Some(Ok(Chunk {
            done: true,
            ..Chunk::default()
        })),
        // In-stream errors (e.g. an `overloaded_error` during high load) arrive as an
        // SSE event rather than an HTTP status; surface them as an Api error.
        StreamEvent::Error { error } => Some(Err(LlmError::Api {
            status: 0,
            message: error.message,
        })),
        StreamEvent::Other => None,
    }
}

/// Parse one raw SSE line into an optional chunk.
///
/// Anthropic frames each event as an `event:` name line plus a `data:` JSON line.
/// Every data payload also carries a `type` field, so we dispatch on the JSON `type`
/// and ignore the `event:` line (no cross-line event-name state). Blank lines,
/// comments, and non-`data:` fields carry no payload.
fn parse_anthropic_line(line: &[u8]) -> Option<Result<Chunk, LlmError>> {
    let line = std::str::from_utf8(line).ok()?.trim();
    let payload = line.strip_prefix("data:")?.trim();
    if payload.is_empty() {
        return None;
    }
    match serde_json::from_str::<StreamEvent>(payload) {
        Ok(event) => event_to_chunk(event),
        Err(e) => Some(Err(LlmError::Decode(e.to_string()))),
    }
}

/// Build the `/v1/messages` request body from OpenAI-shaped history.
fn to_anthropic_request(req: &ChatRequest, max_tokens: u32, effort: ReasoningEffort) -> Value {
    let (system, mut messages) = to_anthropic_messages(&req.messages);
    // Message-level cache breakpoints (#763): mark the penultimate wire message
    // and (when history is long enough) index 0 so the growing conversation prefix
    // is cached across turns. Anthropic allows up to 4 breakpoints total; system
    // and tools already use 2, so we add at most 2 more on messages.
    if req.cache_messages && messages.len() >= 2 {
        let len = messages.len();
        mark_anthropic_cache_breakpoint(&mut messages, len - 2);
        if len >= 4 {
            mark_anthropic_cache_breakpoint(&mut messages, 0);
        }
    }
    let mut body = json!({
        "model": req.model,
        "max_tokens": max_tokens,
        "stream": true,
        "messages": messages,
    });
    if let Some(system) = system {
        // Prompt caching (#437): mark the stable system prefix as a cache
        // breakpoint. Anthropic caches `tools` then `system` up to this point, so
        // prefill is near-free from turn 2. Below the minimum cacheable size the
        // breakpoint is silently ignored, so this is safe for every Claude model.
        body["system"] = json!([{
            "type": "text",
            "text": system,
            "cache_control": { "type": "ephemeral" },
        }]);
    }
    if let Some(mut tools) = to_anthropic_tools(&req.tools) {
        // Second breakpoint on the last tool: the tool-schema block is the largest
        // stable prefix segment and rarely changes within a session.
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = json!({ "type": "ephemeral" });
        }
        body["tools"] = Value::Array(tools);
    }
    if req.thinking {
        let budget = effort.budget_tokens();
        body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
        // Anthropic requires budget_tokens < max_tokens (thinking + answer share
        // the cap), so raise max_tokens to leave answer room when the configured
        // cap is too low for this budget.
        let needed = budget + ANTHROPIC_ANSWER_HEADROOM;
        if needed > max_tokens {
            body["max_tokens"] = json!(needed);
        }
    }
    body
}

/// Convert OpenAI-shaped chat history into a top-level `system` string and a list of
/// Anthropic messages (`{role, content:[blocks]}`). System messages are hoisted out;
/// `tool` messages become user `tool_result` turns; assistant `tool_calls` become
/// `tool_use` blocks. Consecutive same-role messages merge their blocks, and dangling
/// `tool_use` ids get a synthetic `tool_result` so an already-broken session still
/// sends (mirrors the Bedrock provider's pairing repair).
fn to_anthropic_messages(messages: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
    let mut system: Vec<String> = Vec::new();
    let mut out: Vec<Value> = Vec::new();

    for msg in messages {
        if msg.role == "system" {
            if let Some(text) = &msg.content {
                if !text.is_empty() {
                    system.push(text.clone());
                }
            }
            continue;
        }

        let (role, blocks) = match msg.role.as_str() {
            "tool" => ("user", tool_result_blocks(msg)),
            "assistant" => ("assistant", assistant_blocks(msg)),
            _ => ("user", text_blocks(msg)),
        };
        if blocks.is_empty() {
            continue;
        }

        // Merge into the previous message when the role matches, keeping a tidy
        // alternation (Anthropic tolerates same-role runs, but merging keeps tool
        // results adjacent to their turn).
        match out.last_mut() {
            Some(last) if last["role"] == role => {
                if let Some(arr) = last["content"].as_array_mut() {
                    arr.extend(blocks);
                }
            }
            _ => out.push(json!({ "role": role, "content": blocks })),
        }
    }

    let system = (!system.is_empty()).then(|| system.join("\n"));
    (system, enforce_tool_result_pairing(out))
}

/// Add `cache_control: {type: "ephemeral"}` to the last content block of the
/// message at `idx`. This tells Anthropic to cache all tokens up to (and
/// including) this block, so subsequent turns that share the prefix skip prefill.
fn mark_anthropic_cache_breakpoint(messages: &mut [Value], idx: usize) {
    if let Some(msg) = messages.get_mut(idx) {
        if let Some(blocks) = msg["content"].as_array_mut() {
            if let Some(last_block) = blocks.last_mut() {
                last_block["cache_control"] = json!({ "type": "ephemeral" });
            }
        }
    }
}

fn text_blocks(msg: &ChatMessage) -> Vec<Value> {
    match &msg.content {
        Some(text) if !text.is_empty() => vec![json!({ "type": "text", "text": text })],
        _ => vec![],
    }
}

fn assistant_blocks(msg: &ChatMessage) -> Vec<Value> {
    let mut blocks = text_blocks(msg);
    if let Some(calls) = &msg.tool_calls {
        for call in calls {
            // tool_use.input must be a JSON object. A no-arg call streams no argument
            // fragments, so its persisted `arguments` is "" and fails to parse --
            // default to {} (mirrors the Bedrock empty-input fix).
            let input = serde_json::from_str::<Value>(&call.function.arguments)
                .ok()
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({}));
            blocks.push(json!({
                "type": "tool_use",
                "id": call.id,
                "name": call.function.name,
                "input": input,
            }));
        }
    }
    blocks
}

fn tool_result_blocks(msg: &ChatMessage) -> Vec<Value> {
    let Some(id) = &msg.tool_call_id else {
        return vec![];
    };
    vec![json!({
        "type": "tool_result",
        "tool_use_id": id,
        "content": msg.content.clone().unwrap_or_default(),
    })]
}

/// Anthropic rejects an assistant `tool_use` block whose id lacks a `tool_result` in
/// the immediately-following user message. A turn whose future was dropped (window
/// closed, command aborted, a new turn started over an in-flight one) can persist a
/// dangling `tool_use`, which then 400s on the next turn. Inject a synthetic
/// `tool_result` for every dangling id so a broken session can still be sent (mirrors
/// the Bedrock provider's repair).
fn enforce_tool_result_pairing(messages: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(messages.len() + 1);
    let mut iter = messages.into_iter().peekable();

    while let Some(msg) = iter.next() {
        let tool_use_ids = collect_block_ids(&msg, "tool_use", "id");
        out.push(msg);
        if tool_use_ids.is_empty() {
            continue;
        }

        // Ids the following message already answers (only a user message can).
        let covered = match iter.peek() {
            Some(next) if next["role"] == "user" => {
                collect_block_ids(next, "tool_result", "tool_use_id")
            }
            _ => Vec::new(),
        };
        let synth: Vec<Value> = tool_use_ids
            .iter()
            .filter(|id| !covered.contains(id))
            .cloned()
            .map(synthetic_tool_result)
            .collect();

        match iter.peek_mut() {
            Some(next) if next["role"] == "user" => {
                if let Some(arr) = next["content"].as_array_mut() {
                    // Strip orphaned tool_result blocks whose IDs don't belong to
                    // this assistant's tool_uses (#744). A cancel+resend race can
                    // interleave results from a parallel loop into the wrong turn.
                    arr.retain(|b| {
                        b["type"] != "tool_result"
                            || b["tool_use_id"]
                                .as_str()
                                .is_some_and(|id| tool_use_ids.contains(&id.to_string()))
                    });
                    if !synth.is_empty() {
                        let existing = std::mem::take(arr);
                        let mut content = synth;
                        content.extend(existing);
                        *arr = content;
                    }
                }
            }
            _ => {
                if !synth.is_empty() {
                    out.push(json!({ "role": "user", "content": synth }));
                }
            }
        }
    }

    // Final sweep: strip orphaned tool_result blocks from user turns whose
    // preceding assistant has no tool_use blocks (#744).
    strip_orphaned_trailing_results(&mut out);
    out
}

/// Remove tool_result blocks from any user turn whose preceding assistant has no
/// tool_use blocks. Such results are orphans from a parallel-loop race (#744).
/// Also drops user turns left empty after stripping, and merges any adjacent
/// same-role messages that result from the removal (strict alternation).
fn strip_orphaned_trailing_results(messages: &mut Vec<Value>) {
    let mut i = 1;
    while i < messages.len() {
        if messages[i]["role"] == "user" {
            let prev_has_uses = i > 0
                && messages[i - 1]["role"] == "assistant"
                && messages[i - 1]["content"]
                    .as_array()
                    .is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "tool_use"));
            if !prev_has_uses {
                if let Some(arr) = messages[i]["content"].as_array_mut() {
                    arr.retain(|b| b["type"] != "tool_result");
                    if arr.is_empty() {
                        messages.remove(i);
                        // Merge adjacent same-role messages to restore alternation.
                        if i < messages.len()
                            && i > 0
                            && messages[i]["role"] == messages[i - 1]["role"]
                        {
                            if let Some(absorbed) =
                                messages.remove(i)["content"].as_array().cloned()
                            {
                                if let Some(target) = messages[i - 1]["content"].as_array_mut() {
                                    target.extend(absorbed);
                                }
                            }
                        }
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
}

fn collect_block_ids(msg: &Value, block_type: &str, id_field: &str) -> Vec<String> {
    msg["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == block_type)
                .filter_map(|b| b[id_field].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn synthetic_tool_result(id: String) -> Value {
    json!({
        "type": "tool_result",
        "tool_use_id": id,
        "content": "[no result recorded]",
    })
}

/// Map OpenAI `tools` entries to Anthropic `{name, description, input_schema}`.
fn to_anthropic_tools(tools: &[Value]) -> Option<Vec<Value>> {
    let specs: Vec<Value> = tools.iter().filter_map(to_anthropic_tool).collect();
    (!specs.is_empty()).then_some(specs)
}

fn to_anthropic_tool(spec: &Value) -> Option<Value> {
    let function = spec.get("function").unwrap_or(spec);
    let name = function.get("name")?.as_str()?;
    let mut tool = json!({
        "name": name,
        "input_schema": normalize_object_schema(function.get("parameters")),
    });
    if let Some(desc) = function.get("description").and_then(Value::as_str) {
        tool["description"] = Value::String(desc.to_string());
    }
    Some(tool)
}

/// Coerce a tool parameter schema into an Anthropic-valid object schema: a JSON object
/// whose top-level `type` is `"object"`. Mirrors the Bedrock normalization -- an object
/// schema that merely omits `type` keeps its `properties`/`required` and gets
/// `"type":"object"` injected; anything else becomes a minimal
/// `{"type":"object","properties":{}}`.
fn normalize_object_schema(params: Option<&Value>) -> Value {
    match params {
        Some(Value::Object(map)) if map.get("type").and_then(Value::as_str) == Some("object") => {
            Value::Object(map.clone())
        }
        Some(Value::Object(map)) if !map.is_empty() && !map.contains_key("type") => {
            let mut m = map.clone();
            m.insert("type".into(), Value::String("object".into()));
            Value::Object(m)
        }
        _ => json!({ "type": "object", "properties": {} }),
    }
}

/// Pull `error.message` out of an Anthropic error body
/// (`{"type":"error","error":{"type","message"}}`).
fn extract_error_message(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("error")?
        .get("message")?
        .as_str()
        .map(String::from)
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let body = self.emitted_body_for(&req);

        let resp = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            // Surface Anthropic's error.message when present, else the raw body.
            let message = match resp.text().await {
                Ok(body) => extract_error_message(&body).unwrap_or(body),
                Err(e) => e.to_string(),
            };
            return Err(LlmError::Api {
                status: status.as_u16(),
                message,
            });
        }

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
                        if let Some(chunk) = parse_anthropic_line(line) {
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

    /// `GET {base_url}/v1/models` -> `{ "data": [ { "id": ... } ] }`.
    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        let resp = self
            .client
            .get(format!("{}/v1/models", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
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

    /// Fire a minimal turn and pull the first stream event; an auth, header, or
    /// model-id failure surfaces as the first item's error. Mirrors the Bedrock
    /// converse-probe -- validates the actual model id, not just the key. Dropping the
    /// stream after the first event aborts the request, so token spend is negligible.
    async fn test_connection(&self, model: &str) -> Result<(), LlmError> {
        let req = ChatRequest {
            model: model.to_string(),
            messages: vec![ChatMessage::text("user", "ping")],
            tools: Vec::new(),
            thinking: false,
            max_tokens: None,
            cache_messages: false,
        };
        let mut stream = self.chat_stream(req).await?;
        match stream.next().await {
            Some(Err(e)) => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
