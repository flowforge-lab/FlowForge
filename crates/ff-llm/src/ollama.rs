use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;

use crate::{ChatRequest, Chunk, ChunkStream, LlmError, Provider};

/// Talks to a local Ollama server (`http://localhost:11434`) over its NDJSON chat stream.
pub struct OllamaProvider {
    base_url: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: reqwest::Client::new(),
        }
    }
}

impl Default for OllamaProvider {
    fn default() -> Self {
        Self::new("http://localhost:11434")
    }
}

#[derive(Deserialize)]
struct OllamaChunk {
    #[serde(default)]
    message: OllamaMessage,
    #[serde(default)]
    done: bool,
}

#[derive(Deserialize, Default)]
struct OllamaMessage {
    #[serde(default)]
    content: String,
}

#[async_trait]
impl Provider for OllamaProvider {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let body = serde_json::json!({
            "model": req.model,
            "messages": req.messages,
            "stream": true,
        });

        let resp = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?
            .error_for_status()
            .map_err(|e| LlmError::Transport(e.to_string()))?;

        // Ollama streams newline-delimited JSON objects. Reassemble lines across
        // byte-chunk boundaries, parsing each complete line into a Chunk.
        let stream = resp.bytes_stream().scan(Vec::<u8>::new(), |buf, item| {
            let out = match item {
                Ok(bytes) => {
                    buf.extend_from_slice(&bytes);
                    let mut chunks = Vec::new();
                    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = buf.drain(..=pos).collect();
                        let line = &line[..line.len().saturating_sub(1)];
                        if line.is_empty() {
                            continue;
                        }
                        match serde_json::from_slice::<OllamaChunk>(line) {
                            Ok(c) => chunks.push(Ok(Chunk {
                                delta: c.message.content,
                                done: c.done,
                            })),
                            Err(e) => chunks.push(Err(LlmError::Decode(e.to_string()))),
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
}
