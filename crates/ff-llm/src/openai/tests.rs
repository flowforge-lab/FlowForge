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
    assert!(
        !chunk.truncated,
        "finish_reason stop is a clean end of turn"
    );
}

#[test]
fn finish_reason_length_marks_truncated() {
    let line = br#"data: {"choices":[{"delta":{},"finish_reason":"length"}]}"#;
    let chunk = parse_sse_line(line).unwrap().unwrap();
    assert!(chunk.done);
    assert!(
        chunk.truncated,
        "finish_reason length means the output cap cut the turn off (#528)"
    );
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

/// #374: SiliconFlow GLM-5.2 streams the name only in the first fragment, then
/// sends `function.name: ""` (an empty string, not null/omitted) on every
/// continuation fragment. This must decode to `Some("")`, not `None`, so the
/// agent accumulator can recognise it as an empty continuation to ignore rather
/// than clobbering the real name. (A vanilla provider omits the field -> `None`.)
#[test]
fn decodes_empty_string_continuation_name_as_some_empty() {
    let line = br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"","arguments":"{\"city\":"}}]},"finish_reason":null}]}"#;
    let chunk = parse_sse_line(line).unwrap().unwrap();
    let tc = &chunk.tool_calls[0];
    assert_eq!(tc.name.as_deref(), Some(""));
    assert_eq!(tc.arguments, "{\"city\":");
}

/// A continuation fragment that omits `name` entirely decodes to `None`, the
/// vanilla-OpenAI shape -- distinct from GLM's empty-string above.
#[test]
fn decodes_omitted_continuation_name_as_none() {
    let line = br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Paris\"}"}}]},"finish_reason":null}]}"#;
    let chunk = parse_sse_line(line).unwrap().unwrap();
    assert_eq!(chunk.tool_calls[0].name, None);
}

/// #493: SiliconFlow `.com` sends `"arguments": null` in the opening tool-call
/// delta fragment. `#[serde(default)]` alone rejects an explicit null
/// ("invalid type: null, expected a string"), so this must decode to `""`
/// rather than a `Decode` error that aborts the whole stream.
#[test]
fn decodes_null_arguments_as_empty_string() {
    let line = br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"bash","arguments":null}}]},"finish_reason":null}]}"#;
    let chunk = parse_sse_line(line).unwrap().unwrap();
    let tc = &chunk.tool_calls[0];
    assert_eq!(tc.name.as_deref(), Some("bash"));
    assert_eq!(tc.arguments, "");
}

#[test]
fn tool_calls_finish_reason_marks_done() {
    let line = br#"data: {"choices":[{"delta":{"content":null},"finish_reason":"tool_calls"}]}"#;
    let chunk = parse_sse_line(line).unwrap().unwrap();
    assert!(chunk.done);
}

// --- prefix cache observability (#766) -----------------------------------

#[test]
fn usage_chunk_parses_openai_cached_tokens() {
    // OpenAI format: prompt_tokens_details.cached_tokens
    let line = br#"data: {"choices":[],"usage":{"prompt_tokens":730,"completion_tokens":20,"total_tokens":750,"prompt_tokens_details":{"cached_tokens":640}}}"#;
    let chunk = parse_sse_line(line).unwrap().unwrap();
    assert_eq!(chunk.cache_hit_tokens, 640);
    assert_eq!(chunk.cache_miss_tokens, 90); // 730 - 640
}

#[test]
fn usage_chunk_parses_siliconflow_legacy_fields() {
    // SiliconFlow legacy: prompt_cache_hit_tokens / prompt_cache_miss_tokens
    let line = br#"data: {"choices":[],"usage":{"prompt_tokens":568,"prompt_cache_hit_tokens":512,"prompt_cache_miss_tokens":56}}"#;
    let chunk = parse_sse_line(line).unwrap().unwrap();
    assert_eq!(chunk.cache_hit_tokens, 512);
    assert_eq!(chunk.cache_miss_tokens, 56);
}

#[test]
fn usage_chunk_zero_when_no_cache() {
    // DeepSeek-style: fields present but 0
    let line = br#"data: {"choices":[],"usage":{"prompt_tokens":563,"prompt_cache_hit_tokens":0,"prompt_cache_miss_tokens":563}}"#;
    let chunk = parse_sse_line(line).unwrap().unwrap();
    assert_eq!(chunk.cache_hit_tokens, 0);
    assert_eq!(chunk.cache_miss_tokens, 563);
}

