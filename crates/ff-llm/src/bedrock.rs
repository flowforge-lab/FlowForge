//! Amazon Bedrock provider via the `ConverseStream` API (#202). Supports three
//! credential modes — a named `~/.aws` profile, hardcoded IAM keys, and a bearer
//! API key — selected at construction by [`BedrockCreds`]. Secret material is read
//! from the OS keychain by the desktop host and injected here; this crate never
//! touches the keychain itself.

use async_trait::async_trait;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ContentBlockDelta, ContentBlockStart, ConversationRole, ConverseStreamOutput,
    Message, ReasoningContentBlockDelta, SystemContentBlock, Tool, ToolConfiguration,
    ToolInputSchema, ToolResultBlock, ToolResultContentBlock, ToolSpecification, ToolUseBlock,
};
use aws_sdk_bedrockruntime::Client;
use aws_smithy_types::{Document, Number};
use futures_util::stream::{self, StreamExt};

use crate::{ChatMessage, ChatRequest, Chunk, ChunkStream, LlmError, Provider, ToolCallDelta};

/// Which credential source the provider uses to sign requests. Built by the
/// desktop host from a `ProviderConnection` plus any keychain secrets.
#[derive(Clone)]
pub enum BedrockCreds {
    /// A named profile from `~/.aws/{config,credentials}`.
    Profile { name: String },
    /// Hardcoded IAM access keys (with an optional session token).
    IamKeys {
        access_key_id: String,
        secret_access_key: String,
        session_token: Option<String>,
    },
    /// A Bedrock bearer API key.
    ApiKey { token: String },
}

// Manual Debug that never prints secret material (secret access key, session
// token, bearer token), guarding against an accidental future `tracing::debug!(?creds)`
// leaking credentials. The access key id is a non-secret identifier, so it is kept.
impl std::fmt::Debug for BedrockCreds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BedrockCreds::Profile { name } => {
                f.debug_struct("Profile").field("name", name).finish()
            }
            BedrockCreds::IamKeys {
                access_key_id,
                session_token,
                ..
            } => f
                .debug_struct("IamKeys")
                .field("access_key_id", access_key_id)
                .field("secret_access_key", &"<redacted>")
                .field(
                    "session_token",
                    &session_token.as_ref().map(|_| "<redacted>"),
                )
                .finish(),
            BedrockCreds::ApiKey { .. } => f
                .debug_struct("ApiKey")
                .field("token", &"<redacted>")
                .finish(),
        }
    }
}

pub struct BedrockProvider {
    region: String,
    creds: BedrockCreds,
}

impl BedrockProvider {
    pub fn new(region: impl Into<String>, creds: BedrockCreds) -> Self {
        Self {
            region: region.into(),
            creds,
        }
    }

    /// Build a Bedrock client for the configured region and credential mode.
    /// A rustls-ring HTTP client is wired explicitly so we never pull aws-lc-rs.
    async fn client(&self) -> Client {
        use aws_sdk_bedrockruntime::config::{BehaviorVersion, Credentials, Region, Token};

        let http = aws_smithy_http_client::Builder::new()
            .tls_provider(aws_smithy_http_client::tls::Provider::Rustls(
                aws_smithy_http_client::tls::rustls_provider::CryptoMode::Ring,
            ))
            .build_https();
        let region = Region::new(self.region.clone());

        match &self.creds {
            BedrockCreds::Profile { name } => {
                let shared = aws_config::defaults(BehaviorVersion::latest())
                    .region(region)
                    .profile_name(name)
                    .http_client(http)
                    .load()
                    .await;
                Client::new(&shared)
            }
            BedrockCreds::IamKeys {
                access_key_id,
                secret_access_key,
                session_token,
            } => {
                let creds = Credentials::from_keys(
                    access_key_id.clone(),
                    secret_access_key.clone(),
                    session_token.clone(),
                );
                let conf = aws_sdk_bedrockruntime::config::Builder::new()
                    .behavior_version(BehaviorVersion::latest())
                    .region(region)
                    .http_client(http)
                    .credentials_provider(creds)
                    .build();
                Client::from_conf(conf)
            }
            BedrockCreds::ApiKey { token } => {
                let conf = aws_sdk_bedrockruntime::config::Builder::new()
                    .behavior_version(BehaviorVersion::latest())
                    .region(region)
                    .http_client(http)
                    .bearer_token(Token::new(token.clone(), None))
                    .build();
                Client::from_conf(conf)
            }
        }
    }

