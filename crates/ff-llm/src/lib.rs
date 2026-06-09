//! LLM provider abstraction. M1 ships an Ollama implementation (zero credentials).
//! Bedrock and Anthropic providers land in later milestones behind the same trait.

mod ollama;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};

pub use ollama::OllamaProvider;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
}

/// One streamed increment of an assistant response.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub delta: String,
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