#[test]
fn no_usage_yields_zero_cache_metrics() {
    // Normal content chunk without usage
    let line = br#"data: {"choices":[{"delta":{"content":"hi"},"finish_reason":null}]}"#;
    let chunk = parse_sse_line(line).unwrap().unwrap();
    assert_eq!(chunk.cache_hit_tokens, 0);
    assert_eq!(chunk.cache_miss_tokens, 0);
}

#[test]
fn trailing_usage_chunk_after_done_is_parseable() {
    // Regression test for #766 blocker: the finish_reason chunk has done=true
    // but cache_*=0; the usage arrives on a SEPARATE trailing chunk with
    // choices:[]. Both must parse correctly so run_turn can drain the trailing.
    let finish_line = br#"data: {"choices":[{"delta":{"content":""},"finish_reason":"stop"}]}"#;
    let usage_line = br#"data: {"choices":[],"usage":{"prompt_tokens":730,"completion_tokens":20,"total_tokens":750,"prompt_tokens_details":{"cached_tokens":640}}}"#;

    let finish_chunk = parse_sse_line(finish_line).unwrap().unwrap();
    assert!(finish_chunk.done);
    assert_eq!(finish_chunk.cache_hit_tokens, 0);

    let usage_chunk = parse_sse_line(usage_line).unwrap().unwrap();
    assert!(!usage_chunk.done); // choices is empty -> no finish_reason
    assert_eq!(usage_chunk.cache_hit_tokens, 640);
    assert_eq!(usage_chunk.cache_miss_tokens, 90); // 730 - 640
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
        reasoning: None,
    };
    let v = serde_json::to_value([msg]).unwrap();
    let args = &v[0]["tool_calls"][0]["function"]["arguments"];
    assert!(
        args.is_string(),
        "OpenAI arguments must stay a string, got {args}"
    );
}

// ---- #336 BE-4: OpenAI image_url data-URI content blocks ----

use ff_core::{Attachment, AttachmentKind, AttachmentSource};

fn inline_b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn image_attachment(media_type: &str, source: AttachmentSource) -> Attachment {
    Attachment {
        kind: AttachmentKind::Image,
        media_type: media_type.into(),
        source,
        name: Some("shot.png".into()),
        bytes: 4,
    }
}

#[test]
fn multimodal_message_emits_image_url_block() {
    let msg = ChatMessage::multimodal(
        "user",
        "look",
        vec![image_attachment(
            "image/png",
            AttachmentSource::Inline(inline_b64(&[0x89, 0x50, 0x4e, 0x47])),
        )],
    );
    let v = message_to_wire(&msg, WireDialect::default());
    let content = v["content"].as_array().expect("content is an array");
    assert_eq!(content.len(), 2, "text block then image block");
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "look");
    assert_eq!(content[1]["type"], "image_url");
    let url = content[1]["image_url"]["url"].as_str().unwrap();
    assert!(
        url.starts_with("data:image/png;base64,"),
        "data URI prefix, got {url}"
    );
    assert!(
        v.get("attachments").is_none(),
        "internal field must not leak"
    );
}

#[test]
fn text_only_message_keeps_plain_string_content() {
    let v = message_to_wire(
        &ChatMessage::text("user", "plain turn"),
        WireDialect::default(),
    );
    assert!(v["content"].is_string(), "content stays a plain string");
    assert_eq!(v["content"], "plain turn");
    assert!(v.get("attachments").is_none());
}

#[test]
fn image_only_message_omits_text_block() {
    let msg = ChatMessage::multimodal(
        "user",
        "",
        vec![image_attachment(
            "image/jpeg",
            AttachmentSource::Inline(inline_b64(&[0xff, 0xd8, 0xff])),
        )],
    );
    let content = message_to_wire(&msg, WireDialect::default())["content"]
        .as_array()
        .expect("content is an array")
        .clone();
    assert_eq!(content.len(), 1, "only the image block");
    assert_eq!(content[0]["type"], "image_url");
}