    /// Build a Bedrock *control-plane* client, used only by `list_models`
    /// (ListInferenceProfiles). Mirrors [`Self::client`] per credential mode; the
    /// control-plane SDK crate has its own config `Builder` type, so the match
    /// cannot be shared generically with the runtime client.
    async fn control_client(&self) -> aws_sdk_bedrock::Client {
        use aws_sdk_bedrock::config::{BehaviorVersion, Credentials, Region, Token};

        let http = aws_smithy_http_client::Builder::new()
            .tls_provider(aws_smithy_http_client::tls::Provider::Rustls(
                aws_smithy_http_client::tls::rustls_provider::CryptoMode::Ring,
            ))
            .build_https();
        let region = Region::new(self.region.clone());

        match &self.creds {
            BedrockCreds::Profile { name } => {
                let shared = aws_config::defaults(BehaviorVersion::latest())
                    .region(region)
                    .profile_name(name)
                    .http_client(http)
                    .load()
                    .await;
                aws_sdk_bedrock::Client::new(&shared)
            }
            BedrockCreds::IamKeys {
                access_key_id,
                secret_access_key,
                session_token,
            } => {
                let creds = Credentials::from_keys(
                    access_key_id.clone(),
                    secret_access_key.clone(),
                    session_token.clone(),
                );
                let conf = aws_sdk_bedrock::config::Builder::new()
                    .behavior_version(BehaviorVersion::latest())
                    .region(region)
                    .http_client(http)
                    .credentials_provider(creds)
                    .build();
                aws_sdk_bedrock::Client::from_conf(conf)
            }
            BedrockCreds::ApiKey { token } => {
                let conf = aws_sdk_bedrock::config::Builder::new()
                    .behavior_version(BehaviorVersion::latest())
                    .region(region)
                    .http_client(http)
                    .bearer_token(Token::new(token.clone(), None))
                    .build();
                aws_sdk_bedrock::Client::from_conf(conf)
            }
        }
    }
}

#[async_trait]
impl Provider for BedrockProvider {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let (system, messages) = to_converse(&req.messages);
        let client = self.client().await;

        let mut call = client
            .converse_stream()
            .model_id(req.model)
            .set_messages(Some(messages));
        if !system.is_empty() {
            call = call.set_system(Some(system));
        }
        if let Some(cfg) = to_tool_config(&req.tools) {
            call = call.tool_config(cfg);
        }

        let output = call.send().await.map_err(map_sdk_err)?;
        let receiver = output.stream;

        let stream = stream::unfold(Some(receiver), |state| async move {
            let mut rx = state?;
            loop {
                match rx.recv().await {
                    Ok(Some(event)) => {
                        if let Some(chunk) = event_to_chunk(event) {
                            return Some((Ok(chunk), Some(rx)));
                        }
                    }
                    Ok(None) => return None,
                    Err(e) => return Some((Err(map_sdk_err(e)), None)),
                }
            }
        })
        .boxed();

