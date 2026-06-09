//! The agent turn loop. M1 is a straight chat turn: build history -> stream from the
//! provider -> emit token events -> persist the assistant message. Tool dispatch
//! (M2) and the full research/plan/implement/verify loop layer on top of this.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ff_core::{Message, Role};
use ff_llm::{ChatMessage, ChatRequest, Provider};
use ff_memory::MemoryStore;
use futures_util::StreamExt;

/// Events the agent emits during a turn. The host (Tauri shell or a test) decides
/// how to surface them — over IPC, to a channel, or into assertions.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Token { message_id: String, delta: String },
    Done { message_id: String },
    Error { message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Llm(#[from] ff_llm::LlmError),
}

/// Cooperative cancellation flag, shared between a running turn and `cancel`.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

fn to_chat(messages: &[Message]) -> Vec<ChatMessage> {
    messages
        .iter()
        .map(|m| ChatMessage {
            role: match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
                Role::Tool => "tool",
            }
            .to_string(),
            content: m.content.clone(),
        })
        .collect()
}

/// Runs one assistant turn for `session_id`. `on_event` is called synchronously
/// between streamed chunks. The completed assistant message is persisted to `store`
/// and returned.
pub async fn run_turn(
    provider: &dyn Provider,
    store: &MemoryStore,
    session_id: &str,
    model: &str,
    cancel: CancelToken,
    mut on_event: impl FnMut(AgentEvent),
) -> Result<Message, AgentError> {
    let history = store.get_messages(session_id);
    let req = ChatRequest {
        model: model.to_string(),
        messages: to_chat(&history),
    };

    // Reserve the assistant message id up front so the frontend can route tokens.
    let assistant = store.add_message(session_id, Role::Assistant, String::new());
    let message_id = assistant.id.clone();

    let mut stream = match provider.chat_stream(req).await {
        Ok(s) => s,
        Err(e) => {
            on_event(AgentEvent::Error {
                message: e.to_string(),
            });
            return Err(e.into());
        }
    };

    let mut acc = String::new();
    while let Some(item) = stream.next().await {
        if cancel.is_cancelled() {
            break;
        }
        match item {
            Ok(chunk) => {
                if !chunk.delta.is_empty() {
                    acc.push_str(&chunk.delta);
                    on_event(AgentEvent::Token {
                        message_id: message_id.clone(),
                        delta: chunk.delta,
                    });
                }
                if chunk.done {
                    break;
                }
            }
            Err(e) => {
                on_event(AgentEvent::Error {
                    message: e.to_string(),
                });
                return Err(e.into());
            }
        }
    }

    let final_msg = store.set_message_content(&message_id, session_id, acc);
    on_event(AgentEvent::Done {
        message_id: message_id.clone(),
    });
    Ok(final_msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ff_llm::{ChunkStream, LlmError};

    struct MockProvider;

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let chunks = vec![
                Ok(ff_llm::Chunk {
                    delta: "Hel".into(),
                    done: false,
                }),
                Ok(ff_llm::Chunk {
                    delta: "lo".into(),
                    done: true,
                }),
            ];
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    #[tokio::test]
    async fn streams_and_persists() {
        let store = MemoryStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "hi".into());

        let mut tokens = String::new();
        let mut done = false;
        let msg = run_turn(
            &MockProvider,
            &store,
            &s.id,
            "mock",
            CancelToken::new(),
            |ev| match ev {
                AgentEvent::Token { delta, .. } => tokens.push_str(&delta),
                AgentEvent::Done { .. } => done = true,
                AgentEvent::Error { .. } => panic!("unexpected error"),
            },
        )
        .await
        .unwrap();

        assert_eq!(tokens, "Hello");
        assert!(done);
        assert_eq!(msg.content, "Hello");
        assert_eq!(store.get_messages(&s.id).last().unwrap().content, "Hello");
    }
}