#[test]
fn path_image_is_read_and_base64_encoded() {
    let dir = std::env::temp_dir();
    let file = dir.join(format!("ff-openai-test-{}.png", std::process::id()));
    let bytes = [0x89u8, 0x50, 0x4e, 0x47, 0x0d, 0x0a];
    std::fs::write(&file, bytes).unwrap();
    let msg = ChatMessage::multimodal(
        "user",
        "",
        vec![image_attachment(
            "image/png",
            AttachmentSource::Path(file.to_string_lossy().into_owned()),
        )],
    );
    let url = message_to_wire(&msg, WireDialect::default())["content"][0]["image_url"]["url"]
        .as_str()
        .unwrap()
        .to_string();
    std::fs::remove_file(&file).ok();
    assert_eq!(url, format!("data:image/png;base64,{}", inline_b64(&bytes)));
}

#[test]
fn document_attachment_is_skipped() {
    let msg = ChatMessage::multimodal(
        "user",
        "summarize",
        vec![Attachment {
            kind: AttachmentKind::Document,
            media_type: "application/pdf".into(),
            source: AttachmentSource::Inline(inline_b64(b"%PDF-1.4")),
            name: Some("doc.pdf".into()),
            bytes: 8,
        }],
    );
    let v = message_to_wire(&msg, WireDialect::default());
    assert!(v["content"].is_string(), "no image block -> plain string");
    assert!(v.get("attachments").is_none());
}

// ---- #338 follow-up: text-extraction fallback for the OpenAI wire ----

/// A text document attachment whose extracted text reaches the wire as part
/// of the user message's `content` when `with_documents(true)` is set. The
/// doc attachment itself is dropped (no `attachments` key leaks); the model
/// sees the user's text plus the extracted document text in one string.
#[tokio::test]
async fn document_text_is_folded_into_content_when_documents_enabled() {
    let server = MockServer::start().await;
    let provider = OpenAiProvider::new(server.uri(), None).with_documents(true);
    let doc = Attachment {
        kind: AttachmentKind::Document,
        media_type: "text/plain".into(),
        source: AttachmentSource::Inline(inline_b64(b" quarterly results ")),
        name: Some("report.txt".into()),
        bytes: 18,
    };
    let req = ChatRequest {
        model: "gpt-4o".into(),
        messages: vec![ChatMessage::multimodal("user", "summarize this", vec![doc])],
        tools: Vec::new(),
        thinking: false,
        max_tokens: None,
        cache_messages: false,
    };
    let body = captured_body(&provider, &server, req).await;
    let content = body["messages"][0]["content"]
        .as_str()
        .expect("string content");
    assert!(
        content.contains("summarize this"),
        "user text preserved: {content}"
    );
    assert!(
        content.contains("quarterly results"),
        "extracted text folded in: {content}"
    );
    assert!(
        content.contains("report.txt"),
        "document named in envelope: {content}"
    );
    assert!(
        body["messages"][0].get("attachments").is_none(),
        "doc attachment must not leak onto the wire"
    );
}

/// With `with_documents` left at its default (false), the capability strip
/// drops the document before it reaches `message_to_wire` — the #338 skip
/// stays the no-extraction default. The wire content is just the user's text.
#[tokio::test]
async fn document_is_stripped_when_documents_not_enabled() {
    let server = MockServer::start().await;
    // Default provider: no with_documents(true).
    let provider = OpenAiProvider::new(server.uri(), None);
    let doc = Attachment {
        kind: AttachmentKind::Document,
        media_type: "text/plain".into(),
        source: AttachmentSource::Inline(inline_b64(b"secret payload")),
        name: Some("report.txt".into()),
        bytes: 14,
    };
    let req = ChatRequest {
        model: "gpt-4o".into(),
        messages: vec![ChatMessage::multimodal("user", "summarize this", vec![doc])],
        tools: Vec::new(),
        thinking: false,
        max_tokens: None,
        cache_messages: false,
    };
    let body = captured_body(&provider, &server, req).await;
    let content = body["messages"][0]["content"]
        .as_str()
        .expect("string content");
    assert_eq!(
        content, "summarize this",
        "doc stripped, only user text remains"
    );
    assert!(
        !content.contains("secret payload"),
        "doc text must not leak"
    );
}

#[test]
fn unsupported_image_type_is_skipped() {
    let msg = ChatMessage::multimodal(
        "user",
        "look",
        vec![image_attachment(
            "image/svg+xml",
            AttachmentSource::Inline(inline_b64(b"<svg/>")),
        )],
    );
    let v = message_to_wire(&msg, WireDialect::default());
    assert!(v["content"].is_string(), "unsupported type skipped");
}