        Ok(stream)
    }

    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        let client = self.control_client().await;
        let out = client
            .list_inference_profiles()
            .send()
            .await
            .map_err(map_sdk_err)?;
        Ok(out
            .inference_profile_summaries()
            .iter()
            .filter(|p| *p.status() == aws_sdk_bedrock::types::InferenceProfileStatus::Active)
            .map(|p| p.inference_profile_id().to_string())
            .collect())
    }

    async fn test_connection(&self, model: &str) -> Result<(), LlmError> {
        // Converse-probe: fire a minimal turn against the configured model and
        // pull the first event. This exercises the exact send path the app uses,
        // and the active auth mode, for all three credential modes — a list-based
        // probe can pass for an ApiKey token that lacks bedrock:ListInferenceProfiles
        // yet can converse. Auth/construction errors surface from `chat_stream`;
        // per-stream errors (e.g. access-denied) surface on the first event.
        let req = ChatRequest {
            model: model.to_string(),
            messages: vec![ChatMessage::text("user", "ping")],
            tools: Vec::new(),
            thinking: false,
        };
        let mut stream = self.chat_stream(req).await?;
        match stream.next().await {
            Some(Err(e)) => Err(e),
            _ => Ok(()),
        }
    }
}

/// Map one `ConverseStream` event to a [`Chunk`], or `None` for events that carry
/// no streamable payload (message start, content-block stop, metadata).
fn event_to_chunk(event: ConverseStreamOutput) -> Option<Chunk> {
    match event {
        ConverseStreamOutput::ContentBlockStart(ev) => {
            // A tool-use block announces its id + name before any argument bytes.
            if let Some(ContentBlockStart::ToolUse(start)) = ev.start {
                return Some(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: ev.content_block_index as u32,
                        id: Some(start.tool_use_id),
                        name: Some(start.name),
                        arguments: String::new(),
                    }],
                    ..Chunk::default()
                });
            }
            None
        }
        ConverseStreamOutput::ContentBlockDelta(ev) => match ev.delta {
            Some(ContentBlockDelta::Text(text)) => Some(Chunk {
                delta: text,
                ..Chunk::default()
            }),
            Some(ContentBlockDelta::ToolUse(delta)) => Some(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: ev.content_block_index as u32,
                    arguments: delta.input,
                    ..ToolCallDelta::default()
                }],
                ..Chunk::default()
            }),
            Some(ContentBlockDelta::ReasoningContent(ReasoningContentBlockDelta::Text(text))) => {
                Some(Chunk {
                    reasoning_delta: text,
                    ..Chunk::default()
                })
            }
            _ => None,
        },
        ConverseStreamOutput::MessageStop(_) => Some(Chunk {
            done: true,
            ..Chunk::default()
        }),
        _ => None,
    }
}

/// Convert OpenAI-shaped chat history into Converse `system` blocks and a strictly
/// alternating user/assistant message list. Consecutive messages that map to the
/// same role are merged, as the Converse API requires alternation.
fn to_converse(messages: &[ChatMessage]) -> (Vec<SystemContentBlock>, Vec<Message>) {
    let mut system = Vec::new();
    let mut out: Vec<Message> = Vec::new();

    for msg in messages {
        if msg.role == "system" {
            if let Some(text) = &msg.content {
                system.push(SystemContentBlock::Text(text.clone()));
            }
            continue;
        }

        let (role, blocks) = match msg.role.as_str() {
            "tool" => (ConversationRole::User, tool_result_blocks(msg)),
            "assistant" => (ConversationRole::Assistant, assistant_blocks(msg)),
            _ => (ConversationRole::User, text_blocks(msg)),
        };
        if blocks.is_empty() {
            continue;
        }

        // Merge into the previous message when the mapped role matches, preserving
        // Converse's strict user/assistant alternation.
        match out.last_mut() {
            Some(last) if last.role == role => last.content.extend(blocks),
            _ => out.push(
                Message::builder()
                    .role(role)
                    .set_content(Some(blocks))
                    .build()
                    .expect("role is always set"),
            ),
        }
    }

    (system, out)
}

fn text_blocks(msg: &ChatMessage) -> Vec<ContentBlock> {
    match &msg.content {
        Some(text) if !text.is_empty() => vec![ContentBlock::Text(text.clone())],
        _ => vec![],
    }
}

