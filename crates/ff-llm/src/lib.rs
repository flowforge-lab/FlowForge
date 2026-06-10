//! LLM provider abstraction. M1 ships two providers behind a single trait:
//! [`OpenAiProvider`] (OpenAI-compatible SSE — candle-vllm, vLLM, LM Studio, OpenAI)
//! and [`OllamaProvider`] (Ollama-native NDJSON `/api/chat`). Bedrock and Anthropic
//! land in later milestones behind the same trait.

mod ollama;
mod openai;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};

pub use ollama::OllamaProvider;
pub use openai::OpenAiProvider;

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

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    /// OpenAI `tools` entries. Empty = a plain chat turn.
    pub tools: Vec<serde_json::Value>,
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
    pub tool_calls: Vec<ToolCallDelta>,
    pub done: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("decode error: {0}")]
    Decode(String),
}

pub type ChunkStream = BoxStream<'static, Result<Chunk, LlmError>>;

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError>;
}
