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
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};

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

    /// The cached Bedrock runtime client, building it on first use (#691). The
    /// client is cached **process-wide** keyed by `(region, creds-fingerprint)`,
    /// mirroring how the reqwest providers reuse [`crate::build_streaming_http_client`]
    /// across turns. This matters because the desktop host rebuilds the provider
    /// every turn (a config switch takes effect next message), so a per-instance
    /// cache would die each turn and re-pay the ~950ms `aws_config::...load()`
    /// plus a cold TLS handshake. Keying by region + a *fingerprint* (never the
    /// raw secret) of the creds means a changed connection gets a fresh client
    /// while an unchanged one reuses the warm one across iterations AND turns.
    /// The AWS SDK client is `Arc`-backed, so the returned clone is cheap and
    /// shares the connection pool.
    async fn client(&self) -> Client {
        static CACHE: OnceLock<Mutex<HashMap<u64, Client>>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let key = self.client_cache_key();
        if let Some(client) = cache.lock().unwrap().get(&key).cloned() {
            return client;
        }
        // Build outside the lock: `build_client().await` runs `config.load()`,
        // which must not hold the mutex across an await. A benign race where two
        // first callers both build is harmless — both clients are valid; the map
        // simply keeps the last one and the other drops.
        let client = self.build_client().await;
        cache.lock().unwrap().insert(key, client.clone());
        client
    }

    /// Stable cache key for a client: the region plus a fingerprint of the
    /// credential source. `Profile` keys on the profile *name* (no secret, and
    /// stable across credential rotations — the cached client's own SDK provider
    /// keeps refreshing vended creds). `IamKeys`/`ApiKey` hash their secret so a
    /// user editing the key yields a new key and a fresh client, without ever
    /// retaining the raw secret in a process-global map.
    fn client_cache_key(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.region.hash(&mut h);
        match &self.creds {
            BedrockCreds::Profile { name } => {
                0u8.hash(&mut h);
                name.hash(&mut h);
            }
            BedrockCreds::IamKeys {
                access_key_id,
                secret_access_key,
                session_token,
            } => {
                1u8.hash(&mut h);
                access_key_id.hash(&mut h);
                secret_access_key.hash(&mut h);
                session_token.hash(&mut h);
            }
            BedrockCreds::ApiKey { token } => {
                2u8.hash(&mut h);
                token.hash(&mut h);
            }
        }
        h.finish()
    }

    /// Build a Bedrock client for the configured region and credential mode.
    /// A rustls-ring HTTP client is wired explicitly so we never pull aws-lc-rs.
    async fn build_client(&self) -> Client {
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

    /// The cached Bedrock control-plane client, building it on first use (#691).
    /// Same process-wide, fingerprint-keyed cache rationale as [`Self::client`];
    /// used only by `list_models`.
    async fn control_client(&self) -> aws_sdk_bedrock::Client {
        static CACHE: OnceLock<Mutex<HashMap<u64, aws_sdk_bedrock::Client>>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let key = self.client_cache_key();
        if let Some(client) = cache.lock().unwrap().get(&key).cloned() {
            return client;
        }
        let client = self.build_control_client().await;
        cache.lock().unwrap().insert(key, client.clone());
        client
    }

    /// Build a Bedrock *control-plane* client, used only by `list_models`
    /// (ListInferenceProfiles). Mirrors [`Self::build_client`] per credential
    /// mode; the control-plane SDK crate has its own config `Builder` type, so
    /// the match cannot be shared generically with the runtime client.
    async fn build_control_client(&self) -> aws_sdk_bedrock::Client {
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
        let (mut system, mut messages) = to_converse(&wire);
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
        // Message-level cache breakpoints (#763): mark the penultimate message and
        // (when long enough) index 0 so the conversation prefix is cached across
        // turns. Uses at most 2 of the remaining cache-point budget.
        if cache && req.cache_messages && messages.len() >= 2 {
            if let Some(point) = cache_point() {
                let pen = messages.len() - 2;
                messages[pen].content.push(ContentBlock::CachePoint(point));
            }
            if messages.len() >= 4 {
                if let Some(point) = cache_point() {
                    messages[0].content.push(ContentBlock::CachePoint(point));
                }
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
            cache_messages: false,
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
            .iter()
            .filter(|id| !covered.contains(id))
            .cloned()
            .map(synthetic_tool_result)
            .collect();

        match iter.peek_mut() {
            Some(next) if next.role == ConversationRole::User => {
                // Strip orphaned toolResult blocks whose IDs don't belong to this
                // assistant's toolUses (#744). A cancel+resend race can interleave
                // results from a parallel loop into the wrong user turn.
                next.content.retain(|b| match b {
                    ContentBlock::ToolResult(r) => tool_use_ids.contains(&r.tool_use_id),
                    _ => true,
                });
                if !synth.is_empty() {
                    let mut content = synth;
                    content.append(&mut next.content);
                    next.content = content;
                }
            }
            _ => {
                if !synth.is_empty() {
                    out.push(
                        Message::builder()
                            .role(ConversationRole::User)
                            .set_content(Some(synth))
                            .build()
                            .expect("role is always set"),
                    );
                }
            }
        }
    }

    // Final sweep: strip orphaned toolResult blocks from user turns whose
    // preceding assistant has no toolUse blocks at all (e.g. a text-only
    // interrupted assistant followed by a displaced result).
    strip_orphaned_trailing_results(&mut out);
    out
}

/// Remove toolResult blocks from any user turn whose preceding assistant has no
/// toolUse blocks. Such results are orphans from a parallel-loop race (#744) and
/// would cause a Bedrock 400 ("toolResult count exceeds toolUse count"). Also
/// drops user turns left empty after stripping, and merges any adjacent same-role
/// messages that result from the removal (Converse requires strict alternation).
fn strip_orphaned_trailing_results(messages: &mut Vec<Message>) {
    let mut i = 1;
    while i < messages.len() {
        if messages[i].role == ConversationRole::User {
            let prev_has_uses = i > 0
                && messages[i - 1].role == ConversationRole::Assistant
                && messages[i - 1]
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolUse(_)));
            if !prev_has_uses {
                messages[i]
                    .content
                    .retain(|b| !matches!(b, ContentBlock::ToolResult(_)));
                if messages[i].content.is_empty() {
                    messages.remove(i);
                    // Removing a user turn may leave two adjacent assistants;
                    // merge the second into the first to restore alternation.
                    if i < messages.len() && i > 0 && messages[i].role == messages[i - 1].role {
                        let absorbed = messages.remove(i);
                        messages[i - 1].content.extend(absorbed.content);
                    }
                    continue;
                }
            }
        }
        i += 1;
    }
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
        // Python source (#842): no py variant either, so route to Txt. A `.ipynb`
        // arrives already converted to `text/plain` from the FE, so it matches the
        // `text/plain` arm above — no notebook branch needed here.
        "text/x-python" | "application/x-python-code" => Some(DocumentFormat::Txt),
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
            // Python source (#842) → Txt. `.ipynb` never reaches here raw — the
            // FE converts it to `text/plain` before send.
            "py" => Some(DocumentFormat::Txt),
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
mod tests;
