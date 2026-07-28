use super::*;

#[test]
fn parses_a_plain_content_chunk() {
    let mut idx = 0;
    let line = br#"{"message":{"content":"hello"},"done":false}"#;
    let chunk = parse_ollama_line(line, &mut idx).unwrap().unwrap();
    assert_eq!(chunk.delta, "hello");
    assert!(chunk.tool_calls.is_empty());
    assert!(!chunk.done);
    assert_eq!(idx, 0);
}

#[test]
fn parses_a_reasoning_chunk() {
    let mut idx = 0;
    let line = br#"{"message":{"thinking":"hmm"},"done":false}"#;
    let chunk = parse_ollama_line(line, &mut idx).unwrap().unwrap();
    assert_eq!(chunk.reasoning_delta, "hmm");
}

#[test]
fn parses_a_tool_call_with_synthesized_id_and_json_args() {
    let mut idx = 0;
    let line = br#"{"message":{"content":"","tool_calls":[
        {"function":{"name":"create_file","arguments":{"path":"a.txt","body":"hi"}}}
    ]},"done":false}"#;
    let chunk = parse_ollama_line(line, &mut idx).unwrap().unwrap();
    assert_eq!(chunk.tool_calls.len(), 1);
    let call = &chunk.tool_calls[0];
    assert_eq!(call.index, 0);
    assert_eq!(call.id.as_deref(), Some("call_0"));
    assert_eq!(call.name.as_deref(), Some("create_file"));
    // arguments round-trip back into a JSON object the agent can parse.
    let v: serde_json::Value = serde_json::from_str(&call.arguments).unwrap();
    assert_eq!(v["path"], "a.txt");
    assert_eq!(v["body"], "hi");
    assert_eq!(idx, 1, "tool index advances for the next call");
}

#[test]
fn tool_call_index_is_unique_across_calls_and_chunks() {
    let mut idx = 0;
    let first =
        br#"{"message":{"tool_calls":[{"function":{"name":"a","arguments":{}}}]},"done":false}"#;
    let second =
        br#"{"message":{"tool_calls":[{"function":{"name":"b","arguments":{}}}]},"done":true}"#;
    let c1 = parse_ollama_line(first, &mut idx).unwrap().unwrap();
    let c2 = parse_ollama_line(second, &mut idx).unwrap().unwrap();
    assert_eq!(c1.tool_calls[0].index, 0);
    assert_eq!(c1.tool_calls[0].id.as_deref(), Some("call_0"));
    assert_eq!(c2.tool_calls[0].index, 1);
    assert_eq!(c2.tool_calls[0].id.as_deref(), Some("call_1"));
    assert!(c2.done);
}

#[test]
fn empty_line_yields_no_chunk() {
    let mut idx = 0;
    assert!(parse_ollama_line(b"", &mut idx).is_none());
}

#[test]
fn malformed_json_is_a_decode_error() {
    let mut idx = 0;
    let res = parse_ollama_line(b"{not json", &mut idx).unwrap();
    assert!(matches!(res, Err(LlmError::Decode(_))));
}

use crate::{FunctionCall, ToolCall};

fn assistant_call(arguments: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: "call_0".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "write_file".into(),
                arguments: arguments.into(),
            },
        }]),
        tool_call_id: None,
        name: None,

        attachments: Vec::new(),
        reasoning: None,
    }
}

fn args_value(messages: &serde_json::Value) -> &serde_json::Value {
    &messages[0]["tool_calls"][0]["function"]["arguments"]
}

#[test]
fn outbound_string_arguments_become_an_object() {
    // The exact shape ff-agent echoes back; Ollama 400s on a string.
    let msgs = vec![assistant_call(
        r#"{"path":"~/hello.rs","body":"fn main(){}"}"#,
    )];
    let out = ollama_messages(&msgs).unwrap();
    let args = args_value(&out);
    assert!(
        args.is_object(),
        "arguments must be a JSON object, got {args}"
    );
    assert_eq!(args["path"], "~/hello.rs");
    assert_eq!(args["body"], "fn main(){}");
}