fn assistant_blocks(msg: &ChatMessage) -> Vec<ContentBlock> {
    let mut blocks = text_blocks(msg);
    if let Some(calls) = &msg.tool_calls {
        for call in calls {
            let input = serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                .map(|v| json_to_doc(&v))
                .unwrap_or(Document::Null);
            if let Ok(block) = ToolUseBlock::builder()
                .tool_use_id(call.id.clone())
                .name(call.function.name.clone())
                .input(input)
                .build()
            {
                blocks.push(ContentBlock::ToolUse(block));
            }
        }
    }
    blocks
}

fn tool_result_blocks(msg: &ChatMessage) -> Vec<ContentBlock> {
    let Some(id) = &msg.tool_call_id else {
        return vec![];
    };
    let content = msg
        .content
        .clone()
        .map(|t| vec![ToolResultContentBlock::Text(t)])
        .unwrap_or_default();
    match ToolResultBlock::builder()
        .tool_use_id(id.clone())
        .set_content(Some(content))
        .build()
    {
        Ok(block) => vec![ContentBlock::ToolResult(block)],
        Err(_) => vec![],
    }
}

/// Translate OpenAI `tools` specs into a Converse [`ToolConfiguration`]. Returns
/// `None` for a plain chat turn (no tools).
fn to_tool_config(tools: &[serde_json::Value]) -> Option<ToolConfiguration> {
    let specs: Vec<Tool> = tools.iter().filter_map(to_tool).collect();
    if specs.is_empty() {
        return None;
    }
    ToolConfiguration::builder()
        .set_tools(Some(specs))
        .build()
        .ok()
}

fn to_tool(spec: &serde_json::Value) -> Option<Tool> {
    let function = spec.get("function").unwrap_or(spec);
    let name = function.get("name")?.as_str()?.to_string();
    let mut builder = ToolSpecification::builder().name(name);
    if let Some(desc) = function.get("description").and_then(|d| d.as_str()) {
        builder = builder.description(desc);
    }
    // Bedrock Converse requires every tool inputSchema.json to be a JSON Schema
    // with a top-level `"type": "object"`. OpenAI-style specs -- and MCP server
    // schemas, which we pass through verbatim -- do not always satisfy this, so
    // normalize first. inputSchema is required, so set it even when the spec omits
    // `parameters`.
    let schema = normalize_object_schema(function.get("parameters"));
    builder = builder.input_schema(ToolInputSchema::Json(json_to_doc(&schema)));
    builder.build().ok().map(Tool::ToolSpec)
}

