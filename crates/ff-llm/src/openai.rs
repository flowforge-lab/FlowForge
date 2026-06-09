use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;

use crate::{ChatRequest, Chunk, ChunkStream, LlmError, Provider};

/// Talks to any OpenAI-compatible `/v1/chat/completions` server over Server-Sent
/// Events. candle-vllm, vLLM, LM Studio, Ollama's `/v1` shim, and OpenAI itself all
/// speak this protocol, so switching backends is a `base_url` change with no code edits.
pub struct OpenAiProvider {
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            client: reqwest::Client::new(),
        }
    }

    /// Local candle-vllm server (FlowForge M1 default, no credentials).
    pub fn candle_vllm() -> Self {
        Self::new("http://localhost:8000/v1", None)
    }

    /// Hosted OpenAI API (requires a bearer key).
    pub fn openai(api_key: impl Into<String>) -> Self {
        Self::new("https://api.openai.com/v1", Some(api_key.into()))
    }

    /// Ollama's OpenAI-compatible shim (distinct from its native NDJSON `/api/chat`).
    pub fn ollama_compat() -> Self {
        Self::new("http://localhost:11434/v1", None)
    }
}

impl Default for OpenAiProvider {
    fn default() -> Self {
        Self::candle_vllm()
    }
}

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: Delta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
}

/// Parse one raw SSE line into an optional chunk.
///
/// Returns `None` for lines that carry no payload (blank lines, comments, non-`data:`
/// fields). `data: [DONE]` yields a terminal empty chunk.
fn parse_sse_line(line: &[u8]) -> Option<Result<Chunk, LlmError>> {
    let line = std::str::from_utf8(line).ok()?.trim();
    let payload = line.strip_prefix("data:")?.trim();

    if payload.is_empty() {
        return None;
    }
    if payload == "[DONE]" {
        return Some(Ok(Chunk {
            delta: String::new(),
            done: true,
        }));
    }

    match serde_json::from_str::<StreamChunk>(payload) {
        Ok(parsed) => {
            let choice = parsed.choices.into_iter().next();
            let (delta, done) = match choice {
                Some(c) => (c.delta.content.unwrap_or_default(), c.finish_reason.is_some()),
                None => (String::new(), false),
            };
            Some(Ok(Chunk { delta, done }))
        }
        Err(e) => Some(Err(LlmError::Decode(e.to_string()))),
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let body = serde_json::json!({
            "model": req.model,
            "messages": req.messages,
            "stream": true,
        });

        let mut builder = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&body);
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?
            .error_for_status()
            .map_err(|e| LlmError::Transport(e.to_string()))?;

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
                        if let Some(chunk) = parse_sse_line(line) {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_delta() {
        let line = br#"data: {"choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let chunk = parse_sse_line(line).unwrap().unwrap();
        assert_eq!(chunk.delta, "Hello");
        assert!(!chunk.done);
    }

    #[test]
    fn finish_reason_marks_done() {
        let line = br#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        let chunk = parse_sse_line(line).unwrap().unwrap();
        assert_eq!(chunk.delta, "");
        assert!(chunk.done);
    }

    #[test]
    fn done_sentinel_terminates() {
        let chunk = parse_sse_line(b"data: [DONE]").unwrap().unwrap();
        assert!(chunk.done);
        assert_eq!(chunk.delta, "");
    }

    #[test]
    fn blank_and_non_data_lines_skipped() {
        assert!(parse_sse_line(b"").is_none());
        assert!(parse_sse_line(b": keep-alive comment").is_none());
        assert!(parse_sse_line(b"event: message").is_none());
    }
}
