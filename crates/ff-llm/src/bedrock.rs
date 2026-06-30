//! Amazon Bedrock provider via the `ConverseStream` API (#202). Supports three
//! credential modes — a named `~/.aws` profile, hardcoded IAM keys, and a bearer
//! API key — selected at construction by [`BedrockCreds`]. Secret material is read
//! from the OS keychain by the desktop host and injected here; this crate never
//! touches the keychain itself.

use async_trait::async_trait;
use aws_sdk_bedrockruntime::types::{
    CachePointBlock, CachePointType, ContentBlock, ContentBlockDelta, ContentBlockStart,
    ConversationRole, ConverseStreamOutput, DocumentBlock, DocumentFormat, DocumentSource,
    ImageBlock, ImageFormat, ImageSource, InferenceConfiguration, Message,
    ReasoningContentBlockDelta, StopReason, SystemContentBlock, Tool, ToolConfiguration,
    ToolInputSchema, ToolResultBlock, ToolResultContentBlock, ToolSpecification, ToolUseBlock,
};
use aws_sdk_bedrockruntime::Client;
use aws_smithy_types::{Blob, Document, Number};
use ff_core::Attachment;
use futures_util::stream::{self, StreamExt};

use crate::{
    attachment_bytes, ChatMessage, ChatRequest, Chunk, ChunkStream, LlmError, Provider,
    ReasoningEffort, ToolCallDelta,
};

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
    /// Whether the connection's model accepts image/document attachments
    /// (#332/#334). When false, attachments are stripped before the Converse
    /// request is built so a non-vision model never receives image/document
    /// blocks it would reject. Defaults false; set via [`BedrockProvider::with_vision`].
    supports_vision: bool,
    /// Whether the connection's model accepts *document* attachments (#504).
    /// Independent of [`supports_vision`]: a text-only Claude reads PDFs but has
    /// no vision. When false, documents are stripped before the Converse request
    /// is built. Defaults false; set via [`BedrockProvider::with_documents`].
    supports_documents: bool,
    /// Reasoning depth dial (#394). On a thinking turn, drives the Converse
    /// `reasoning_config.budget_tokens` (Claude extended thinking) and the
    /// matching `maxTokens`. Defaults to [`ReasoningEffort::Medium`].
    reasoning_effort: ReasoningEffort,
}

impl BedrockProvider {
    pub fn new(region: impl Into<String>, creds: BedrockCreds) -> Self {
        Self {
            region: region.into(),
            creds,
            supports_vision: false,
            supports_documents: false,
            reasoning_effort: ReasoningEffort::default(),
        }
    }

    /// Declare whether the target model can accept image attachments.
    pub fn with_vision(mut self, supports_vision: bool) -> Self {
        self.supports_vision = supports_vision;
        self
    }

    /// Declare whether the target model can accept document attachments (#504).
    pub fn with_documents(mut self, supports_documents: bool) -> Self {
        self.supports_documents = supports_documents;
        self
    }