#[test]
fn unreadable_path_is_skipped() {
    let msg = ChatMessage::multimodal(
        "user",
        "look",
        vec![image_attachment(
            "image/png",
            AttachmentSource::Path("/nonexistent/ff/missing.png".into()),
        )],
    );
    let v = message_to_wire(&msg, WireDialect::default());
    assert!(v["content"].is_string(), "unreadable file skipped");
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
async fn list_models_surfaces_error_body_on_4xx() {
    // A provider 400 carries a diagnostic body (e.g. SiliconFlow
    // `{"code":20015,"message":"Field required"}`). The body text must reach
    // `LlmError::Api.message` rather than being discarded, so the user can
    // see *why* the request was rejected.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(serde_json::json!({"code": 20015, "message": "Field required"})),
        )
        .mount(&server)
        .await;
    let provider = OpenAiProvider::new(server.uri(), Some("sk-test".into()));
    match provider.list_models().await {
        Err(LlmError::Api { status, message }) => {
            assert_eq!(status, 400);
            assert!(
                message.contains("Field required") && message.contains("20015"),
                "body not surfaced: {message:?}"
            );
        }
        other => panic!("expected Api 400 with body, got {other:?}"),
    }
}

#[tokio::test]
async fn list_models_falls_back_to_reason_on_empty_body() {
    // No body to surface -- the message must not be empty; fall back to the
    // status's canonical reason so the error still reads sensibly.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let provider = OpenAiProvider::new(server.uri(), Some("sk-test".into()));
    match provider.list_models().await {
        Err(LlmError::Api { status, message }) => {
            assert_eq!(status, 403);
            assert!(!message.is_empty(), "message should fall back to reason");
        }
        other => panic!("expected Api 403, got {other:?}"),
    }
}

#[tokio::test]
async fn list_models_truncates_oversized_error_body() {
    // A pathologically large error body must be bounded so it cannot bloat
    // the surfaced message; truncation keeps the head and flags itself.
    let server = MockServer::start().await;
    let huge = "x".repeat(10_000);
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(400).set_body_string(huge))
        .mount(&server)
        .await;
    let provider = OpenAiProvider::new(server.uri(), Some("sk-test".into()));
    match provider.list_models().await {
        Err(LlmError::Api { status, message }) => {
            assert_eq!(status, 400);
            assert!(
                message.ends_with("...[truncated]"),
                "expected truncation flag"
            );
            assert!(
                message.len() < 2_100,
                "message not bounded: {}",
                message.len()
            );
        }
        other => panic!("expected Api 400, got {other:?}"),
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

// ---- #375 PR-2: per-gateway wire dialect ----

use crate::{FunctionCall, ReasoningEffort, ReasoningWire, ToolCall, ToolCallContent, WireDialect};

fn assistant_tool_call_with_reasoning(reasoning: Option<&str>) -> ChatMessage {
    ChatMessage {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "search".into(),
                arguments: "{}".into(),
            },
        }]),
        tool_call_id: None,
        name: None,
        attachments: Vec::new(),
        reasoning: reasoning.map(str::to_string),
    }
}

#[test]
fn dialect_default_omits_reasoning_and_content() {
    let msg = assistant_tool_call_with_reasoning(Some("hidden CoT"));
    let v = message_to_wire(&msg, WireDialect::default());
    // Vanilla OpenAI must never see the reasoning carrier nor its replay.
    assert!(v.get("reasoning").is_none());
    assert!(v.get("reasoning_content").is_none());
    // Default Omit: no "content" key (serde drops the None).
    assert!(v.get("content").is_none());
}

#[test]
fn dialect_reasoning_content_replays_on_tool_call_turn() {
    let msg = assistant_tool_call_with_reasoning(Some("because A then B"));
    let dialect = WireDialect {
        reasoning: ReasoningWire::ReasoningContent,
        tool_call_content: ToolCallContent::Omit,
        think_tags: false,
    };
    let v = message_to_wire(&msg, dialect);
    assert_eq!(v["reasoning_content"], "because A then B");
    assert!(v.get("reasoning").is_none());
}

#[test]
fn dialect_reasoning_field_replays_for_openrouter() {
    let msg = assistant_tool_call_with_reasoning(Some("step"));
    let dialect = WireDialect {
        reasoning: ReasoningWire::Reasoning,
        tool_call_content: ToolCallContent::Omit,
        think_tags: false,
    };
    let v = message_to_wire(&msg, dialect);
    assert_eq!(v["reasoning"], "step");
    assert!(v.get("reasoning_content").is_none());
}

