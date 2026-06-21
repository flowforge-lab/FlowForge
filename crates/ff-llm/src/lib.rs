//! LLM provider abstraction. M1 ships two providers behind a single trait:
//! [`OpenAiProvider`] (OpenAI-compatible SSE — candle-vllm, vLLM, LM Studio, OpenAI)
//! and [`OllamaProvider`] (Ollama-native NDJSON `/api/chat`). [`BedrockProvider`]
//! (AWS Converse) and [`AnthropicProvider`] (native Messages API) land behind the
//! same trait.

mod anthropic;
mod bedrock;
mod ollama;
mod openai;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

pub use anthropic::AnthropicProvider;
pub use bedrock::{BedrockCreds, BedrockProvider};
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
    /// When true, request provider reasoning/thinking streams when supported (#181).
    pub thinking: bool,
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
    pub reasoning_delta: String,
    pub tool_calls: Vec<ToolCallDelta>,
    pub done: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("api error (status {status}): {message}")]
    Api { status: u16, message: String },
    #[error("decode error: {0}")]
    Decode(String),
}

impl LlmError {
    /// Whether the error is worth retrying. Transport blips (connection refused,
    /// timeout, reset) and overloaded/transient HTTP statuses (408, 429, 5xx) are
    /// transient; client errors (other 4xx) and decode failures are fatal and must
    /// surface immediately so the user fixes the request rather than retrying it.
    pub fn is_transient(&self) -> bool {
        match self {
            LlmError::Transport(_) => true,
            LlmError::Api { status, .. } => {
                *status == 408 || *status == 429 || (500..=599).contains(status)
            }
            LlmError::Decode(_) => false,
        }
    }
}

pub type ChunkStream = BoxStream<'static, Result<Chunk, LlmError>>;

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError>;

    /// Best-effort list of model ids the server currently has loaded. Used by the
    /// provider settings panel to populate the model picker; callers treat any
    /// error (server down, endpoint unsupported) as "no suggestions". Providers
    /// without a discovery endpoint keep the default (no suggestions).
    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        Ok(Vec::new())
    }

    /// Best-effort nudge to wake the server before the first real turn. Fires a
    /// tiny request and drains a few decode steps, then drops the stream (which
    /// aborts the request). On a local GPU backend this spins the device up out
    /// of its idle power state and JIT-compiles the decode kernels, so the
    /// user's first message does not pay the cold-start ramp. Callers ignore the
    /// result: warmup must never block a turn or surface an error to the UI.
    ///
    /// The default works for any streaming provider; `model` is the id the
    /// server expects (local backends generally ignore it).
    async fn warmup(&self, model: &str) -> Result<(), LlmError> {
        let req = ChatRequest {
            model: model.to_string(),
            messages: vec![ChatMessage::text("user", "ok")],
            tools: Vec::new(),
            thinking: false,
        };
        let mut stream = self.chat_stream(req).await?;
        // Draining ~32 decode steps is what it empirically takes for an idle
        // Apple-Silicon GPU to reach its full clock; fewer leaves it half-ramped
        // and the next real turn still stalls. Dropping the stream at end of
        // scope aborts the request so the server stops generating early.
        for _ in 0..32u8 {
            match stream.next().await {
                Some(Ok(chunk)) if !chunk.done => continue,
                _ => break,
            }
        }
        Ok(())
    }

    /// Probe that the endpoint is reachable and the active credentials work,
    /// backing the settings "Test Connection" button. The default is a no-op
    /// `Ok` (local backends need no auth); hosted providers override with a real
    /// round-trip. `model` is the connection's configured model, for providers
    /// whose probe needs one (e.g. a Bedrock converse-probe).
    async fn test_connection(&self, _model: &str) -> Result<(), LlmError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct EndlessProvider {
        polled: Arc<AtomicUsize>,
        first_role: Mutex<Option<String>>,
    }

    #[async_trait]
    impl Provider for EndlessProvider {
        async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
            *self.first_role.lock().unwrap() = req.messages.first().map(|m| m.role.clone());
            let polled = self.polled.clone();
            let stream = futures_util::stream::unfold(0usize, move |i| {
                let polled = polled.clone();
                async move {
                    polled.fetch_add(1, Ordering::SeqCst);
                    let chunk = Chunk {
                        delta: "x".into(),
                        reasoning_delta: String::new(),
                        tool_calls: vec![],
                        done: false,
                    };
                    Some((Ok(chunk), i + 1))
                }
            });
            Ok(stream.boxed())
        }
    }

    #[tokio::test]
    async fn test_connection_default_is_ok() {
        let provider = EndlessProvider {
            polled: Arc::new(AtomicUsize::new(0)),
            first_role: Mutex::new(None),
        };
        provider.test_connection("test-model").await.unwrap();
    }

    #[tokio::test]
    async fn warmup_sends_one_user_turn_and_stops_early() {
        let provider = EndlessProvider {
            polled: Arc::new(AtomicUsize::new(0)),
            first_role: Mutex::new(None),
        };
        provider.warmup("test-model").await.unwrap();
        assert_eq!(provider.first_role.lock().unwrap().as_deref(), Some("user"));
        // Bounded: warmup never drains the endless stream.
        assert!(
            provider.polled.load(Ordering::SeqCst) <= 32,
            "warmup drained too many chunks"
        );
    }

    #[test]
    fn transient_errors_are_retryable() {
        assert!(LlmError::Transport("connection refused".into()).is_transient());
        for status in [408u16, 429, 500, 502, 503, 504] {
            assert!(
                LlmError::Api {
                    status,
                    message: "x".into()
                }
                .is_transient(),
                "status {status} should be transient"
            );
        }
    }

    #[test]
    fn client_and_decode_errors_are_fatal() {
        for status in [400u16, 401, 403, 404, 422] {
            assert!(
                !LlmError::Api {
                    status,
                    message: "x".into()
                }
                .is_transient(),
                "status {status} should be fatal"
            );
        }
        assert!(!LlmError::Decode("bad json".into()).is_transient());
    }
}