#[test]
fn outbound_empty_or_invalid_arguments_become_empty_object() {
    // Empty/whitespace/malformed, plus valid-but-non-object JSON (array,
    // scalar, string) — Ollama requires an object, so all map to `{}`.
    for raw in ["", "not json", "   ", "[1,2]", "42", r#""hi""#, "null"] {
        let out = ollama_messages(&[assistant_call(raw)]).unwrap();
        let args = args_value(&out);
        assert!(args.is_object(), "{raw:?} should map to an object");
        assert_eq!(
            args.as_object().unwrap().len(),
            0,
            "{raw:?} -> empty object"
        );
    }
}

#[test]
fn outbound_messages_without_tool_calls_are_unchanged() {
    let msgs = vec![ChatMessage::text("user", "hi")];
    let out = ollama_messages(&msgs).unwrap();
    assert_eq!(out, serde_json::to_value(&msgs).unwrap());
    assert!(out[0].get("tool_calls").is_none());
}

#[test]
fn outbound_converts_every_call_in_a_multi_call_message() {
    let msg = ChatMessage {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(vec![
            ToolCall {
                id: "call_0".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "a".into(),
                    arguments: r#"{"x":1}"#.into(),
                },
            },
            ToolCall {
                id: "call_1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "b".into(),
                    arguments: r#"{"y":2}"#.into(),
                },
            },
        ]),
        tool_call_id: None,
        name: None,

        attachments: Vec::new(),
        reasoning: None,
    };
    let out = ollama_messages(&[msg]).unwrap();
    assert_eq!(out[0]["tool_calls"][0]["function"]["arguments"]["x"], 1);
    assert_eq!(out[0]["tool_calls"][1]["function"]["arguments"]["y"], 2);
}

use ff_core::{Attachment, AttachmentKind, AttachmentSource};

fn inline_b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn image(media_type: &str, source: AttachmentSource) -> Attachment {
    Attachment {
        kind: AttachmentKind::Image,
        media_type: media_type.into(),
        source,
        name: Some("shot.png".into()),
        bytes: 4,
    }
}

const PNG: [u8; 4] = [0x89, 0x50, 0x4e, 0x47];

#[test]
fn image_attachment_emits_bare_base64_in_images_array() {
    let b64 = inline_b64(&PNG);
    let msgs = vec![ChatMessage::multimodal(
        "user",
        "look at this",
        vec![image("image/png", AttachmentSource::Inline(b64.clone()))],
    )];
    let out = ollama_messages(&msgs).unwrap();
    let m = &out[0];
    assert!(
        m.get("attachments").is_none(),
        "internal field must not leak"
    );
    assert!(m["content"].is_string(), "content stays a plain string");
    let images = m["images"].as_array().expect("images array present");
    assert_eq!(images.len(), 1);
    let entry = images[0].as_str().unwrap();
    assert_eq!(entry, b64, "bare base64, byte-identical to the payload");
    assert!(
        !entry.starts_with("data:"),
        "Ollama takes bare base64, not a data URI"
    );
}

#[test]
fn document_attachment_is_skipped_no_images() {
    let msgs = vec![ChatMessage::multimodal(
        "user",
        "read this",
        vec![Attachment {
            kind: AttachmentKind::Document,
            media_type: "application/pdf".into(),
            source: AttachmentSource::Inline(inline_b64(b"%PDF-1.4")),
            name: Some("doc.pdf".into()),
            bytes: 8,
        }],
    )];
    let out = ollama_messages(&msgs).unwrap();
    assert!(
        out[0].get("images").is_none(),
        "no image block for a document"
    );
    assert!(out[0].get("attachments").is_none());
}

#[test]
fn unsupported_image_type_is_skipped() {
    let msgs = vec![ChatMessage::multimodal(
        "user",
        "svg here",
        vec![image(
            "image/svg+xml",
            AttachmentSource::Inline(inline_b64(b"<svg/>")),
        )],
    )];
    let out = ollama_messages(&msgs).unwrap();
    assert!(
        out[0].get("images").is_none(),
        "unsupported type produces no images"
    );
}

#[test]
fn text_only_turn_is_byte_identical() {
    let msgs = vec![ChatMessage::text("user", "hi")];
    let out = ollama_messages(&msgs).unwrap();
    assert_eq!(out, serde_json::to_value(&msgs).unwrap());
    assert!(out[0].get("images").is_none());
    assert!(out[0].get("attachments").is_none());
}