    /// Set the reasoning depth dial (#394). Applies only on thinking turns, where
    /// it sets Claude extended-thinking `budget_tokens` and a matching `maxTokens`.
    /// Defaults to [`ReasoningEffort::Medium`].
    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = effort;
        self
    }

    /// The configured reasoning-effort dial (#394). Assertable without a live
    /// Bedrock call, so callers (e.g. the CLI's `build_provider`) can prove the
    /// dial reaches the provider without emitting a Converse request.
    pub fn reasoning_effort(&self) -> ReasoningEffort {
        self.reasoning_effort
    }

    /// Thinking config that `chat_stream` emits on the Converse wire request.
    /// Factored out (#395 acceptance) so the provider's private
    /// `reasoning_effort` dial is assertable without a live Bedrock call.
    pub fn thinking_config_for(&self, model: &str) -> (Document, Option<i32>) {
        thinking_request_config(model, self.reasoning_effort)
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

/// Headroom reserved for the answer above the reasoning budget when extended
/// thinking is on, since Converse requires `maxTokens > budget_tokens` (#394).
const BEDROCK_ANSWER_HEADROOM: u32 = 4_096;

/// Output-token ceiling pinned on adaptive-thinking Claude turns (Opus 4.6+,
/// Sonnet 4.6+). Adaptive thinking has no explicit reasoning budget to clear,
/// but extended-thinking tokens still count against the model's output cap.
/// Left on the small per-model default, a hard-thinking turn can exhaust the
/// budget and stop on `MaxTokens` mid `toolUse.input`, truncating the
/// tool-call JSON (which then surfaced as "invalid JSON", #528). Pin a
/// generous ceiling so thinking plus a full tool call fit comfortably.
const ADAPTIVE_THINKING_MAX_TOKENS: i32 = 32_768;

/// Build the `additionalModelRequestFields` document that turns on Claude
/// extended thinking with the given reasoning-token budget (#394). The legacy
/// `reasoning_config.type = "enabled"` interface, used by Opus/Sonnet 4.5 and
/// older; adaptive-era models reject it (see [`adaptive_thinking_doc`]).
fn reasoning_config_doc(budget_tokens: u32) -> Document {
    Document::Object(std::collections::HashMap::from([(
        "reasoning_config".to_string(),
        Document::Object(std::collections::HashMap::from([
            ("type".to_string(), Document::String("enabled".to_string())),
            (
                "budget_tokens".to_string(),
                Document::Number(Number::PosInt(budget_tokens as u64)),
            ),
        ])),
    )]))
}

/// Build the `additionalModelRequestFields` document for adaptive thinking
/// (Opus 4.6+, Sonnet 4.6+). These models deprecated `reasoning_config`/
/// `budget_tokens` and return a `ValidationException` for `type = "enabled"`;
/// they take `thinking.type = "adaptive"` plus an effort label that must live in
/// a separate `output_config` object (effort inside `thinking` is rejected).
/// The caller still pins a generous `ADAPTIVE_THINKING_MAX_TOKENS` ceiling so
/// thinking tokens cannot starve the tool-call output (#528).
fn adaptive_thinking_doc(effort: ReasoningEffort) -> Document {
    Document::Object(std::collections::HashMap::from([
        (
            "thinking".to_string(),
            Document::Object(std::collections::HashMap::from([(
                "type".to_string(),
                Document::String("adaptive".to_string()),
            )])),
        ),
        (
            "output_config".to_string(),
            Document::Object(std::collections::HashMap::from([(
                "effort".to_string(),
                Document::String(effort.effort_str().to_string()),
            )])),
        ),
    ]))
}

/// Extra Converse request fields (and any paired `maxTokens` pin) for a thinking
/// turn. This is the single branch used by the emitted request path.
fn thinking_request_config(model: &str, effort: ReasoningEffort) -> (Document, Option<i32>) {
    if uses_adaptive_thinking(model) {
        (
            adaptive_thinking_doc(effort),
            Some(ADAPTIVE_THINKING_MAX_TOKENS),
        )
    } else {
        let budget = effort.budget_tokens();
        (
            reasoning_config_doc(budget),
            Some((budget + BEDROCK_ANSWER_HEADROOM) as i32),
        )
    }
}

/// Whether `model` is an adaptive-thinking Claude (Opus 4.6+, Sonnet 4.6+, and
/// the named Mythos/Fable lines), which require [`adaptive_thinking_doc`] rather
/// than the legacy [`reasoning_config_doc`]. Version-aware so future minors
/// (e.g. Opus 4.9) stay on the adaptive path; Opus/Sonnet 4.5 and older fall
/// through to the legacy path. Matches Bedrock ids like
/// `us.anthropic.claude-opus-4-8` and Anthropic-native `claude-sonnet-4-6`.
fn uses_adaptive_thinking(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    if m.contains("mythos") || m.contains("fable") {
        return true;
    }
    for family in ["opus", "sonnet"] {
        if let Some(rest) = m.split(&format!("{family}-")).nth(1) {
            let mut parts = rest.split(['-', '.', ':']).filter(|s| !s.is_empty());
            if let (Some(major), minor) = (parts.next(), parts.next()) {
                if let Ok(major) = major.parse::<u32>() {
                    // Guard against the older `claude-3-5-sonnet-<date>` naming,
                    // where the family is FOLLOWED by an 8-digit date rather than
                    // a version. Real version majors stay well under 100.
                    if major >= 100 {
                        continue;
                    }
                    let minor = minor.and_then(|x| x.parse::<u32>().ok()).unwrap_or(0);
                    if major > 4 || (major == 4 && minor >= 6) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[async_trait]
impl Provider for BedrockProvider {
    fn supports_vision(&self) -> bool {
        self.supports_vision
    }

    fn supports_documents(&self) -> bool {
        self.supports_documents
    }

    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let wire =
            crate::messages_for_wire(&req.messages, self.supports_vision, self.supports_documents);
        let (mut system, messages) = to_converse(&wire);
        let client = self.client().await;

        // Prompt caching (#437): on models that support it, mark the stable
        // system + tool-schema prefix with a cache point so prefill is near-free
        // from turn 2. Gated to a known-supported allowlist -- an unsupported
        // model 400s on a cachePoint block, and a validation error is not
        // retried, so a false positive would break the turn.
        let cache = model_supports_cache_point(&req.model);
        if cache {
            if let (false, Some(point)) = (system.is_empty(), cache_point()) {
                system.push(SystemContentBlock::CachePoint(point));
            }
        }
        let thinking_config = req.thinking.then(|| self.thinking_config_for(&req.model));

        let mut call = client
            .converse_stream()
            .model_id(req.model)
            .set_messages(Some(messages));
        if !system.is_empty() {
            call = call.set_system(Some(system));
        }
        if let Some(cfg) = to_tool_config(&req.tools, cache) {
            call = call.tool_config(cfg);
        }
        // Claude extended thinking (#394). Two wire shapes by model generation:
        //   - Adaptive (Opus 4.6+, Sonnet 4.6+): thinking.type = "adaptive" +
        //     output_config.effort, plus a generous maxTokens ceiling so thinking
        //     tokens cannot starve the tool-call output (#528). The legacy
        //     "enabled" shape is rejected with a 400.
        //   - Legacy (Opus/Sonnet 4.5 and older): reasoning_config + a maxTokens
        //     pin, since Converse needs maxTokens > budget_tokens.
        // Non-thinking turns are untouched, preserving the model's default maxTokens.
        if let Some((fields, max_tokens)) = thinking_config {
            if let Some(max_tokens) = max_tokens {
                call = call.inference_config(
                    InferenceConfiguration::builder()
                        .max_tokens(max_tokens)
                        .build(),
                );
            }
            call = call.additional_model_request_fields(fields);
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
            max_tokens: None,
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
        ConverseStreamOutput::MessageStop(ev) => Some(Chunk {
            done: true,
            truncated: matches!(ev.stop_reason(), StopReason::MaxTokens),
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
            _ => (ConversationRole::User, user_blocks(msg)),
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

    (system, enforce_tool_result_pairing(out))
}

/// Bedrock rejects any assistant `tool_use` block whose id lacks a `tool_result`
/// in the immediately-following message. A turn whose future was dropped (window
/// closed, command aborted, a new turn started over an in-flight one) can persist
/// an assistant `tool_use` with no result, since the agent's `[cancelled]` backfill
/// only runs on cooperative cancel -- not on async drop. That malformed history is
/// replayed verbatim on the next turn and 400s the provider. Repair it here so an
/// already-broken session can still be sent: inject a synthetic `tool_result` for
/// every dangling `tool_use` id.
fn enforce_tool_result_pairing(messages: Vec<Message>) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len() + 1);
    let mut iter = messages.into_iter().peekable();

    while let Some(msg) = iter.next() {
        let tool_use_ids = collect_tool_use_ids(&msg);
        out.push(msg);
        if tool_use_ids.is_empty() {
            continue;
        }

        // Ids the following message already answers (only a user message can).
        let covered = match iter.peek() {
            Some(next) if next.role == ConversationRole::User => collect_tool_result_ids(next),
            _ => Vec::new(),
        };
        let synth: Vec<ContentBlock> = tool_use_ids
            .into_iter()
            .filter(|id| !covered.contains(id))
            .map(synthetic_tool_result)
            .collect();
        if synth.is_empty() {
            continue;
        }

        match iter.peek_mut() {
            // Prepend so tool results lead the following user turn.
            Some(next) if next.role == ConversationRole::User => {
                let mut content = synth;
                content.append(&mut next.content);
                next.content = content;
            }
            // Trailing assistant, or a non-user message follows: insert a fresh user
            // message carrying the synthetic results.
            _ => out.push(
                Message::builder()
                    .role(ConversationRole::User)
                    .set_content(Some(synth))
                    .build()
                    .expect("role is always set"),
            ),
        }
    }

    out
}

fn collect_tool_use_ids(msg: &Message) -> Vec<String> {
    msg.content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse(u) => Some(u.tool_use_id.clone()),
            _ => None,
        })
        .collect()
}

fn collect_tool_result_ids(msg: &Message) -> Vec<String> {
    msg.content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolResult(r) => Some(r.tool_use_id.clone()),
            _ => None,
        })
        .collect()
}

fn synthetic_tool_result(id: String) -> ContentBlock {
    ContentBlock::ToolResult(
        ToolResultBlock::builder()
            .tool_use_id(id)
            .content(ToolResultContentBlock::Text(
                "[no result recorded]".to_string(),
            ))
            .build()
            .expect("tool_use_id is always set"),
    )
}

fn text_blocks(msg: &ChatMessage) -> Vec<ContentBlock> {
    match &msg.content {
        Some(text) if !text.is_empty() => vec![ContentBlock::Text(text.clone())],
        _ => vec![],
    }
}

/// Blocks for a user turn: the text block (if any) followed by a native
/// image/document block per attachment (#332, #335). Each attachment block is built
/// independently so one bad attachment is skipped without losing the rest of the
/// turn; an attachment-only message therefore still yields a non-empty Vec and is
/// not dropped by `to_converse`s `is_empty` guard. Attachments are a user-message
/// concept (and Bedrock only accepts image/document blocks on user turns), so this
/// lives apart from `text_blocks` -- the assistant path stays image-free.
fn user_blocks(msg: &ChatMessage) -> Vec<ContentBlock> {
    let mut blocks = text_blocks(msg);
    let mut doc_names: Vec<String> = Vec::new();
    blocks.extend(
        msg.attachments
            .iter()
            .filter_map(|a| attachment_block(a, &mut doc_names)),
    );
    blocks
}

/// Map an IANA media type to a Bedrock [`ImageFormat`]. Bedrock accepts only this
/// fixed allowlist, so an unrecognized type yields `None` and the caller skips the
/// attachment rather than emitting a block Bedrock would reject (trust-boundary:
/// `supportsVision` does not guarantee every format is accepted, #334).
fn image_format(media_type: &str) -> Option<ImageFormat> {
    match media_type.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some(ImageFormat::Png),
        "image/jpeg" | "image/jpg" => Some(ImageFormat::Jpeg),
        "image/gif" => Some(ImageFormat::Gif),
        "image/webp" => Some(ImageFormat::Webp),
        _ => None,
    }
}

/// Map an IANA media type (falling back to the file-name extension) to a Bedrock
/// [`DocumentFormat`]. Same fixed-allowlist discipline as [`image_format`].
fn document_format(media_type: &str, name: Option<&str>) -> Option<DocumentFormat> {
    let by_media = match media_type.trim().to_ascii_lowercase().as_str() {
        "text/csv" => Some(DocumentFormat::Csv),
        "application/msword" => Some(DocumentFormat::Doc),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            Some(DocumentFormat::Docx)
        }
        "text/html" => Some(DocumentFormat::Html),
        "text/markdown" => Some(DocumentFormat::Md),
        "application/pdf" => Some(DocumentFormat::Pdf),
        "text/plain" => Some(DocumentFormat::Txt),
        "application/vnd.ms-excel" => Some(DocumentFormat::Xls),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
            Some(DocumentFormat::Xlsx)
        }
        // Bedrock's DocumentFormat has no JSON variant; route it to Txt so JSON
        // attachments still reach the model as readable text (#504).
        "application/json" | "text/json" => Some(DocumentFormat::Txt),
        _ => None,
    };
    by_media.or_else(|| {
        let ext = name?.rsplit_once('.')?.1.to_ascii_lowercase();
        match ext.as_str() {
            "csv" => Some(DocumentFormat::Csv),
            "doc" => Some(DocumentFormat::Doc),
            "docx" => Some(DocumentFormat::Docx),
            "html" | "htm" => Some(DocumentFormat::Html),
            "md" | "markdown" => Some(DocumentFormat::Md),
            "pdf" => Some(DocumentFormat::Pdf),
            "txt" => Some(DocumentFormat::Txt),
            "xls" => Some(DocumentFormat::Xls),
            "xlsx" => Some(DocumentFormat::Xlsx),
            "json" => Some(DocumentFormat::Txt),
            _ => None,
        }
    })
}

