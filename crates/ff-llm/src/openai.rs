use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;

use crate::{ChatRequest, Chunk, ChunkStream, LlmError, Provider, ToolCallDelta};

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
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Vec<StreamToolCall>,
}

#[derive(Deserialize, Default)]
struct StreamToolCall {
    #[serde(default)]
    index: u32,
    id: Option<String>,
    #[serde(default)]
    function: StreamFunction,
}

#[derive(Deserialize, Default)]
struct StreamFunction {
    name: Option<String>,
    #[serde(default)]
    arguments: String,
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
            done: true,
            ..Chunk::default()
        }));
    }

    match serde_json::from_str::<StreamChunk>(payload) {
        Ok(parsed) => {
            let chunk = match parsed.choices.into_iter().next() {
                Some(c) => Chunk {
                    delta: c.delta.content.unwrap_or_default(),
                    reasoning_delta: c
                        .delta
                        .reasoning_content
                        .or(c.delta.reasoning)
                        .unwrap_or_default(),
                    tool_calls: c
                        .delta
                        .tool_calls
                        .into_iter()
                        .map(|tc| ToolCallDelta {
                            index: tc.index,
                            id: tc.id,
                            name: tc.function.name,
                            arguments: tc.function.arguments,
                        })
                        .collect(),
                    done: c.finish_reason.is_some(),
                },
                None => Chunk::default(),
            };
            Some(Ok(chunk))
        }
        Err(e) => Some(Err(LlmError::Decode(e.to_string()))),
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let mut body = serde_json::json!({
            "model": req.model,
            "messages": req.messages,
            "stream": true,
        });
        if !req.tools.is_empty() {
            body["tools"] = serde_json::Value::Array(req.tools.clone());
            body["tool_choice"] = serde_json::json!("auto");
        }

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
            .map_err(|e| LlmError::Api {
                status: e.status().map(|s| s.as_u16()).unwrap_or(0),
                message: e.to_string(),
            })?;

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

    /// `GET {base_url}/models` -> `{ "data": [ { "id": ... } ] }`.
    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        let mut builder = self.client.get(format!("{}/models", self.base_url));
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?
            .error_for_status()
            .map_err(|e| LlmError::Api {
                status: e.status().map(|s| s.as_u16()).unwrap_or(0),
                message: e.to_string(),
            })?;
        let list: ModelList = resp
            .json()
            .await
            .map_err(|e| LlmError::Decode(e.to_string()))?;
        Ok(list.data.into_iter().map(|m| m.id).collect())
    }

    /// Probe the endpoint by listing models. A hosted, key-gated server (e.g.
    /// SiliconFlow) returns 401 on a bad key, surfacing as an [`LlmError::Api`]; a
    /// local server returns its catalog. Zero token cost, and the lightest call
    /// that exercises both the URL and the credentials -- so the settings "Test
    /// Connection" button means something for every OpenAI-compatible backend.
    ///
    /// Unlike `BedrockProvider::test_connection`, which fires a chat round-trip
    /// because a Bedrock token may be allowed to converse yet lack
    /// `bedrock:ListInferenceProfiles`, an OpenAI-compatible key gates `/models`
    /// and `/chat/completions` identically -- so `list_models` is a valid
    /// auth + reachability probe with no token cost, and no chat probe is needed.
    async fn test_connection(&self, _model: &str) -> Result<(), LlmError> {
        self.list_models().await.map(|_| ())
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

    #[test]
    fn parses_tool_call_delta() {
        let line = br#"data: {"choices":[{"delta":{"content":null,"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"bash","arguments":"{\"command\":\"ls\"}"}}]},"finish_reason":null}]}"#;
        let chunk = parse_sse_line(line).unwrap().unwrap();
        assert_eq!(chunk.tool_calls.len(), 1);
        let tc = &chunk.tool_calls[0];
        assert_eq!(tc.index, 0);
        assert_eq!(tc.id.as_deref(), Some("call_1"));
        assert_eq!(tc.name.as_deref(), Some("bash"));
        assert!(tc.arguments.contains("\"command\""));
    }

    #[test]
    fn tool_calls_finish_reason_marks_done() {
        let line =
            br#"data: {"choices":[{"delta":{"content":null},"finish_reason":"tool_calls"}]}"#;
        let chunk = parse_sse_line(line).unwrap().unwrap();
        assert!(chunk.done);
    }

    /// Guard: the OpenAI wire protocol requires `tool_calls[].function.arguments`
    /// to be a JSON-encoded **string**. The Ollama-native provider converts this
    /// to an object at its own boundary; this provider must NOT -- an object here
    /// is a 400 (`cannot unmarshal object into ... of type string`). This test
    /// fails loudly if anyone "unifies" the two and breaks OpenAI-compatible
    /// servers (OpenAI, candle-vLLM, LM Studio, Ollama's /v1 shim).
    #[test]
    fn tool_call_arguments_serialize_as_a_string() {
        use crate::{ChatMessage, FunctionCall, ToolCall};
        let msg = ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_0".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "write_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
            }]),
            tool_call_id: None,
            name: None,

            attachments: Vec::new(),
        };
        let v = serde_json::to_value([msg]).unwrap();
        let args = &v[0]["tool_calls"][0]["function"]["arguments"];
        assert!(
            args.is_string(),
            "OpenAI arguments must stay a string, got {args}"
        );
    }

    // ---- #311 PR-3a: list_models / test_connection probe (HTTP-level) ----

    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn models_body(ids: &[&str]) -> serde_json::Value {
        serde_json::json!({ "data": ids.iter().map(|id| serde_json::json!({ "id": id })).collect::<Vec<_>>() })
    }

    #[tokio::test]
    async fn test_connection_ok_on_reachable_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(models_body(&["gpt-4o"])))
            .mount(&server)
            .await;
        let provider = OpenAiProvider::new(server.uri(), Some("sk-test".into()));
        assert!(provider.test_connection("gpt-4o").await.is_ok());
    }

    #[tokio::test]
    async fn test_connection_surfaces_401_on_bad_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let provider = OpenAiProvider::new(server.uri(), Some("sk-bad".into()));
        match provider.test_connection("gpt-4o").await {
            Err(LlmError::Api { status, .. }) => assert_eq!(status, 401),
            other => panic!("expected Api 401, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_connection_surfaces_5xx() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let provider = OpenAiProvider::new(server.uri(), Some("sk-test".into()));
        match provider.test_connection("gpt-4o").await {
            Err(LlmError::Api { status, .. }) => assert_eq!(status, 500),
            other => panic!("expected Api 500, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_connection_ok_on_empty_catalog() {
        // Reachable + authenticated, but the server lists no models. A bare 200 is
        // still a successful probe -- the credential and URL are valid.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(models_body(&[])))
            .mount(&server)
            .await;
        let provider = OpenAiProvider::new(server.uri(), Some("sk-test".into()));
        assert!(provider.test_connection("gpt-4o").await.is_ok());
    }

    #[tokio::test]
    async fn list_models_parses_ids() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(models_body(&["a", "b"])))
            .mount(&server)
            .await;
        let provider = OpenAiProvider::new(server.uri(), None);
        assert_eq!(provider.list_models().await.unwrap(), vec!["a", "b"]);
    }

    #[tokio::test]
    async fn list_models_sends_bearer_when_key_present() {
        // The mock only matches when the Authorization header carries the key, so a
        // pass proves the bearer is plumbed onto the request.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("authorization", "Bearer sk-plumbed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(models_body(&["gpt-4o"])))
            .mount(&server)
            .await;
        let provider = OpenAiProvider::new(server.uri(), Some("sk-plumbed".into()));
        assert!(provider.test_connection("gpt-4o").await.is_ok());
    }
}