/// Folded from the #368 review: one bad attachment must not drop the turn -- the
/// valid image still lands in `images`, the unreadable one is skipped.
#[test]
fn mixed_valid_and_unreadable_keeps_the_good_image() {
    let msgs = vec![ChatMessage::multimodal(
        "user",
        "two images",
        vec![
            image("image/png", AttachmentSource::Inline(inline_b64(&PNG))),
            image(
                "image/png",
                AttachmentSource::Path("/nonexistent/flowforge/missing.png".into()),
            ),
        ],
    )];
    let out = ollama_messages(&msgs).unwrap();
    let images = out[0]["images"].as_array().expect("images present");
    assert_eq!(images.len(), 1, "only the readable image survives");
    assert_eq!(images[0].as_str().unwrap(), inline_b64(&PNG));
}

/// Folded from the #368 review: exercise the strip (messages_for_wire) and reshape
/// (ollama_messages) layers together. Vision off => no images and no leaked field;
/// vision on => images present.
#[test]
fn strip_then_reshape_composition() {
    let msgs = vec![ChatMessage::multimodal(
        "user",
        "look",
        vec![image(
            "image/png",
            AttachmentSource::Inline(inline_b64(&PNG)),
        )],
    )];

    let off = ollama_messages(&crate::messages_for_wire(&msgs, false, false)).unwrap();
    assert!(
        off[0].get("images").is_none(),
        "vision off: image stripped before reshape"
    );
    assert!(
        off[0].get("attachments").is_none(),
        "vision off: no leaked field"
    );

    let on = ollama_messages(&crate::messages_for_wire(&msgs, true, false)).unwrap();
    assert!(
        on[0]["images"].as_array().is_some_and(|a| a.len() == 1),
        "vision on: image emitted"
    );
    assert!(on[0].get("attachments").is_none());
}

fn req(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_string(),
        messages: vec![],
        tools: vec![],
        thinking: false,
        max_tokens: None,
        cache_messages: false,
    }
}

#[test]
fn context_window_reports_configured_num_ctx() {
    // A configured num_ctx is the served window the agent budgets against,
    // not the model's trained maximum (#538).
    let p = OllamaProvider::default().with_num_ctx(Some(131_072));
    assert_eq!(p.context_window("Qwen/Qwen3.6-35B-A3B"), 131_072);
}

#[test]
fn context_window_clamps_num_ctx_to_trained_ceiling() {
    // num_ctx above the model's trained window cannot be served, so the
    // reported window is clamped to the family-lookup ceiling.
    let p = OllamaProvider::default().with_num_ctx(Some(9_999_999));
    assert_eq!(
        p.context_window("Qwen/Qwen3.6-35B-A3B"),
        crate::model_context_window("Qwen/Qwen3.6-35B-A3B"),
    );
}

#[test]
fn context_window_unset_is_conservative() {
    // With no num_ctx the served window is the server's OLLAMA_CONTEXT_LENGTH
    // default, which the agent cannot see; report the conservative default so
    // the budget under-fills rather than overflowing (#538).
    let p = OllamaProvider::default();
    assert_eq!(
        p.context_window("Qwen/Qwen3.6-35B-A3B"),
        crate::DEFAULT_CONTEXT_WINDOW_TOKENS,
    );
}

#[test]
fn context_window_uses_probed_budget_when_no_num_ctx() {
    // With no explicit num_ctx, the probed served window (#612) becomes the
    // budget denominator -- so a user who raised OLLAMA_CONTEXT_LENGTH budgets
    // against the window the runtime actually serves, not the 32k default.
    let p = OllamaProvider::default().with_budget_window(Some(131_072));
    assert_eq!(p.context_window("Qwen/Qwen3.6-35B-A3B"), 131_072);
}

#[test]
fn context_window_explicit_num_ctx_overrides_probed_budget() {
    // An explicit num_ctx is the served window the request pins, so it wins
    // over a probed budget -- and is still clamped to the trained ceiling.
    let trained = crate::model_context_window("Qwen/Qwen3.6-35B-A3B");
    let p = OllamaProvider::default()
        .with_num_ctx(Some(8_192))
        .with_budget_window(Some(131_072));
    assert_eq!(p.context_window("Qwen/Qwen3.6-35B-A3B"), 8_192);
    let clamped = OllamaProvider::default()
        .with_num_ctx(Some(9_999_999))
        .with_budget_window(Some(131_072));
    assert_eq!(clamped.context_window("Qwen/Qwen3.6-35B-A3B"), trained);
}