/// Bedrock requires a document `name` drawn from a restricted charset (alphanumerics,
/// whitespace, hyphens, parentheses, square brackets). Sanitize the original name,
/// collapsing every other character to a space, and fall back to "document" when
/// nothing usable remains. Uniqueness within a message is enforced by the caller.
fn sanitize_document_name(name: Option<&str>) -> String {
    let cleaned: String = name
        .unwrap_or("")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '(' | ')' | '[' | ']') {
                c
            } else {
                ' '
            }
        })
        .collect();
    let trimmed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.is_empty() {
        "document".to_string()
    } else {
        trimmed
    }
}

/// Build a Converse content block for one attachment, or `None` (with a warning) when
/// it cannot be sent: an unsupported media type, an unreadable file, or undecodable
/// inline data. Skipping rather than failing keeps one bad file from dropping the
/// whole turn. `doc_names` tracks names already used in this message so duplicates get
/// a `-2`, `-3`, ... suffix (Bedrock requires distinct document names).
fn attachment_block(a: &Attachment, doc_names: &mut Vec<String>) -> Option<ContentBlock> {
    match a.kind {
        ff_core::AttachmentKind::Image => {
            let Some(format) = image_format(&a.media_type) else {
                tracing::warn!(media_type = %a.media_type, "skipping image attachment: unsupported media type for Bedrock");
                return None;
            };
            let bytes = attachment_bytes(a)
                .map_err(|e| tracing::warn!(error = %e, "skipping image attachment"))
                .ok()?;
            let block = ImageBlock::builder()
                .format(format)
                .source(ImageSource::Bytes(Blob::new(bytes)))
                .build()
                .ok()?;
            Some(ContentBlock::Image(block))
        }
        ff_core::AttachmentKind::Document => {
            let Some(format) = document_format(&a.media_type, a.name.as_deref()) else {
                tracing::warn!(media_type = %a.media_type, "skipping document attachment: unsupported media type for Bedrock");
                return None;
            };
            let bytes = attachment_bytes(a)
                .map_err(|e| tracing::warn!(error = %e, "skipping document attachment"))
                .ok()?;
            let name = unique_document_name(sanitize_document_name(a.name.as_deref()), doc_names);
            let block = DocumentBlock::builder()
                .format(format)
                .name(name)
                .source(DocumentSource::Bytes(Blob::new(bytes)))
                .build()
                .ok()?;
            Some(ContentBlock::Document(block))
        }
    }
}

