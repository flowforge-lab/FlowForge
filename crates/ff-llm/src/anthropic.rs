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
    let (system, messages) = to_anthropic_messages(&req.messages);
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
        };
        let mut stream = self.chat_stream(req).await?;
        match stream.next().await {
            Some(Err(e)) => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FunctionCall, ToolCall};

    fn tool_call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
        }
    }

    fn assistant_with_calls(calls: Vec<ToolCall>) -> ChatMessage {
        ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(calls),
            tool_call_id: None,
            name: None,

            attachments: Vec::new(),
            reasoning: None,
        }
    }

    fn tool_msg(id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(id.into()),
            name: None,

            attachments: Vec::new(),
            reasoning: None,
        }
    }

    // --- history -> messages translation ------------------------------------

    #[test]
    fn system_messages_become_system_string() {
        let msgs = vec![
            ChatMessage::text("system", "be brief"),
            ChatMessage::text("user", "hi"),
        ];
        let (system, out) = to_anthropic_messages(&msgs);
        assert_eq!(system.as_deref(), Some("be brief"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["content"][0]["text"], "hi");
    }

    #[test]
    fn user_summary_stays_in_messages_not_hoisted() {
        // A compaction summary uses role=user precisely so it keeps its
        // chronological slot: only system-role messages are hoisted into the
        // top-level system param, which would tear it before the recent tail.
        let msgs = vec![
            ChatMessage::text("system", "be brief"),
            ChatMessage::text("user", "Summary of 40 earlier messages"),
            ChatMessage::text("assistant", "recent verbatim reply"),
        ];
        let (system, out) = to_anthropic_messages(&msgs);
        assert_eq!(system.as_deref(), Some("be brief"));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(
            out[0]["content"][0]["text"],
            "Summary of 40 earlier messages"
        );
        assert_eq!(out[1]["role"], "assistant");
        assert_eq!(out[1]["content"][0]["text"], "recent verbatim reply");
    }

    #[test]
    fn multiple_system_messages_join() {
        let msgs = vec![
            ChatMessage::text("system", "a"),
            ChatMessage::text("system", "b"),
            ChatMessage::text("user", "hi"),
        ];
        let (system, _) = to_anthropic_messages(&msgs);
        assert_eq!(system.as_deref(), Some("a\nb"));
    }

    #[test]
    fn consecutive_same_role_messages_merge() {
        let msgs = vec![
            ChatMessage::text("user", "one"),
            ChatMessage::text("user", "two"),
        ];
        let (_, out) = to_anthropic_messages(&msgs);
        assert_eq!(out.len(), 1);
        let content = out[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["text"], "one");
        assert_eq!(content[1]["text"], "two");
    }

    #[test]
    fn tool_role_maps_to_user_tool_result() {
        let (_, out) = to_anthropic_messages(&[tool_msg("toolu_1", "42")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        let block = &out[0]["content"][0];
        assert_eq!(block["type"], "tool_result");
        assert_eq!(block["tool_use_id"], "toolu_1");
        assert_eq!(block["content"], "42");
    }

    #[test]
    fn tool_message_without_id_is_skipped() {
        let msg = ChatMessage {
            role: "tool".into(),
            content: Some("orphan".into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,

            attachments: Vec::new(),
            reasoning: None,
        };
        let (_, out) = to_anthropic_messages(&[msg]);
        assert!(out.is_empty());
    }

    #[test]
    fn assistant_tool_calls_become_tool_use_blocks() {
        let msg = ChatMessage {
            role: "assistant".into(),
            content: Some("let me check".into()),
            tool_calls: Some(vec![tool_call(
                "toolu_1",
                "get_weather",
                r#"{"location":"SF"}"#,
            )]),
            tool_call_id: None,
            name: None,

            attachments: Vec::new(),
            reasoning: None,
        };
        let (_, out) = to_anthropic_messages(&[msg]);
        let content = out[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "let me check");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "toolu_1");
        assert_eq!(content[1]["name"], "get_weather");
        assert_eq!(content[1]["input"]["location"], "SF");
    }

    #[test]
    fn empty_args_tool_use_becomes_empty_object() {
        let (_, out) =
            to_anthropic_messages(&[assistant_with_calls(vec![tool_call("toolu_1", "noop", "")])]);
        assert_eq!(out[0]["content"][0]["input"], json!({}));
    }

    #[test]
    fn invalid_args_tool_use_becomes_empty_object() {
        let (_, out) = to_anthropic_messages(&[assistant_with_calls(vec![tool_call(
            "toolu_1", "noop", "not json",
        )])]);
        assert_eq!(out[0]["content"][0]["input"], json!({}));
    }

    #[test]
    fn null_args_tool_use_becomes_empty_object() {
        let (_, out) = to_anthropic_messages(&[assistant_with_calls(vec![tool_call(
            "toolu_1", "noop", "null",
        )])]);
        assert_eq!(out[0]["content"][0]["input"], json!({}));
    }

    #[test]
    fn object_args_are_preserved() {
        let (_, out) = to_anthropic_messages(&[assistant_with_calls(vec![tool_call(
            "toolu_1",
            "f",
            r#"{"a":1,"b":"x"}"#,
        )])]);
        assert_eq!(out[0]["content"][0]["input"], json!({"a":1,"b":"x"}));
    }

    // --- tool_use / tool_result pairing repair ------------------------------

    #[test]
    fn dangling_tool_use_gets_synthetic_result() {
        let msgs = vec![
            assistant_with_calls(vec![tool_call("toolu_1", "f", "{}")]),
            ChatMessage::text("user", "next"),
        ];
        let (_, out) = to_anthropic_messages(&msgs);
        assert_eq!(out.len(), 2);
        let user = out[1]["content"].as_array().unwrap();
        assert_eq!(user[0]["type"], "tool_result");
        assert_eq!(user[0]["tool_use_id"], "toolu_1");
        assert_eq!(user[1]["text"], "next");
    }

    #[test]
    fn trailing_tool_use_gets_synthetic_result() {
        let (_, out) =
            to_anthropic_messages(&[assistant_with_calls(vec![tool_call("toolu_1", "f", "{}")])]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1]["role"], "user");
        assert_eq!(out[1]["content"][0]["type"], "tool_result");
        assert_eq!(out[1]["content"][0]["tool_use_id"], "toolu_1");
    }

    #[test]
    fn well_formed_pairing_is_unchanged() {
        let msgs = vec![
            assistant_with_calls(vec![tool_call("toolu_1", "f", "{}")]),
            tool_msg("toolu_1", "ok"),
        ];
        let (_, out) = to_anthropic_messages(&msgs);
        assert_eq!(out.len(), 2);
        let user = out[1]["content"].as_array().unwrap();
        assert_eq!(user.len(), 1);
        assert_eq!(user[0]["content"], "ok");
    }

    #[test]
    fn partial_tool_results_backfilled() {
        let msgs = vec![
            assistant_with_calls(vec![
                tool_call("toolu_1", "f", "{}"),
                tool_call("toolu_2", "g", "{}"),
            ]),
            tool_msg("toolu_1", "ok"),
        ];
        let (_, out) = to_anthropic_messages(&msgs);
        let user = out[1]["content"].as_array().unwrap();
        assert_eq!(user.len(), 2);
        let ids: Vec<&str> = user
            .iter()
            .map(|b| b["tool_use_id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"toolu_1"));
        assert!(ids.contains(&"toolu_2"));
    }

    // --- tool schema mapping ------------------------------------------------

    #[test]
    fn tools_built_from_openai_spec() {
        let tools = vec![json!({
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "weather",
                "parameters": { "type": "object", "properties": { "q": { "type": "string" } } }
            }
        })];
        let out = to_anthropic_tools(&tools).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["name"], "get_weather");
        assert_eq!(out[0]["description"], "weather");
        assert_eq!(out[0]["input_schema"]["type"], "object");
        assert_eq!(out[0]["input_schema"]["properties"]["q"]["type"], "string");
    }

    #[test]
    fn no_tools_yields_none() {
        assert!(to_anthropic_tools(&[]).is_none());
    }

    #[test]
    fn schema_missing_type_gets_object_injected() {
        let tools = vec![json!({
            "function": { "name": "f", "parameters": { "properties": { "q": { "type": "string" } } } }
        })];
        let out = to_anthropic_tools(&tools).unwrap();
        assert_eq!(out[0]["input_schema"]["type"], "object");
        assert_eq!(out[0]["input_schema"]["properties"]["q"]["type"], "string");
    }

    #[test]
    fn empty_or_missing_schema_becomes_object() {
        let tools = vec![json!({ "function": { "name": "f" } })];
        let out = to_anthropic_tools(&tools).unwrap();
        assert_eq!(
            out[0]["input_schema"],
            json!({ "type": "object", "properties": {} })
        );
    }

    // --- SSE event -> Chunk -------------------------------------------------

    #[test]
    fn text_delta_maps_to_chunk() {
        let line = br#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let chunk = parse_anthropic_line(line).unwrap().unwrap();
        assert_eq!(chunk.delta, "Hello");
        assert!(!chunk.done);
    }

    #[test]
    fn thinking_delta_maps_to_reasoning() {
        let line = br#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#;
        let chunk = parse_anthropic_line(line).unwrap().unwrap();
        assert_eq!(chunk.reasoning_delta, "hmm");
    }

    #[test]
    fn tool_use_start_then_input_json_delta_preserves_args() {
        let start = br#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}"#;
        let chunk = parse_anthropic_line(start).unwrap().unwrap();
        assert_eq!(chunk.tool_calls.len(), 1);
        let tc = &chunk.tool_calls[0];
        assert_eq!(tc.index, 1);
        assert_eq!(tc.id.as_deref(), Some("toolu_1"));
        assert_eq!(tc.name.as_deref(), Some("get_weather"));
        assert_eq!(tc.arguments, "");

        let delta = br#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"location\":"}}"#;
        let chunk = parse_anthropic_line(delta).unwrap().unwrap();
        let tc = &chunk.tool_calls[0];
        assert_eq!(tc.index, 1);
        assert!(tc.id.is_none());
        assert!(tc.arguments.contains("location"));
    }

    #[test]
    fn message_stop_marks_done() {
        let chunk = parse_anthropic_line(br#"data: {"type":"message_stop"}"#)
            .unwrap()
            .unwrap();
        assert!(chunk.done);
    }

    #[test]
    fn signature_delta_is_ignored() {
        let line = br#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc"}}"#;
        assert!(parse_anthropic_line(line).is_none());
    }

    #[test]
    fn payload_carrying_events_yield_nothing() {
        assert!(parse_anthropic_line(br#"data: {"type":"ping"}"#).is_none());
        assert!(parse_anthropic_line(
            br#"data: {"type":"message_start","message":{"id":"m","content":[]}}"#
        )
        .is_none());
        assert!(
            parse_anthropic_line(br#"data: {"type":"content_block_stop","index":0}"#).is_none()
        );
        assert!(parse_anthropic_line(
            br#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#
        )
        .is_none());
    }

    #[test]
    fn non_data_and_blank_lines_skipped() {
        assert!(parse_anthropic_line(b"").is_none());
        assert!(parse_anthropic_line(b"event: message_stop").is_none());
        assert!(parse_anthropic_line(b": ping comment").is_none());
    }

    #[test]
    fn error_event_maps_to_api_error() {
        let line =
            br#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        let err = parse_anthropic_line(line).unwrap().unwrap_err();
        match err {
            LlmError::Api { message, .. } => assert_eq!(message, "Overloaded"),
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn malformed_json_is_decode_error() {
        let err = parse_anthropic_line(br#"data: {not json}"#)
            .unwrap()
            .unwrap_err();
        assert!(matches!(err, LlmError::Decode(_)));
    }

    // --- request body -------------------------------------------------------

    #[test]
    fn request_includes_required_max_tokens() {
        let req = ChatRequest {
            model: "claude-x".into(),
            messages: vec![ChatMessage::text("user", "hi")],
            tools: vec![],
            thinking: false,
            max_tokens: None,
        };
        let body = to_anthropic_request(&req, 4096, ReasoningEffort::Medium);
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["model"], "claude-x");
        assert_eq!(body["stream"], true);
        assert!(body.get("thinking").is_none());
        assert!(body.get("system").is_none());
    }

    #[test]
    fn thinking_request_sets_budget_below_max_tokens() {
        let req = ChatRequest {
            model: "claude-x".into(),
            messages: vec![ChatMessage::text("user", "hi")],
            tools: vec![],
            thinking: true,
            max_tokens: None,
        };
        // Medium budget is 4096; with the default 4096 cap, max_tokens is bumped
        // so budget stays strictly below it (Anthropic requirement).
        let body = to_anthropic_request(&req, 4096, ReasoningEffort::Medium);
        assert_eq!(body["thinking"]["type"], "enabled");
        let budget = body["thinking"]["budget_tokens"].as_u64().unwrap();
        let max_tokens = body["max_tokens"].as_u64().unwrap();
        assert_eq!(budget, 4096);
        assert!(
            budget < max_tokens,
            "budget {budget} !< max_tokens {max_tokens}"
        );
    }

    #[test]
    fn thinking_budget_scales_with_effort() {
        let req = ChatRequest {
            model: "claude-x".into(),
            messages: vec![ChatMessage::text("user", "hi")],
            tools: vec![],
            thinking: true,
            max_tokens: None,
        };
        // Budgets are uniform and concrete regardless of max_tokens.
        let low = to_anthropic_request(&req, 32000, ReasoningEffort::Low);
        assert_eq!(low["thinking"]["budget_tokens"], 1024);
        let med = to_anthropic_request(&req, 32000, ReasoningEffort::Medium);
        assert_eq!(med["thinking"]["budget_tokens"], 4096);
        let high = to_anthropic_request(&req, 32000, ReasoningEffort::High);
        assert_eq!(high["thinking"]["budget_tokens"], 8192);
        // A generous cap is left untouched (only bumped when too low).
        assert_eq!(high["max_tokens"], 32000);
    }

    /// #395 acceptance: the provider's private `reasoning_effort` dial (set via
    /// `with_reasoning_effort`) must reach the emitted Anthropic wire body, not
    /// just `to_anthropic_request`'s return value when the effort is passed
    /// directly.  High → `thinking.budget_tokens = 8192`.
    #[test]
    fn high_effort_provider_emits_8192_thinking_budget() {
        let req = ChatRequest {
            model: "claude-x".into(),
            messages: vec![ChatMessage::text("user", "hi")],
            tools: vec![],
            thinking: true,
            max_tokens: None,
        };
        let provider = AnthropicProvider::new("sk-ant-test")
            .with_max_tokens(32000)
            .with_reasoning_effort(ReasoningEffort::High);

        // The effort comes from the provider's private field — not passed
        // explicitly. This proves `chat_stream`'s code path threads the dial.
        let body = provider.emitted_body_for(&req);
        assert_eq!(body["thinking"]["budget_tokens"], 8192);
    }

    #[test]
    fn request_hoists_system_and_tools() {
        let req = ChatRequest {
            model: "claude-x".into(),
            messages: vec![
                ChatMessage::text("system", "sys"),
                ChatMessage::text("user", "hi"),
            ],
            tools: vec![
                json!({"function":{"name":"f","parameters":{"type":"object","properties":{}}}}),
            ],
            thinking: false,
            max_tokens: None,
        };
        let body = to_anthropic_request(&req, 100, ReasoningEffort::Medium);
        // System is now a block array with a cache breakpoint (#437).
        assert_eq!(body["system"][0]["text"], "sys");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["tools"][0]["name"], "f");
    }

    // #437: the system prefix and the *last* tool each carry a cache breakpoint,
    // so the stable tools+system prefix is cached from turn 2 onward.
    #[test]
    fn system_and_last_tool_carry_cache_control() {
        let req = ChatRequest {
            model: "claude-x".into(),
            messages: vec![ChatMessage::text("system", "sys")],
            tools: vec![
                json!({"function":{"name":"a","parameters":{"type":"object","properties":{}}}}),
                json!({"function":{"name":"b","parameters":{"type":"object","properties":{}}}}),
            ],
            thinking: false,
            max_tokens: None,
        };
        let body = to_anthropic_request(&req, 100, ReasoningEffort::Medium);
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        // Only the last tool gets the breakpoint; the first stays plain.
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(body["tools"][1]["name"], "b");
        assert_eq!(body["tools"][1]["cache_control"]["type"], "ephemeral");
    }

    // --- creds --------------------------------------------------------------

    #[test]
    fn debug_redacts_api_key() {
        let p = AnthropicProvider::new("sk-ant-secret");
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("sk-ant-secret"));
        assert!(dbg.contains("<redacted>"));
    }
}