#[test]
fn dialect_does_not_replay_when_no_reasoning_present() {
    let msg = assistant_tool_call_with_reasoning(None);
    let dialect = WireDialect {
        reasoning: ReasoningWire::ReasoningContent,
        tool_call_content: ToolCallContent::Omit,
        think_tags: false,
    };
    let v = message_to_wire(&msg, dialect);
    assert!(v.get("reasoning_content").is_none());
    assert!(v.get("reasoning").is_none());
}

#[test]
fn dialect_replays_only_on_tool_call_turn_not_on_plain_assistant() {
    // A plain assistant turn (no tool_calls) keeps its content; reasoning
    // is private to the model and never replayed back to it on a normal turn.
    let mut msg = ChatMessage::text("assistant", "the answer is 42");
    msg.reasoning = Some("...".into());
    let dialect = WireDialect {
        reasoning: ReasoningWire::ReasoningContent,
        tool_call_content: ToolCallContent::Omit,
        think_tags: false,
    };
    let v = message_to_wire(&msg, dialect);
    assert!(v.get("reasoning_content").is_none());
    assert_eq!(v["content"], "the answer is 42");
}

#[test]
fn dialect_empty_string_content_for_glm_minimax() {
    let msg = assistant_tool_call_with_reasoning(None);
    let dialect = WireDialect {
        reasoning: ReasoningWire::ReasoningContent,
        tool_call_content: ToolCallContent::EmptyString,
        think_tags: false,
    };
    let v = message_to_wire(&msg, dialect);
    // GLM/MiniMax reject content: null with HTTP 400 code 20015; empty string is accepted.
    assert_eq!(v["content"], "");
}

#[test]
fn dialect_empty_string_only_when_content_actually_missing() {
    // If the tool-call turn has accompanying text (a "thinking out loud" preamble),
    // the EmptyString rule must NOT clobber it.
    let mut msg = assistant_tool_call_with_reasoning(None);
    msg.content = Some("let me search".into());
    let dialect = WireDialect {
        reasoning: ReasoningWire::ReasoningContent,
        tool_call_content: ToolCallContent::EmptyString,
        think_tags: false,
    };
    let v = message_to_wire(&msg, dialect);
    assert_eq!(v["content"], "let me search");
}

// ---- reasoning-cost controls (#394): SiliconFlow thinking_budget /
// enable_thinking emission, gated so other gateways stay clean. ----

fn user_req(thinking: bool) -> ChatRequest {
    ChatRequest {
        model: "zai-org/GLM-5.2".into(),
        messages: vec![ChatMessage::text("user", "hi")],
        tools: Vec::new(),
        thinking,
        max_tokens: None,
        cache_messages: false,
    }
}

/// Capture the JSON body the provider POSTs for one request.
async fn captured_body(
    provider: &OpenAiProvider,
    server: &MockServer,
    req: ChatRequest,
) -> serde_json::Value {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("data: [DONE]\n\n"))
        .mount(server)
        .await;
    let _ = provider.chat_stream(req).await.expect("send succeeds");
    let reqs = server.received_requests().await.expect("requests recorded");
    serde_json::from_slice(&reqs[0].body).expect("body is json")
}

#[tokio::test]
async fn siliconflow_thinking_on_sends_effort_budget_cap() {
    let server = MockServer::start().await;
    let provider = OpenAiProvider::new(server.uri(), None).with_reasoning_control(
        ReasoningControl::SiliconFlow {
            effort: ReasoningEffort::Medium,
        },
    );
    let body = captured_body(&provider, &server, user_req(true)).await;
    assert_eq!(body["thinking_budget"], 4096);
    assert!(
        body.get("enable_thinking").is_none(),
        "budget cap, not the off-switch, when thinking is on"
    );
}

#[tokio::test]
async fn siliconflow_low_effort_caps_tighter_than_medium() {
    let server = MockServer::start().await;
    let provider = OpenAiProvider::new(server.uri(), None).with_reasoning_control(
        ReasoningControl::SiliconFlow {
            effort: ReasoningEffort::Low,
        },
    );
    let body = captured_body(&provider, &server, user_req(true)).await;
    assert_eq!(body["thinking_budget"], 1024);
}