#[test]
fn context_window_falls_to_default_when_neither_set() {
    // No num_ctx and an unprimed/empty probe (cold start, before /api/ps lists
    // the model) falls to the conservative default -- a safe under-fill.
    let mut p = OllamaProvider::default();
    Provider::set_context_budget(&mut p, None);
    assert_eq!(
        p.context_window("Qwen/Qwen3.6-35B-A3B"),
        crate::DEFAULT_CONTEXT_WINDOW_TOKENS,
    );
}

#[test]
fn set_context_budget_threads_probed_window_into_budget() {
    // The Provider-trait setter the host calls before a turn primes the same
    // budget the builder does, so the wiring in lib.rs reaches context_window.
    let mut p = OllamaProvider::default();
    Provider::set_context_budget(&mut p, Some(262_144));
    assert_eq!(p.context_window("moonshotai/Kimi-K2.7-Code"), 262_144);
}

#[test]
fn resolve_served_window_prefers_explicit_clamped_to_trained() {
    use ff_core::ContextWindowSource;
    // Explicit env wins and is reported as-is when within the trained ceiling.
    assert_eq!(
        resolve_served_window(Some(131_072), Some(8_192), Some(262_144)),
        (131_072, ContextWindowSource::Explicit),
    );
    // Explicit above the trained ceiling clamps (Ollama serves min()).
    assert_eq!(
        resolve_served_window(Some(9_999_999), None, Some(262_144)),
        (262_144, ContextWindowSource::Explicit),
    );
}

#[test]
fn resolve_served_window_uses_ps_value_without_explicit() {
    use ff_core::ContextWindowSource;
    assert_eq!(
        resolve_served_window(None, Some(40_960), Some(262_144)),
        (40_960, ContextWindowSource::Served),
    );
}

#[test]
fn resolve_served_window_falls_back_to_conservative_default() {
    use ff_core::ContextWindowSource;
    assert_eq!(
        resolve_served_window(None, None, Some(262_144)),
        (
            crate::DEFAULT_CONTEXT_WINDOW_TOKENS,
            ContextWindowSource::Default
        ),
    );
}

#[test]
fn ps_list_parses_context_length_present_and_absent() {
    let with: PsList =
        serde_json::from_str(r#"{"models":[{"name":"qwen3.6:35b","context_length":131072}]}"#)
            .unwrap();
    assert_eq!(with.models[0].context_length, Some(131_072));

    // Older Ollama builds omit context_length; the field must stay optional.
    let without: PsList = serde_json::from_str(r#"{"models":[{"name":"qwen3.6:35b"}]}"#).unwrap();
    assert_eq!(without.models[0].context_length, None);
}

#[tokio::test]
async fn chat_stream_sends_num_ctx_when_configured() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{\"message\":{\"content\":\"\"},\"done\":true}\n"),
        )
        .mount(&server)
        .await;

    let provider = OllamaProvider::new(server.uri()).with_num_ctx(Some(131_072));
    let mut stream = provider.chat_stream(req("qwen3.6:35b-a3b")).await.unwrap();
    while stream.next().await.is_some() {}

    let reqs = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(body["options"]["num_ctx"], 131_072);
}

#[tokio::test]
async fn chat_stream_omits_num_ctx_when_unset() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{\"message\":{\"content\":\"\"},\"done\":true}\n"),
        )
        .mount(&server)
        .await;

    let provider = OllamaProvider::new(server.uri());
    let mut stream = provider.chat_stream(req("qwen3.6:35b-a3b")).await.unwrap();
    while stream.next().await.is_some() {}

    let reqs = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert!(
        body.get("options").is_none(),
        "no num_ctx => no options key"
    );
}

#[tokio::test]
async fn chat_stream_sends_keep_alive_when_configured() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{\"message\":{\"content\":\"\"},\"done\":true}\n"),
        )
        .mount(&server)
        .await;

    // Explicit builder value so the assertion is independent of the ambient
    // FLOWFORGE_OLLAMA_KEEP_ALIVE env / process default.
    let provider = OllamaProvider::new(server.uri()).with_keep_alive(Some("30m".to_string()));
    let mut stream = provider.chat_stream(req("qwen3.6:35b-a3b")).await.unwrap();
    while stream.next().await.is_some() {}

    let reqs = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(
        body["keep_alive"], "30m",
        "keep_alive is sent so the model stays warm between turns"
    );
}