/// Coerce a tool parameter schema into a Bedrock-valid object schema: a JSON
/// object whose top-level `type` is `"object"`. An object schema that merely omits
/// `type` keeps its `properties`/`required` and gets `"type":"object"` injected;
/// anything else (an empty `{}`, a non-object `type`, a non-object value, or
/// `None`) becomes a minimal `{"type":"object","properties":{}}`.
fn normalize_object_schema(params: Option<&serde_json::Value>) -> serde_json::Value {
    use serde_json::{json, Value};
    match params {
        Some(Value::Object(map)) if map.get("type").and_then(|t| t.as_str()) == Some("object") => {
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

fn json_to_doc(value: &serde_json::Value) -> Document {
    use serde_json::Value as J;
    match value {
        J::Null => Document::Null,
        J::Bool(b) => Document::Bool(*b),
        J::Number(n) => {
            if let Some(u) = n.as_u64() {
                Document::Number(Number::PosInt(u))
            } else if let Some(i) = n.as_i64() {
                Document::Number(Number::NegInt(i))
            } else {
                Document::Number(Number::Float(n.as_f64().unwrap_or(0.0)))
            }
        }
        J::String(s) => Document::String(s.clone()),
        J::Array(a) => Document::Array(a.iter().map(json_to_doc).collect()),
        J::Object(o) => {
            Document::Object(o.iter().map(|(k, v)| (k.clone(), json_to_doc(v))).collect())
        }
    }
}

#[cfg(test)]
fn doc_to_json(doc: &Document) -> serde_json::Value {
    use serde_json::Value as J;
    match doc {
        Document::Null => J::Null,
        Document::Bool(b) => J::Bool(*b),
        Document::Number(Number::PosInt(u)) => J::from(*u),
        Document::Number(Number::NegInt(i)) => J::from(*i),
        Document::Number(Number::Float(f)) => J::from(*f),
        Document::String(s) => J::String(s.clone()),
        Document::Array(a) => J::Array(a.iter().map(doc_to_json).collect()),
        Document::Object(o) => {
            J::Object(o.iter().map(|(k, v)| (k.clone(), doc_to_json(v))).collect())
        }
    }
}

/// Classify an SDK error. We surface the upstream HTTP status when present so the
/// retry layer can distinguish throttling/5xx (transient) from validation (fatal).
fn map_sdk_err<E, R>(err: aws_sdk_bedrockruntime::error::SdkError<E, R>) -> LlmError
where
    E: std::fmt::Debug + aws_smithy_types::error::metadata::ProvideErrorMetadata,
{
    use aws_sdk_bedrockruntime::error::SdkError;
    match err {
        SdkError::ServiceError(ctx) => {
            let inner = ctx.err();
            // Throttling and server-side faults are retryable; validation is not.
            // Fixed code list: a new 5xx-class exception AWS adds later that is not
            // one of these would fall through to fatal — revisit if that bites.
            let transient = matches!(
                inner.code(),
                Some("ThrottlingException")
                    | Some("ServiceUnavailableException")
                    | Some("ModelTimeoutException")
                    | Some("InternalServerException")
            );
            let message = format!("{inner:?}");
            if transient {
                LlmError::Transport(message)
            } else {
                LlmError::Api { status: 0, message }
            }
        }
        SdkError::TimeoutError(_) => LlmError::Transport("request timed out".into()),
        SdkError::DispatchFailure(_) => LlmError::Transport("dispatch failure".into()),
        SdkError::ResponseError(_) => LlmError::Transport("malformed response".into()),
        SdkError::ConstructionFailure(_) => {
            LlmError::Transport("request construction failure".into())
        }
        _ => LlmError::Transport("unknown SDK error".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FunctionCall, ToolCall};
    use aws_sdk_bedrockruntime::types::{
        ContentBlockDeltaEvent, ContentBlockStartEvent, ToolUseBlockDelta, ToolUseBlockStart,
    };

    fn assistant_with_call(args: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "bash".into(),
                    arguments: args.into(),
                },
            }]),
            tool_call_id: None,
            name: None,
        }
    }

    #[test]
    fn system_messages_become_system_blocks() {
        let (system, messages) = to_converse(&[
            ChatMessage::text("system", "be brief"),
            ChatMessage::text("user", "hi"),
        ]);
        assert_eq!(system.len(), 1);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, ConversationRole::User);
    }

    #[test]
    fn consecutive_same_role_messages_merge() {
        let (_, messages) = to_converse(&[
            ChatMessage::text("user", "a"),
            ChatMessage::text("user", "b"),
            ChatMessage::text("assistant", "c"),
        ]);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content.len(), 2);
        assert_eq!(messages[1].role, ConversationRole::Assistant);
    }

    #[test]
    fn tool_role_maps_to_user_tool_result() {
        let msg = ChatMessage {
            role: "tool".into(),
            content: Some("result body".into()),
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            name: Some("bash".into()),
        };
        let (_, messages) = to_converse(&[msg]);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, ConversationRole::User);
        assert!(matches!(
            messages[0].content[0],
            ContentBlock::ToolResult(_)
        ));
    }

    #[test]
    fn assistant_tool_calls_become_tool_use_blocks() {
        let (_, messages) = to_converse(&[assistant_with_call(r#"{"command":"ls"}"#)]);
        let block = &messages[0].content[0];
        match block {
            ContentBlock::ToolUse(b) => {
                assert_eq!(b.tool_use_id, "call_1");
                assert_eq!(b.name, "bash");
            }
            _ => panic!("expected tool-use block"),
        }
    }

    #[test]
    fn text_delta_maps_to_chunk() {
        let event = ConverseStreamOutput::ContentBlockDelta(
            ContentBlockDeltaEvent::builder()
                .delta(ContentBlockDelta::Text("Hello".into()))
                .content_block_index(0)
                .build()
                .unwrap(),
        );
        let chunk = event_to_chunk(event).unwrap();
        assert_eq!(chunk.delta, "Hello");
        assert!(!chunk.done);
    }

    #[test]
    fn reasoning_delta_maps_to_reasoning() {
        let event = ConverseStreamOutput::ContentBlockDelta(
            ContentBlockDeltaEvent::builder()
                .delta(ContentBlockDelta::ReasoningContent(
                    ReasoningContentBlockDelta::Text("thinking".into()),
                ))
                .content_block_index(0)
                .build()
                .unwrap(),
        );
        let chunk = event_to_chunk(event).unwrap();
        assert_eq!(chunk.reasoning_delta, "thinking");
    }

    #[test]
    fn tool_use_start_then_delta_preserves_json_args() {
        let start = ConverseStreamOutput::ContentBlockStart(
            ContentBlockStartEvent::builder()
                .start(ContentBlockStart::ToolUse(
                    ToolUseBlockStart::builder()
                        .tool_use_id("call_9")
                        .name("bash")
                        .build()
                        .unwrap(),
                ))
                .content_block_index(1)
                .build()
                .unwrap(),
        );
        let chunk = event_to_chunk(start).unwrap();
        assert_eq!(chunk.tool_calls[0].index, 1);
        assert_eq!(chunk.tool_calls[0].id.as_deref(), Some("call_9"));
        assert_eq!(chunk.tool_calls[0].name.as_deref(), Some("bash"));

        let delta = ConverseStreamOutput::ContentBlockDelta(
            ContentBlockDeltaEvent::builder()
                .delta(ContentBlockDelta::ToolUse(
                    ToolUseBlockDelta::builder()
                        .input(r#"{"command":"ls"}"#)
                        .build()
                        .unwrap(),
                ))
                .content_block_index(1)
                .build()
                .unwrap(),
        );
        let chunk = event_to_chunk(delta).unwrap();
        // Arguments stay a JSON string fragment, matching the app-wide tool-call contract.
        assert_eq!(chunk.tool_calls[0].arguments, r#"{"command":"ls"}"#);
        assert!(chunk.tool_calls[0].id.is_none());
    }

    #[test]
    fn message_stop_marks_done() {
        let event = ConverseStreamOutput::MessageStop(
            aws_sdk_bedrockruntime::types::MessageStopEvent::builder()
                .stop_reason(aws_sdk_bedrockruntime::types::StopReason::EndTurn)
                .build()
                .unwrap(),
        );
        let chunk = event_to_chunk(event).unwrap();
        assert!(chunk.done);
    }

    #[test]
    fn tool_config_built_from_openai_spec() {
        let spec = serde_json::json!({
            "type": "function",
            "function": {
                "name": "bash",
                "description": "run a shell command",
                "parameters": {
                    "type": "object",
                    "properties": { "command": { "type": "string" } },
                    "required": ["command"]
                }
            }
        });
        let cfg = to_tool_config(&[spec]).unwrap();
        assert_eq!(cfg.tools.len(), 1);
        match &cfg.tools[0] {
            Tool::ToolSpec(s) => assert_eq!(s.name, "bash"),
            _ => panic!("expected tool spec"),
        }
    }

    #[test]
    fn no_tools_yields_no_config() {
        assert!(to_tool_config(&[]).is_none());
    }

    #[test]
    fn schema_with_object_type_passes_through() {
        let params = serde_json::json!({
            "type": "object",
            "properties": { "x": { "type": "string" } },
            "required": ["x"]
        });
        assert_eq!(normalize_object_schema(Some(&params)), params);
    }

    #[test]
    fn schema_missing_type_gets_object_injected() {
        let params = serde_json::json!({ "properties": { "x": { "type": "string" } } });
        let out = normalize_object_schema(Some(&params));
        assert_eq!(out["type"], "object");
        assert_eq!(out["properties"]["x"]["type"], "string");
    }

    #[test]
    fn empty_or_non_object_schemas_become_object() {
        let want = serde_json::json!({ "type": "object", "properties": {} });
        assert_eq!(normalize_object_schema(Some(&serde_json::json!({}))), want);
        assert_eq!(
            normalize_object_schema(Some(&serde_json::json!({ "type": "string" }))),
            want
        );
        assert_eq!(normalize_object_schema(None), want);
    }

    #[test]
    fn to_tool_always_sends_object_typed_schema() {
        // An MCP-style spec whose params lack a top-level object type must still
        // reach Bedrock as a valid object schema (the #202 ValidationException).
        let spec = serde_json::json!({
            "type": "function",
            "function": {
                "name": "weird",
                "description": "no top-level type",
                "parameters": { "properties": { "q": { "type": "string" } } }
            }
        });
        let cfg = to_tool_config(&[spec]).unwrap();
        match &cfg.tools[0] {
            Tool::ToolSpec(s) => match s.input_schema.as_ref().unwrap() {
                ToolInputSchema::Json(doc) => {
                    assert_eq!(doc_to_json(doc)["type"], "object");
                }
                _ => panic!("expected json input schema"),
            },
            _ => panic!("expected tool spec"),
        }
    }

    #[test]
    fn json_document_round_trips() {
        let value = serde_json::json!({
            "s": "text",
            "n": 42,
            "neg": -7,
            "f": 1.5,
            "b": true,
            "nil": null,
            "arr": [1, 2, 3],
            "nested": { "k": "v" }
        });
        let doc = json_to_doc(&value);
        assert_eq!(doc_to_json(&doc), value);
    }

    #[test]
    fn creds_debug_redacts_secrets() {
        let iam = BedrockCreds::IamKeys {
            access_key_id: "AKIAEXAMPLE".into(),
            secret_access_key: "super-secret-key".into(),
            session_token: Some("super-secret-token".into()),
        };
        let s = format!("{iam:?}");
        assert!(
            !s.contains("super-secret-key"),
            "secret access key leaked: {s}"
        );
        assert!(
            !s.contains("super-secret-token"),
            "session token leaked: {s}"
        );
        // The access key id is a non-secret identifier and stays visible.
        assert!(
            s.contains("AKIAEXAMPLE"),
            "access key id should be shown: {s}"
        );

        let api = BedrockCreds::ApiKey {
            token: "br-super-secret-bearer".into(),
        };
        let s = format!("{api:?}");
        assert!(
            !s.contains("br-super-secret-bearer"),
            "bearer token leaked: {s}"
        );

        // A None session token must not render the redaction placeholder as present.
        let iam_none = BedrockCreds::IamKeys {
            access_key_id: "AKIA2".into(),
            secret_access_key: "k".into(),
            session_token: None,
        };
        let s = format!("{iam_none:?}");
        assert!(
            s.contains("None"),
            "absent session token should read None: {s}"
        );
    }

    #[test]
    fn creds_modes_construct() {
        let _ = BedrockProvider::new(
            "us-east-2",
            BedrockCreds::ApiKey {
                token: "secret".into(),
            },
        );
        let _ = BedrockProvider::new(
            "us-east-2",
            BedrockCreds::Profile {
                name: "default".into(),
            },
        );
        let _ = BedrockProvider::new(
            "us-east-2",
            BedrockCreds::IamKeys {
                access_key_id: "AKIA".into(),
                secret_access_key: "secret".into(),
                session_token: None,
            },
        );
    }
}