#[tokio::test]
async fn siliconflow_high_effort_caps_at_8192() {
    // High is a hard 8192 cap (not uncapped), keeping the cost guard intact
    // even at the top of the dial.
    let server = MockServer::start().await;
    let provider = OpenAiProvider::new(server.uri(), None).with_reasoning_control(
        ReasoningControl::SiliconFlow {
            effort: ReasoningEffort::High,
        },
    );
    let body = captured_body(&provider, &server, user_req(true)).await;
    assert_eq!(body["thinking_budget"], 8192);
    assert!(body.get("enable_thinking").is_none());
}

#[tokio::test]
async fn siliconflow_thinking_off_disables_reasoning() {
    let server = MockServer::start().await;
    let provider = OpenAiProvider::new(server.uri(), None).with_reasoning_control(
        ReasoningControl::SiliconFlow {
            effort: ReasoningEffort::High,
        },
    );
    let body = captured_body(&provider, &server, user_req(false)).await;
    assert_eq!(body["enable_thinking"], false);
    assert!(
        body.get("thinking_budget").is_none(),
        "off-switch, not a budget, when thinking is off"
    );
}

#[tokio::test]
async fn max_tokens_is_sent_when_set_on_the_request() {
    // The output-cap pin (#550) must reach the wire so the gateway does not
    // apply its small default and truncate a large tool-call payload.
    let server = MockServer::start().await;
    let provider = OpenAiProvider::new(server.uri(), None);
    let req = ChatRequest {
        max_tokens: Some(32_768),
        ..user_req(false)
    };
    let body = captured_body(&provider, &server, req).await;
    assert_eq!(body["max_tokens"], 32_768);
}

#[tokio::test]
async fn max_tokens_is_omitted_when_unset() {
    // No pin -> no field, so the provider/gateway default stands untouched.
    let server = MockServer::start().await;
    let provider = OpenAiProvider::new(server.uri(), None);
    let body = captured_body(&provider, &server, user_req(false)).await;
    assert!(body.get("max_tokens").is_none());
}

#[tokio::test]
async fn default_provider_emits_no_reasoning_params() {
    // Vanilla OpenAI / candle-vllm / LM Studio reject unknown fields, so the
    // default ReasoningControl::None must keep the body clean either way.
    let server = MockServer::start().await;
    let provider = OpenAiProvider::new(server.uri(), None);
    let body = captured_body(&provider, &server, user_req(true)).await;
    assert!(body.get("thinking_budget").is_none());
    assert!(body.get("enable_thinking").is_none());
}

// ---- read_timeout: a mid-stream stall must surface as a transient error,
// not hang forever (SiliconFlow GLM ~31 min stuck). ----

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Guard for a detached test server. Dropping it signals the server to exit
/// its stall and aborts the task as a backstop, releasing the bound listener
/// and any accepted socket (closing their fds). Without this, the spawned
/// task outlives the test body, holding an open socket for the full
/// `sleep(3600)` duration; nextest's process-per-test model then surfaces it
/// as a leak (issue #1072 side-finding).
struct ServerGuard {
    handle: Option<tokio::task::JoinHandle<()>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        // Signal the server to exit its stall so the task ends naturally and
        // drops the held listener + socket. This handles the common case
        // where the server has already accepted and is parked on the stall.
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        // Abort as a backstop for the case where the server hasn't accepted
        // yet (still parked on `accept()`). Dropping the aborted future
        // releases the listener.
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// Accepts one connection, sends HTTP headers + a single SSE `data:` chunk,
/// then holds the socket open without ever sending more bytes or closing --
/// the exact "headers sent, then silence" failure mode. Returns the base URL
/// and a guard whose `Drop` ends the stall and releases the socket.
async fn spawn_stalling_sse_server() -> (String, ServerGuard) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf).await;
            let frame = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n",
                frame.len(),
                frame
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
            // Hold the socket open without the terminating chunk so the client
            // hits its read timeout (the failure mode under test). Bound the
            // stall on a shutdown signal (test done) with a long backstop so a
            // bug that drops the guard wouldn't hang the suite forever.
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(3600)) => {}
                _ = &mut shutdown_rx => {}
            }
        }
    });
    (
        format!("http://{addr}"),
        ServerGuard {
            handle: Some(handle),
            shutdown: Some(shutdown_tx),
        },
    )
}