/// Return a name not already present in `used`, appending `-2`, `-3`, ... on collision,
/// and record the chosen name. Bedrock rejects duplicate document names in a message.
fn unique_document_name(base: String, used: &mut Vec<String>) -> String {
    let mut candidate = base.clone();
    let mut n = 2;
    while used.contains(&candidate) {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    used.push(candidate.clone());
    candidate
}

fn assistant_blocks(msg: &ChatMessage) -> Vec<ContentBlock> {
    let mut blocks = text_blocks(msg);
    if let Some(calls) = &msg.tool_calls {
        for call in calls {
            // Bedrock rejects a null/empty `toolUse.input` ("input is empty"). A no-arg
            // call streams no argument fragments, so its persisted `arguments` is "" and
            // fails to parse -- default to an empty object, matching the always-object
            // tool schema (the Bedrock/Anthropic convention for a no-arg call is `{}`).
            let input = serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                .ok()
                .filter(serde_json::Value::is_object)
                .map(|v| json_to_doc(&v))
                .unwrap_or_else(|| json_to_doc(&serde_json::json!({})));
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
fn to_tool_config(tools: &[serde_json::Value], cache: bool) -> Option<ToolConfiguration> {
    let mut specs: Vec<Tool> = tools.iter().filter_map(to_tool).collect();
    if specs.is_empty() {
        return None;
    }
    // Prompt caching (#437): a trailing cache point caches the tool-schema block,
    // the largest stable prefix segment. Only on models known to support it.
    if cache {
        if let Some(point) = cache_point() {
            specs.push(Tool::CachePoint(point));
        }
    }
    ToolConfiguration::builder()
        .set_tools(Some(specs))
        .build()
        .ok()
}

/// A `default`-type cache point block, or `None` if the SDK rejects the build
/// (treated as best-effort: a missing cache point just forgoes the speedup).
fn cache_point() -> Option<CachePointBlock> {
    CachePointBlock::builder()
        .r#type(CachePointType::Default)
        .build()
        .ok()
}

/// Whether `model` is known to support Converse prompt caching (`cachePoint`).
/// Conservative allowlist: an unsupported model 400s on a cache point and the
/// error is not retried, so this biases hard toward off. A false negative just
/// forgoes the speedup. Excludes the legacy date-suffixed 3.x ids (3.5 Sonnet
/// v1, Opus 3, Haiku 3) that predate caching.
fn model_supports_cache_point(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    // Amazon Nova (text sizes) support prompt caching. New text sizes must be
    // added here explicitly -- multimodal Nova variants (canvas/reel) are out.
    if m.contains("nova-micro") || m.contains("nova-lite") || m.contains("nova-pro") {
        return true;
    }
    // Claude families with documented cachePoint support, matched by id substring.
    const SUPPORTED: &[&str] = &[
        "claude-3-7-sonnet",
        "claude-3-5-haiku",
        "claude-3-5-sonnet-20241022", // v2 only -- v1 (20240620) does not support it
    ];
    if SUPPORTED.iter().any(|s| m.contains(s)) {
        return true;
    }
    // Every Claude 4+ family supports cachePoint. `uses_adaptive_thinking` only
    // covers the opus/sonnet adaptive lines (and mythos/fable), so cache support
    // for any 4+ family -- including a future haiku-5+ -- is matched separately by
    // major version rather than the old `*-4` substrings (which missed 5+).
    uses_adaptive_thinking(&m) || claude_major_ge_4(&m)
}

/// True when `model` names a Claude opus/sonnet/haiku at major version >= 4.
/// Matches by version rather than literal `*-4` so future majors (haiku-5, ...)
/// are covered symmetrically. The `< 100` ceiling rejects the legacy
/// `claude-3-5-sonnet-<8-digit-date>` ids, whose date parses as a huge "major".
fn claude_major_ge_4(m: &str) -> bool {
    for family in ["opus", "sonnet", "haiku"] {
        if let Some(rest) = m.split(&format!("{family}-")).nth(1) {
            if let Some(major) = rest.split(['-', '.', ':']).next() {
                if let Ok(major) = major.parse::<u32>() {
                    if (4..100).contains(&major) {
                        return true;
                    }
                }
            }
        }
    }
    false
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
    use base64::Engine as _;
    use ff_core::AttachmentSource;

    /// Pull a `u64` out of a `Document` at `obj.reasoning_config.budget_tokens`.
    fn budget_of(doc: &Document) -> u64 {
        let Document::Object(top) = doc else {
            panic!("expected object")
        };
        let Document::Object(rc) = &top["reasoning_config"] else {
            panic!("expected reasoning_config object")
        };
        assert_eq!(rc["type"], Document::String("enabled".to_string()));
        match rc["budget_tokens"] {
            Document::Number(Number::PosInt(v)) => v,
            _ => panic!("budget_tokens not a positive int"),
        }
    }

    #[test]
    fn reasoning_config_doc_enables_thinking_with_budget() {
        // Each effort level maps to its uniform concrete budget for Converse.
        let low = ReasoningEffort::Low.budget_tokens();
        let med = ReasoningEffort::Medium.budget_tokens();
        let high = ReasoningEffort::High.budget_tokens();
        assert_eq!(budget_of(&reasoning_config_doc(low)), 1024);
        assert_eq!(budget_of(&reasoning_config_doc(med)), 4096);
        assert_eq!(budget_of(&reasoning_config_doc(high)), 8192);
        // maxTokens (budget + headroom) stays above every budget, so Converse
        // accepts the request.
        for b in [low, med, high] {
            assert!(b + BEDROCK_ANSWER_HEADROOM > b);
        }
    }

    #[test]
    fn adaptive_thinking_doc_emits_type_and_effort() {
        // effort lives in a SEPARATE output_config object, not inside thinking
        // (Bedrock rejects effort nested under thinking).
        for (effort, label) in [
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::Medium, "medium"),
            (ReasoningEffort::High, "high"),
        ] {
            let Document::Object(top) = adaptive_thinking_doc(effort) else {
                panic!("expected object")
            };
            let Document::Object(thinking) = &top["thinking"] else {
                panic!("expected thinking object")
            };
            assert_eq!(thinking["type"], Document::String("adaptive".to_string()));
            assert!(
                !thinking.contains_key("effort"),
                "effort must not nest inside thinking"
            );
            let Document::Object(oc) = &top["output_config"] else {
                panic!("expected output_config object")
            };
            assert_eq!(oc["effort"], Document::String(label.to_string()));
            // No legacy budget interface leaks into the adaptive shape.
            assert!(!top.contains_key("reasoning_config"));
        }
    }

    #[test]
    fn thinking_request_config_emits_legacy_budget_or_adaptive_effort() {
        let (legacy, legacy_max) =
            thinking_request_config("anthropic.claude-sonnet-4-5", ReasoningEffort::High);
        assert_eq!(budget_of(&legacy), 8192);
        assert_eq!(
            legacy_max,
            Some((8192 + BEDROCK_ANSWER_HEADROOM) as i32),
            "legacy Converse thinking pins maxTokens above the budget"
        );

        let (adaptive, adaptive_max) =
            thinking_request_config("us.anthropic.claude-opus-4-8", ReasoningEffort::High);
        assert_eq!(
            adaptive_max,
            Some(ADAPTIVE_THINKING_MAX_TOKENS),
            "adaptive thinking pins a generous maxTokens so thinking cannot starve tool output (#528)"
        );
        let Document::Object(top) = adaptive else {
            panic!("expected object")
        };
        assert!(
            !top.contains_key("reasoning_config"),
            "adaptive models must not emit deprecated budget_tokens"
        );
        let Document::Object(output_config) = &top["output_config"] else {
            panic!("expected output_config object")
        };
        assert_eq!(
            output_config["effort"],
            Document::String(ReasoningEffort::High.effort_str().to_string())
        );
    }

    /// #395 acceptance: the provider's private `reasoning_effort` dial (set via
    /// `with_reasoning_effort`) must reach the thinking config that `chat_stream`
    /// emits on the wire — not just `thinking_request_config`'s return value
    /// when the effort is passed directly.  High → legacy budget 8192 /
    /// adaptive `output_config.effort = "high"`.
    #[test]
    fn high_effort_provider_emits_legacy_budget_or_adaptive_effort() {
        let provider = BedrockProvider::new(
            "us-east-2",
            BedrockCreds::ApiKey {
                token: "secret".into(),
            },
        )
        .with_reasoning_effort(ReasoningEffort::High);

        // Legacy model — budget_tokens path.
        let (legacy, legacy_max) = provider.thinking_config_for("anthropic.claude-sonnet-4-5");
        assert_eq!(budget_of(&legacy), 8192);
        assert_eq!(
            legacy_max,
            Some((8192 + BEDROCK_ANSWER_HEADROOM) as i32),
            "legacy Converse thinking pins maxTokens above the budget"
        );

        // Adaptive model — output_config.effort path.
        let (adaptive, adaptive_max) = provider.thinking_config_for("us.anthropic.claude-opus-4-8");
        assert_eq!(
            adaptive_max,
            Some(ADAPTIVE_THINKING_MAX_TOKENS),
            "adaptive thinking pins a generous maxTokens so thinking cannot starve tool output (#528)"
        );
        let Document::Object(top) = adaptive else {
            panic!("expected object")
        };
        assert!(
            !top.contains_key("reasoning_config"),
            "adaptive models must not emit deprecated budget_tokens"
        );
        let Document::Object(output_config) = &top["output_config"] else {
            panic!("expected output_config object")
        };
        assert_eq!(
            output_config["effort"],
            Document::String(ReasoningEffort::High.effort_str().to_string())
        );
    }

    #[test]
    fn uses_adaptive_thinking_splits_by_model_generation() {
        // Adaptive era: Opus/Sonnet 4.6+, future minors, and the named lines.
        for m in [
            "us.anthropic.claude-opus-4-8",
            "us.anthropic.claude-opus-4-6",
            "us.anthropic.claude-opus-4-7",
            "claude-sonnet-4-6",
            "us.anthropic.claude-opus-4-9",
            "us.anthropic.claude-opus-5-0",
            "claude-mythos-5",
            "claude-fable-5",
        ] {
            assert!(uses_adaptive_thinking(m), "expected adaptive: {m}");
        }
        // Legacy era: Opus/Sonnet 4.5 and older, and non-Claude.
        for m in [
            "us.anthropic.claude-opus-4-5",
            "us.anthropic.claude-sonnet-4-5",
            "anthropic.claude-3-5-sonnet-20241022-v2:0",
            "us.anthropic.claude-3-opus-20240229-v1:0",
            "meta.llama3-70b",
        ] {
            assert!(!uses_adaptive_thinking(m), "expected legacy: {m}");
        }
    }

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

            attachments: Vec::new(),
            reasoning: None,
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
    fn user_summary_stays_in_messages_not_hoisted() {
        // role=user keeps the compaction summary in its chronological slot;
        // only system-role messages are hoisted into the top-level system param.
        let (system, messages) = to_converse(&[
            ChatMessage::text("system", "be brief"),
            ChatMessage::text("user", "Summary of 40 earlier messages"),
            ChatMessage::text("assistant", "recent verbatim reply"),
        ]);
        assert_eq!(system.len(), 1);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, ConversationRole::User);
        assert_eq!(messages[1].role, ConversationRole::Assistant);
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

            attachments: Vec::new(),
            reasoning: None,
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

    fn tool_use_input(args: &str) -> serde_json::Value {
        let (_, messages) = to_converse(&[assistant_with_call(args)]);
        match &messages[0].content[0] {
            ContentBlock::ToolUse(b) => doc_to_json(&b.input),
            _ => panic!("expected tool-use block"),
        }
    }

    #[test]
    fn empty_args_tool_use_becomes_empty_object() {
        // A no-arg call streams no argument fragments -> persisted as "". Must serialize
        // as an empty object, not null (Bedrock: "toolUse.input is empty").
        assert_eq!(tool_use_input(""), serde_json::json!({}));
    }

    #[test]
    fn invalid_args_tool_use_becomes_empty_object() {
        assert_eq!(tool_use_input("not json"), serde_json::json!({}));
    }

    #[test]
    fn null_args_tool_use_becomes_empty_object() {
        assert_eq!(tool_use_input("null"), serde_json::json!({}));
    }

    #[test]
    fn object_args_are_preserved() {
        assert_eq!(
            tool_use_input(r#"{"command":"ls"}"#),
            serde_json::json!({"command": "ls"})
        );
    }

    fn assistant_with_calls(ids: &[&str]) -> ChatMessage {
        ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(
                ids.iter()
                    .map(|id| ToolCall {
                        id: (*id).into(),
                        kind: "function".into(),
                        function: FunctionCall {
                            name: "bash".into(),
                            arguments: r#"{"command":"ls"}"#.into(),
                        },
                    })
                    .collect(),
            ),
            tool_call_id: None,
            name: None,

            attachments: Vec::new(),
            reasoning: None,
        }
    }

    fn tool_result(id: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".into(),
            content: Some("ok".into()),
            tool_calls: None,
            tool_call_id: Some(id.into()),
            name: Some("bash".into()),

            attachments: Vec::new(),
            reasoning: None,
        }
    }

    fn result_ids(msg: &Message) -> Vec<&str> {
        msg.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult(r) => Some(r.tool_use_id.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn dangling_tool_use_gets_synthetic_result() {
        // assistant(tool_use) immediately followed by a plain user prompt: the
        // result was never persisted, so a synthetic one must lead that user turn.
        let (_, messages) = to_converse(&[
            assistant_with_calls(&["call_1"]),
            ChatMessage::text("user", "what next?"),
        ]);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].role, ConversationRole::User);
        assert_eq!(result_ids(&messages[1]), vec!["call_1"]);
        // The original user text is preserved after the injected result.
        assert!(matches!(messages[1].content[1], ContentBlock::Text(_)));
    }

    #[test]
    fn trailing_tool_use_gets_synthetic_result() {
        // History ends on an assistant tool_use (turn future dropped before the
        // result was recorded): append a fresh user message carrying the result.
        let (_, messages) = to_converse(&[assistant_with_calls(&["call_1"])]);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].role, ConversationRole::User);
        assert_eq!(result_ids(&messages[1]), vec!["call_1"]);
    }

    #[test]
    fn partial_tool_results_backfilled() {
        // Two parallel calls, only the first result persisted: the second is
        // backfilled into the same following user message.
        let (_, messages) = to_converse(&[
            assistant_with_calls(&["call_1", "call_2"]),
            tool_result("call_1"),
        ]);
        assert_eq!(messages.len(), 2);
        let mut ids = result_ids(&messages[1]);
        ids.sort_unstable();
        assert_eq!(ids, vec!["call_1", "call_2"]);
    }

    #[test]
    fn well_formed_history_is_unchanged() {
        let (_, messages) =
            to_converse(&[assistant_with_calls(&["call_1"]), tool_result("call_1")]);
        assert_eq!(messages.len(), 2);
        assert_eq!(result_ids(&messages[1]), vec!["call_1"]);
        assert_eq!(messages[1].content.len(), 1);
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
        assert!(!chunk.truncated, "EndTurn is a clean stop, not truncation");
    }

    #[test]
    fn message_stop_max_tokens_marks_truncated() {
        let event = ConverseStreamOutput::MessageStop(
            aws_sdk_bedrockruntime::types::MessageStopEvent::builder()
                .stop_reason(StopReason::MaxTokens)
                .build()
                .unwrap(),
        );
        let chunk = event_to_chunk(event).unwrap();
        assert!(chunk.done);
        assert!(
            chunk.truncated,
            "MaxTokens means the output cap cut the turn off mid-stream (#528)"
        );
    }

    #[test]
    fn adaptive_thinking_pins_a_generous_max_tokens() {
        let (_, max_tokens) =
            thinking_request_config("us.anthropic.claude-opus-4-8", ReasoningEffort::High);
        assert_eq!(
            max_tokens,
            Some(ADAPTIVE_THINKING_MAX_TOKENS),
            "adaptive thinking must pin maxTokens so thinking cannot starve tool-call output (#528)"
        );
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
        let cfg = to_tool_config(&[spec], false).unwrap();
        assert_eq!(cfg.tools.len(), 1);
        match &cfg.tools[0] {
            Tool::ToolSpec(s) => assert_eq!(s.name, "bash"),
            _ => panic!("expected tool spec"),
        }
    }

    #[test]
    fn no_tools_yields_no_config() {
        assert!(to_tool_config(&[], false).is_none());
    }

    #[test]
    fn cache_point_appended_to_tools_when_enabled() {
        let spec = serde_json::json!({
            "type": "function",
            "function": { "name": "bash", "parameters": { "type": "object", "properties": {} } }
        });
        // Disabled: tools are byte-identical to today (one ToolSpec, no cache point).
        let off = to_tool_config(std::slice::from_ref(&spec), false).unwrap();
        assert_eq!(off.tools.len(), 1);
        assert!(matches!(off.tools[0], Tool::ToolSpec(_)));
        // Enabled: a trailing CachePoint follows the tool spec.
        let on = to_tool_config(&[spec], true).unwrap();
        assert_eq!(on.tools.len(), 2);
        assert!(matches!(on.tools[0], Tool::ToolSpec(_)));
        assert!(matches!(on.tools[1], Tool::CachePoint(_)));
    }

    #[test]
    fn model_cache_support_allowlist() {
        // Supported: Nova, Claude 3.7 / 3.5 Haiku / 3.5 Sonnet v2, and 4+.
        for m in [
            "amazon.nova-pro-v1:0",
            "anthropic.claude-3-7-sonnet-20250219-v1:0",
            "anthropic.claude-3-5-haiku-20241022-v1:0",
            "anthropic.claude-3-5-sonnet-20241022-v2:0",
            "anthropic.claude-opus-4-20250514-v1:0",
            "us.anthropic.claude-opus-4-6",
            "anthropic.claude-sonnet-4-20250514-v1:0",
            // Future majors must be covered symmetrically across families.
            "anthropic.claude-haiku-5-20260101-v1:0",
            "us.anthropic.claude-opus-5-20260101-v1:0",
        ] {
            assert!(model_supports_cache_point(m), "expected supported: {m}");
        }
        // Unsupported: legacy date-suffixed 3.x and unknown models -- must NOT
        // emit a cache point (would 400 / break the turn).
        for m in [
            "anthropic.claude-3-5-sonnet-20240620-v1:0",
            "anthropic.claude-3-opus-20240229-v1:0",
            "anthropic.claude-3-haiku-20240307-v1:0",
            "meta.llama3-70b",
            "deepseek-v4-pro",
        ] {
            assert!(!model_supports_cache_point(m), "expected unsupported: {m}");
        }
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
        let cfg = to_tool_config(&[spec], false).unwrap();
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

    fn inline_b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn image_msg(media_type: &str, source: AttachmentSource) -> ChatMessage {
        ChatMessage::multimodal(
            "user",
            "look at this",
            vec![Attachment {
                kind: ff_core::AttachmentKind::Image,
                media_type: media_type.into(),
                source,
                name: Some("shot.png".into()),
                bytes: 4,
            }],
        )
    }

    #[test]
    fn multimodal_user_message_carries_image_block() {
        let msg = image_msg(
            "image/png",
            AttachmentSource::Inline(inline_b64(&[0x89, 0x50, 0x4e, 0x47])),
        );
        let (_, messages) = to_converse(&[msg]);
        assert_eq!(messages.len(), 1);
        let content = &messages[0].content;
        assert_eq!(content.len(), 2, "text block then image block");
        assert!(matches!(content[0], ContentBlock::Text(_)));
        match &content[1] {
            ContentBlock::Image(img) => assert_eq!(img.format, ImageFormat::Png),
            other => panic!("expected image block, got {other:?}"),
        }
    }

    #[test]
    fn document_attachment_maps_to_document_block_with_sanitized_name() {
        let msg = ChatMessage::multimodal(
            "user",
            "summarize",
            vec![Attachment {
                kind: ff_core::AttachmentKind::Document,
                media_type: "application/pdf".into(),
                source: AttachmentSource::Inline(inline_b64(b"%PDF-1.4")),
                name: Some("Q3 report (final)/v2.pdf".into()),
                bytes: 8,
            }],
        );
        let (_, messages) = to_converse(&[msg]);
        match &messages[0].content[1] {
            ContentBlock::Document(doc) => {
                assert_eq!(doc.format, DocumentFormat::Pdf);
                assert_eq!(doc.name, "Q3 report (final) v2 pdf");
            }
            other => panic!("expected document block, got {other:?}"),
        }
    }

    #[test]
    fn multiple_unnamed_documents_get_unique_names() {
        let doc = |src: &str| Attachment {
            kind: ff_core::AttachmentKind::Document,
            media_type: "application/pdf".into(),
            source: AttachmentSource::Inline(inline_b64(src.as_bytes())),
            name: None,
            bytes: src.len() as u64,
        };
        let msg = ChatMessage::multimodal("user", "", vec![doc("a"), doc("b")]);
        let (_, messages) = to_converse(&[msg]);
        let names: Vec<&str> = messages[0]
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Document(d) => Some(d.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["document", "document-2"]);
    }

    #[test]
    fn image_only_message_is_not_dropped() {
        let msg = ChatMessage::multimodal(
            "user",
            "",
            vec![Attachment {
                kind: ff_core::AttachmentKind::Image,
                media_type: "image/jpeg".into(),
                source: AttachmentSource::Inline(inline_b64(&[0xff, 0xd8, 0xff])),
                name: None,
                bytes: 3,
            }],
        );
        let (_, messages) = to_converse(&[msg]);
        assert_eq!(messages.len(), 1, "image-only turn must not be dropped");
        assert_eq!(messages[0].content.len(), 1);
        assert!(matches!(messages[0].content[0], ContentBlock::Image(_)));
    }

    #[test]
    fn unsupported_media_type_is_skipped() {
        let msg = image_msg(
            "image/svg+xml",
            AttachmentSource::Inline(inline_b64(b"<svg/>")),
        );
        let (_, messages) = to_converse(&[msg]);
        assert_eq!(messages.len(), 1, "turn still sent");
        assert_eq!(messages[0].content.len(), 1, "only the text block remains");
        assert!(matches!(messages[0].content[0], ContentBlock::Text(_)));
    }

    #[test]
    fn unreadable_path_attachment_is_skipped() {
        let msg = image_msg(
            "image/png",
            AttachmentSource::Path("/nonexistent/flowforge/does-not-exist.png".into()),
        );
        let (_, messages) = to_converse(&[msg]);
        assert_eq!(messages[0].content.len(), 1, "unreadable file dropped");
        assert!(matches!(messages[0].content[0], ContentBlock::Text(_)));
    }

    #[test]
    fn undecodable_inline_base64_is_skipped() {
        let msg = image_msg("image/png", AttachmentSource::Inline("not!base64!".into()));
        let (_, messages) = to_converse(&[msg]);
        assert_eq!(messages[0].content.len(), 1, "bad base64 dropped");
    }

    #[test]
    fn document_format_falls_back_to_extension() {
        let msg = ChatMessage::multimodal(
            "user",
            "",
            vec![Attachment {
                kind: ff_core::AttachmentKind::Document,
                media_type: "application/octet-stream".into(),
                source: AttachmentSource::Inline(inline_b64(b"col1,col2")),
                name: Some("data.csv".into()),
                bytes: 9,
            }],
        );
        let (_, messages) = to_converse(&[msg]);
        match &messages[0].content[0] {
            ContentBlock::Document(d) => assert_eq!(d.format, DocumentFormat::Csv),
            other => panic!("expected document block, got {other:?}"),
        }
    }

    #[test]
    fn json_document_maps_to_txt() {
        // Bedrock has no JSON DocumentFormat; both the media type and the .json
        // extension route to Txt so JSON still reaches the model (#504).
        for (media, name) in [
            ("application/json", "config.json"),
            ("application/octet-stream", "config.json"),
        ] {
            let msg = ChatMessage::multimodal(
                "user",
                "",
                vec![Attachment {
                    kind: ff_core::AttachmentKind::Document,
                    media_type: media.into(),
                    source: AttachmentSource::Inline(inline_b64(b"{\"k\":1}")),
                    name: Some(name.into()),
                    bytes: 7,
                }],
            );
            let (_, messages) = to_converse(&[msg]);
            match &messages[0].content[0] {
                ContentBlock::Document(d) => assert_eq!(d.format, DocumentFormat::Txt),
                other => panic!("expected document block, got {other:?}"),
            }
        }
    }

    #[test]
    fn text_only_message_is_unchanged() {
        let (_, messages) = to_converse(&[ChatMessage::text("user", "plain turn")]);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.len(), 1);
        match &messages[0].content[0] {
            ContentBlock::Text(t) => assert_eq!(t, "plain turn"),
            other => panic!("expected text block, got {other:?}"),
        }
    }

    #[test]
    fn assistant_message_with_attachments_emits_no_image_block() {
        // Attachments are a user-turn concept and Bedrock rejects image/document
        // blocks on an assistant message. Guard that the assistant path stays
        // image-free even if a message is constructed with attachments.
        let msg = ChatMessage::multimodal(
            "assistant",
            "here you go",
            vec![Attachment {
                kind: ff_core::AttachmentKind::Image,
                media_type: "image/png".into(),
                source: AttachmentSource::Inline(inline_b64(&[0x89, 0x50, 0x4e, 0x47])),
                name: None,
                bytes: 4,
            }],
        );
        let (_, messages) = to_converse(&[msg]);
        assert_eq!(messages.len(), 1);
        assert!(
            messages[0]
                .content
                .iter()
                .all(|b| !matches!(b, ContentBlock::Image(_) | ContentBlock::Document(_))),
            "assistant turn must not carry image/document blocks"
        );
    }

    // The capability strip (#338): `chat_stream` routes `req.messages` through
    // `messages_for_wire(_, supports_vision)` before `to_converse`, so a non-vision
    // Bedrock model never receives image/document blocks it would reject. These
    // exercise that exact composition.
    #[test]
    fn vision_off_strips_attachments_before_converse() {
        let msg = image_msg(
            "image/png",
            AttachmentSource::Inline(inline_b64(&[0x89, 0x50, 0x4e, 0x47])),
        );
        let wire = crate::messages_for_wire(std::slice::from_ref(&msg), false, false);
        let (_, messages) = to_converse(&wire);
        assert_eq!(messages.len(), 1);
        assert!(
            messages[0]
                .content
                .iter()
                .all(|b| !matches!(b, ContentBlock::Image(_) | ContentBlock::Document(_))),
            "vision off: no image/document block reaches Converse"
        );
    }

    #[test]
    fn vision_on_keeps_image_block() {
        let msg = image_msg(
            "image/png",
            AttachmentSource::Inline(inline_b64(&[0x89, 0x50, 0x4e, 0x47])),
        );
        let wire = crate::messages_for_wire(std::slice::from_ref(&msg), true, true);
        let (_, messages) = to_converse(&wire);
        assert!(
            messages[0]
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::Image(_))),
            "vision on: the image block is emitted"
        );
    }
}