#[tokio::test]
async fn chat_stream_omits_keep_alive_when_none() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{\"message\":{\"content\":\"\"},\"done\":true}\n"),
        )
        .mount(&server)
        .await;

    let provider = OllamaProvider::new(server.uri()).with_keep_alive(None);
    let mut stream = provider.chat_stream(req("qwen3.6:35b-a3b")).await.unwrap();
    while stream.next().await.is_some() {}

    let reqs = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert!(
        body.get("keep_alive").is_none(),
        "None => no keep_alive key, deferring to Ollama's own default"
    );
}

// ---- #338 follow-up: text-extraction fallback on the Ollama wire ----

#[tokio::test]
async fn document_text_is_folded_into_content_when_documents_enabled() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{\"message\":{\"content\":\"\"},\"done\":true}\n"),
        )
        .mount(&server)
        .await;

    let doc = Attachment {
        kind: AttachmentKind::Document,
        media_type: "text/plain".into(),
        source: AttachmentSource::Inline(inline_b64(b"the plan")),
        name: Some("plan.txt".into()),
        bytes: 8,
    };
    let req = ChatRequest {
        model: "qwen3.6:35b-a3b".into(),
        messages: vec![ChatMessage::multimodal("user", "read this", vec![doc])],
        tools: Vec::new(),
        thinking: false,
        max_tokens: None,
        cache_messages: false,
    };
    let provider = OllamaProvider::new(server.uri()).with_documents(true);
    let mut stream = provider.chat_stream(req).await.unwrap();
    while stream.next().await.is_some() {}

    let reqs = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let content = body["messages"][0]["content"]
        .as_str()
        .expect("string content");
    assert!(
        content.contains("read this"),
        "user text preserved: {content}"
    );
    assert!(
        content.contains("the plan"),
        "extracted text folded in: {content}"
    );
    assert!(
        body["messages"][0].get("attachments").is_none(),
        "doc attachment must not leak onto the Ollama wire"
    );
    assert!(
        body["messages"][0].get("images").is_none(),
        "no images for a doc-only message"
    );
}

// ---- #625: dynamic Ollama vision-capability detection via /api/show ----

#[test]
fn show_response_parses_capabilities_and_derives_vision() {
    let show: ShowResponse = serde_json::from_str(
        r#"{"model_info":{"qwen35moe.context_length":262144},
            "capabilities":["completion","vision","tools","thinking"]}"#,
    )
    .unwrap();
    assert_eq!(show.trained_window(), Some(262_144));
    assert!(show.supports_vision());

    // Text-only model: no vision tag, and older builds omit capabilities.
    let text: ShowResponse =
        serde_json::from_str(r#"{"model_info":{"llama.context_length":131072}}"#).unwrap();
    assert_eq!(text.trained_window(), Some(131_072));
    assert!(!text.supports_vision());
}

#[test]
fn messages_have_image_only_when_an_image_is_attached() {
    let text = vec![ChatMessage::text("user", "hi")];
    assert!(!messages_have_image(&text));

    let doc = vec![ChatMessage::multimodal(
        "user",
        "read",
        vec![Attachment {
            kind: AttachmentKind::Document,
            media_type: "application/pdf".into(),
            source: AttachmentSource::Inline(inline_b64(b"%PDF")),
            name: None,
            bytes: 4,
        }],
    )];
    assert!(!messages_have_image(&doc));

    let img = vec![ChatMessage::multimodal(
        "user",
        "look",
        vec![image(
            "image/png",
            AttachmentSource::Inline(inline_b64(&PNG)),
        )],
    )];
    assert!(messages_have_image(&img));
}

fn image_req(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        messages: vec![ChatMessage::multimodal(
            "user",
            "look",
            vec![image(
                "image/png",
                AttachmentSource::Inline(inline_b64(&PNG)),
            )],
        )],
        tools: Vec::new(),
        thinking: false,
        max_tokens: None,
        cache_messages: false,
    }
}

async fn mount_show(server: &wiremock::MockServer, capabilities: &str) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "{{\"model_info\":{{\"qwen35moe.context_length\":262144}},\"capabilities\":{capabilities}}}"
        )))
        .mount(server)
        .await;
}

async fn mount_chat(server: &wiremock::MockServer) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{\"message\":{\"content\":\"\"},\"done\":true}\n"),
        )
        .mount(server)
        .await;
}