#[tokio::test]
async fn mid_stream_stall_surfaces_transient_transport_error() {
    let (base, _guard) = spawn_stalling_sse_server().await;
    let mut provider = OpenAiProvider::new(base, None);
    // Production read_timeout is 60s; use a short one so the test is fast.
    // The behavior under test (idle silence -> Transport error) is identical.
    provider.client = reqwest::Client::builder()
        .read_timeout(Duration::from_millis(300))
        .build()
        .unwrap();

    let req = ChatRequest {
        model: "test-model".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: Some("hi".into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning: None,
            attachments: Vec::new(),
        }],
        tools: Vec::new(),
        thinking: false,
        max_tokens: None,
        cache_messages: false,
    };

    let mut stream = provider.chat_stream(req).await.expect("headers arrive");

    // Consume exactly as the agent loop does: take items until the first error,
    // bounding the whole thing on wall-clock so a regression (real hang) fails
    // loudly instead of stalling the suite. A stalled body otherwise blocks
    // forever; with read_timeout it errors within ~300ms.
    let mut saw_content = false;
    let mut stall_err: Option<LlmError> = None;
    loop {
        let next = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .expect("stream must yield or error, never hang");
        match next {
            Some(Ok(chunk)) => {
                if chunk.delta == "hi" {
                    saw_content = true;
                }
            }
            Some(Err(e)) => {
                stall_err = Some(e);
                break;
            }
            None => break,
        }
    }

    assert!(
        saw_content,
        "expected the initial content chunk before the stall"
    );
    match stall_err {
        Some(e @ LlmError::Transport(_)) => assert!(
            e.is_transient(),
            "stall error must be retryable so the agent loop recovers"
        ),
        other => panic!("expected a transient Transport error from the stall, got {other:?}"),
    }
}

/// Accepts the connection, consumes the request, then holds the socket open
/// without ever sending a response -- no status line, no headers. This is the
/// "connected, request sent, server silent before responding" stall, distinct
/// from a mid-body stall: it must trip during chat_stream's `.send()` await,
/// not hang. Returns the base URL and a guard whose `Drop` ends the stall.
async fn spawn_no_response_server() -> (String, ServerGuard) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(3600)) => {}
                _ = &mut shutdown_rx => {}
            }
        }
    });
    (
        format!("http://{addr}"),
        ServerGuard {
            handle: Some(handle),
            shutdown: Some(shutdown_tx),
        },
    )
}

#[tokio::test]
async fn header_wait_stall_surfaces_transient_transport_error() {
    let (base, _guard) = spawn_no_response_server().await;
    let mut provider = OpenAiProvider::new(base, None);
    // Production read_timeout is 60s; a short one keeps the test fast. The
    // behavior under test (no response bytes -> Transport error) is identical.
    provider.client = reqwest::Client::builder()
        .read_timeout(Duration::from_millis(400))
        .build()
        .unwrap();

    let req = ChatRequest {
        model: "test-model".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: Some("hi".into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning: None,
            attachments: Vec::new(),
        }],
        tools: Vec::new(),
        thinking: false,
        max_tokens: None,
        cache_messages: false,
    };

    // The stall is before any response, so it surfaces from chat_stream itself
    // (the `.send()` await), not from polling the stream. This mirrors the
    // continuation-call hang seen in the field (tool ran, next provider call
    // never responded). Bound on wall-clock so a regression fails loudly.
    let result = tokio::time::timeout(Duration::from_secs(10), provider.chat_stream(req))
        .await
        .expect("chat_stream must return or error, never hang");

    match result {
        Err(e @ LlmError::Transport(_)) => assert!(
            e.is_transient(),
            "header-wait stall must be retryable so run_turn retries the call"
        ),
        Err(other) => panic!("expected a transient Transport error, got {other:?}"),
        Ok(_) => panic!("expected an error from a server that never responds"),
    }
}

// --- #1123: assistant-terminated conversation repair ---------------------

#[test]
fn assistant_terminated_history_gets_a_synthetic_user_turn() {
    let messages = vec![
        ChatMessage::text("user", "watch that file"),
        ChatMessage::text("assistant", "observer started"),
    ];
    let fixed = crate::enforce_user_terminated(messages);
    assert_eq!(fixed.len(), 3, "one synthetic turn appended");
    assert_eq!(fixed.last().unwrap().role, "user");
    assert_eq!(fixed.last().unwrap().content.as_deref(), Some("Continue."));
}