/// A model built `with_vision(false)` still sends the image when the daemon
/// reports `vision` -- the name-based gate is only a floor (#625).
#[tokio::test]
async fn chat_stream_keeps_image_when_daemon_reports_vision() {
    let server = wiremock::MockServer::start().await;
    mount_show(&server, r#"["completion","vision"]"#).await;
    mount_chat(&server).await;

    let provider = OllamaProvider::new(server.uri()).with_vision(false);
    let mut stream = provider
        .chat_stream(image_req("qwen3.6:35b-a3b"))
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let reqs = server.received_requests().await.unwrap();
    let chat = reqs
        .iter()
        .find(|r| r.url.path() == "/api/chat")
        .expect("a /api/chat request");
    let body: serde_json::Value = serde_json::from_slice(&chat.body).unwrap();
    let images = body["messages"][0]["images"]
        .as_array()
        .expect("image survived the probe-granted vision");
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].as_str().unwrap(), inline_b64(&PNG));
}

/// When the daemon reports no `vision` capability and the name-based gate is
/// off, the image is stripped before the wire (#625, fail-closed preserved).
#[tokio::test]
async fn chat_stream_strips_image_when_daemon_reports_no_vision() {
    let server = wiremock::MockServer::start().await;
    mount_show(&server, r#"["completion","tools"]"#).await;
    mount_chat(&server).await;

    let provider = OllamaProvider::new(server.uri()).with_vision(false);
    let mut stream = provider.chat_stream(image_req("qwen3:4b")).await.unwrap();
    while stream.next().await.is_some() {}

    let reqs = server.received_requests().await.unwrap();
    let chat = reqs
        .iter()
        .find(|r| r.url.path() == "/api/chat")
        .expect("a /api/chat request");
    let body: serde_json::Value = serde_json::from_slice(&chat.body).unwrap();
    assert!(
        body["messages"][0].get("images").is_none(),
        "no-vision model must drop the image"
    );
}

/// A text-only turn never probes `/api/show` -- the probe is gated on an image
/// being present, so the common path adds no latency (#625).
#[tokio::test]
async fn chat_stream_does_not_probe_show_on_text_only_turn() {
    let server = wiremock::MockServer::start().await;
    mount_chat(&server).await; // intentionally no /api/show mock

    let provider = OllamaProvider::new(server.uri()).with_vision(false);
    let mut req = req("qwen3:4b");
    req.messages = vec![ChatMessage::text("user", "hi")];
    let mut stream = provider.chat_stream(req).await.unwrap();
    while stream.next().await.is_some() {}

    let reqs = server.received_requests().await.unwrap();
    assert!(
        reqs.iter().all(|r| r.url.path() != "/api/show"),
        "no capability probe on a text-only turn"
    );
}

/// The probe is also skipped when vision is already granted by name -- no
/// redundant `/api/show` round-trip (#625).
#[tokio::test]
async fn chat_stream_does_not_probe_show_when_vision_already_on() {
    let server = wiremock::MockServer::start().await;
    mount_chat(&server).await; // no /api/show mock

    let provider = OllamaProvider::new(server.uri()).with_vision(true);
    let mut stream = provider.chat_stream(image_req("llava:7b")).await.unwrap();
    while stream.next().await.is_some() {}

    let reqs = server.received_requests().await.unwrap();
    assert!(
        reqs.iter().all(|r| r.url.path() != "/api/show"),
        "name-based vision short-circuits the probe"
    );
}

/// `served_window` folds the daemon's vision capability into the probe so the
/// host can correct the name-based gate for the UI attach gate (#625).
#[tokio::test]
async fn served_window_probe_reports_vision_capability() {
    let server = wiremock::MockServer::start().await;
    mount_show(&server, r#"["completion","vision","tools"]"#).await;

    let probe = OllamaProvider::new(server.uri())
        .served_window("qwen3.6:35b-a3b")
        .await;
    assert_eq!(probe.supports_vision, Some(true));
    assert_eq!(probe.trained, Some(262_144));
}

/// Ollama warmth is residency (`keep_alive`), so warmup is a light reload nudge,
/// not the candle-vLLM 32-step GPU-clock ramp (#61).
#[test]
fn warmup_ramp_is_a_single_residency_touch() {
    assert_eq!(
        OllamaProvider::new("http://localhost:11434").warmup_ramp_steps(),
        1
    );
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
