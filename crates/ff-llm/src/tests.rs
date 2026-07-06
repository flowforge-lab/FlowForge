use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[test]
fn model_context_window_maps_known_families_and_defaults() {
    // Family substrings (case-insensitive), not exact ids, so point releases inherit.
    // Values are the raw served windows probed on 2026-06-24.
    assert_eq!(model_context_window("zai-org/GLM-5.2"), 1_048_576);
    assert_eq!(model_context_window("zai-org/GLM-5"), 202_752);
    assert_eq!(model_context_window("zai-org/GLM-4.5"), 131_072);
    assert_eq!(
        model_context_window("deepseek-ai/DeepSeek-V4-Pro"),
        1_000_000
    );
    assert_eq!(model_context_window("deepseek-ai/DeepSeek-V3.2"), 163_840);
    assert_eq!(model_context_window("moonshotai/Kimi-K2.7-Code"), 262_144);
    assert_eq!(model_context_window("MiniMaxAI/MiniMax-M3"), 700_000);
    assert_eq!(model_context_window("MiniMaxAI/MiniMax-M2.5"), 196_608);
    assert_eq!(model_context_window("anthropic.claude-opus-4"), 200_000);
    assert_eq!(model_context_window("gpt-4o-mini"), 128_000);
    // Unknown family falls back to the conservative default.
    assert_eq!(
        model_context_window("some-local-7b"),
        DEFAULT_CONTEXT_WINDOW_TOKENS
    );
}

#[test]
fn budgeted_max_output_tokens_pins_the_ceiling_for_a_large_window() {
    // GLM-5.2 has a 1M window; light input leaves plenty of room, so the cap is
    // the generous ceiling rather than the full (huge) headroom.
    assert_eq!(
        budgeted_max_output_tokens("zai-org/GLM-5.2", 5_000),
        Some(32_768)
    );
}

#[test]
fn budgeted_max_output_tokens_scales_down_as_context_fills() {
    // gpt-4o-mini: 128k window, 100k input. 128_000 - 100_000 - 10_240 = 17_760,
    // which is below the ceiling, so the pin tracks the remaining headroom.
    assert_eq!(
        budgeted_max_output_tokens("gpt-4o-mini", 100_000),
        Some(17_760)
    );
}

#[test]
fn budgeted_max_output_tokens_is_none_when_headroom_is_tiny() {
    // gpt-4o-mini with input near the window: 128_000 - 126_000 - 10_240 saturates
    // below MIN_USEFUL_OUTPUT, so we skip the pin and let the provider default
    // stand (relieving real context pressure is compaction's job).
    assert_eq!(budgeted_max_output_tokens("gpt-4o-mini", 126_000), None);
}

#[test]
fn budgeted_max_output_tokens_never_exceeds_a_small_window() {
    // A small unknown-family model (default window) with the safety buffer alone
    // consuming most of it must not be pinned above what it can serve; here the
    // remaining headroom is below the useful floor, so None.
    let ctx = model_context_window("some-local-7b");
    let near_full = ctx.saturating_sub(1_000);
    assert_eq!(budgeted_max_output_tokens("some-local-7b", near_full), None);
}

/// GLM-4.5-Air must NOT inherit a generic `glm` window: its served cap (98,304)
/// is below the budget the old flat 128K rule produced (128_000 * 0.8 = 102,400),
/// which would have pushed the agent's budget *above* the real window and let the
/// request overflow before compaction ever engaged. The more specific rule wins.
#[test]
fn glm_4_5_air_is_not_oversized_by_generic_glm_rule() {
    assert_eq!(model_context_window("zai-org/GLM-4.5-Air"), 98_304);
    assert_ne!(model_context_window("zai-org/GLM-4.5-Air"), 131_072);
}

/// Regression guard: no rule may report a window larger than the cap the
/// provider actually serves. A budget computed from an oversized window never
/// triggers compaction in time, so this catches the GLM-4.5-Air class of bug
/// for every family we have probed.
#[test]
fn no_family_window_exceeds_probed_served_cap() {
    // (model id, served `max_prompt_tokens` measured against the live API)
    let probed: &[(&str, u64)] = &[
        ("zai-org/GLM-5.2", 1_048_576),
        ("zai-org/GLM-5.1", 202_752),
        ("zai-org/GLM-5", 202_752),
        ("zai-org/GLM-5V-Turbo", 202_752),
        ("zai-org/GLM-4.5-Air", 98_304),
        ("deepseek-ai/DeepSeek-V4-Pro", 1_000_000),
        ("deepseek-ai/DeepSeek-V4-Flash", 1_048_576),
        ("deepseek-ai/DeepSeek-V3.2", 163_840),
        ("moonshotai/Kimi-K2.7-Code", 262_144),
        ("MiniMaxAI/MiniMax-M3", 700_000),
        ("MiniMaxAI/MiniMax-M2.5", 196_608),
    ];
    for (model, served) in probed {
        assert!(
            model_context_window(model) <= *served,
            "{model}: reported window {} exceeds served cap {served}",
            model_context_window(model),
        );
    }
}

#[test]
fn context_window_trait_default_delegates_to_family_lookup() {
    let p = OpenAiProvider::candle_vllm();
    assert_eq!(p.context_window("zai-org/GLM-5.2"), 1_048_576);
    assert_eq!(p.context_window("unknown"), DEFAULT_CONTEXT_WINDOW_TOKENS);
}

fn img_attachment() -> ff_core::Attachment {
    ff_core::Attachment {
        kind: ff_core::AttachmentKind::Image,
        media_type: "image/png".into(),
        source: ff_core::AttachmentSource::Inline("aGk=".into()),
        name: None,
        bytes: 2,
    }
}

#[test]
fn messages_for_wire_strips_attachments_when_no_vision() {
    let msg = ChatMessage::multimodal("user", "see this", vec![img_attachment()]);
    let stripped = messages_for_wire(std::slice::from_ref(&msg), false, false);
    assert!(stripped[0].attachments.is_empty());
    // A stripped, text-only message serializes without an `attachments` key.
    let v = serde_json::to_value(&stripped[0]).unwrap();
    assert!(v.get("attachments").is_none());
}

#[test]
fn messages_for_wire_keeps_attachments_when_vision() {
    let msg = ChatMessage::multimodal("user", "see this", vec![img_attachment()]);
    let kept = messages_for_wire(std::slice::from_ref(&msg), true, false);
    assert_eq!(kept[0].attachments.len(), 1);
}

#[test]
fn messages_for_wire_borrows_text_only_path() {
    let msgs = vec![ChatMessage::text("user", "hi")];
    // No attachments anywhere -> borrowed (zero-copy), regardless of the flag.
    assert!(matches!(
        messages_for_wire(&msgs, false, false),
        std::borrow::Cow::Borrowed(_)
    ));
}

fn doc_attachment() -> ff_core::Attachment {
    ff_core::Attachment {
        kind: ff_core::AttachmentKind::Document,
        media_type: "application/pdf".into(),
        source: ff_core::AttachmentSource::Inline("aGk=".into()),
        name: Some("report.pdf".into()),
        bytes: 2,
    }
}

#[test]
fn messages_for_wire_gates_image_and_document_independently() {
    let msg = ChatMessage::multimodal(
        "user",
        "see these",
        vec![img_attachment(), doc_attachment()],
    );

    let vision_only = messages_for_wire(std::slice::from_ref(&msg), true, false);
    assert_eq!(vision_only[0].attachments.len(), 1);
    assert_eq!(
        vision_only[0].attachments[0].kind,
        ff_core::AttachmentKind::Image
    );

    let docs_only = messages_for_wire(std::slice::from_ref(&msg), false, true);
    assert_eq!(docs_only[0].attachments.len(), 1);
    assert_eq!(
        docs_only[0].attachments[0].kind,
        ff_core::AttachmentKind::Document
    );

    let neither = messages_for_wire(std::slice::from_ref(&msg), false, false);
    assert!(neither[0].attachments.is_empty());

    assert!(matches!(
        messages_for_wire(std::slice::from_ref(&msg), true, true),
        std::borrow::Cow::Borrowed(_)
    ));
}

struct EndlessProvider {
    polled: Arc<AtomicUsize>,
    first_role: Mutex<Option<String>>,
    // Ramp depth this double reports from `warmup_ramp_steps`, so a test can
    // assert an override (e.g. Ollama's 1) is honored without a live daemon.
    ramp: u8,
}

#[async_trait]
impl Provider for EndlessProvider {
    fn warmup_ramp_steps(&self) -> u8 {
        self.ramp
    }

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
                    truncated: false,
                    ..Chunk::default()
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
        ramp: 32,
    };
    provider.test_connection("test-model").await.unwrap();
}

#[tokio::test]
async fn warmup_sends_one_user_turn_and_stops_early() {
    let provider = EndlessProvider {
        polled: Arc::new(AtomicUsize::new(0)),
        first_role: Mutex::new(None),
        ramp: 32,
    };
    provider.warmup("test-model").await.unwrap();
    assert_eq!(provider.first_role.lock().unwrap().as_deref(), Some("user"));
    // Bounded to the default ramp: warmup never drains the endless stream.
    assert_eq!(
        provider.polled.load(Ordering::SeqCst),
        32,
        "warmup should drain exactly the default ramp depth"
    );
}

#[tokio::test]
async fn warmup_honors_a_shallow_ramp_override() {
    // A residency-based provider (e.g. Ollama, ramp 1) must drain only its
    // reported ramp depth, not the candle-vLLM default of 32 (#61).
    let provider = EndlessProvider {
        polled: Arc::new(AtomicUsize::new(0)),
        first_role: Mutex::new(None),
        ramp: 1,
    };
    provider.warmup("test-model").await.unwrap();
    assert_eq!(
        provider.polled.load(Ordering::SeqCst),
        1,
        "a ramp-1 provider must drain exactly one chunk"
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

// ---- #375 PR-2: wire-dialect selector + carrier hygiene ----

#[test]
fn wire_dialect_defaults_for_local_and_vanilla_gateways() {
    use ff_core::ProviderKind as K;
    for kind in [K::CandleVllm, K::Ollama, K::Bedrock] {
        let d = wire_dialect(kind, None, "any-model");
        assert_eq!(d.reasoning, ReasoningWire::None, "{kind:?}");
        assert_eq!(d.tool_call_content, ToolCallContent::Omit, "{kind:?}");
    }
    // Vanilla OpenAI (no vendor descriptor) is also a no-op.
    let d = wire_dialect(K::OpenAi, None, "gpt-4o-mini");
    assert_eq!(d.reasoning, ReasoningWire::None);
    assert_eq!(d.tool_call_content, ToolCallContent::Omit);
}

#[test]
fn wire_dialect_siliconflow_replays_reasoning_content() {
    // Confirmed empirically against api.siliconflow.com: DeepSeek thinking
    // mode returns intermittent HTTP 400 (code 20015) without this echo.
    let d = wire_dialect(
        ff_core::ProviderKind::SiliconFlow,
        None,
        "deepseek-ai/DeepSeek-V4-Pro",
    );
    assert_eq!(d.reasoning, ReasoningWire::ReasoningContent);
    assert_eq!(d.tool_call_content, ToolCallContent::Omit);
}

#[test]
fn wire_dialect_siliconflow_glm_minimax_use_empty_string() {
    // Confirmed empirically: GLM-5.2 returns 20015 "content cannot be null"
    // when an assistant tool-call message omits content; "" is accepted.
    for model in ["zai-org/GLM-5.2", "MiniMax/MiniMax-M3"] {
        let d = wire_dialect(ff_core::ProviderKind::SiliconFlow, None, model);
        assert_eq!(d.reasoning, ReasoningWire::ReasoningContent, "{model}");
        assert_eq!(d.tool_call_content, ToolCallContent::EmptyString, "{model}");
    }
}

#[test]
fn wire_dialect_openrouter_replays_reasoning_field() {
    // OpenRouter rides the OpenAi kind today; vendor descriptor selects the dialect.
    let d = wire_dialect(
        ff_core::ProviderKind::OpenAi,
        Some("openrouter"),
        "anthropic/claude-3.7-sonnet:thinking",
    );
    assert_eq!(d.reasoning, ReasoningWire::Reasoning);
    assert_eq!(d.tool_call_content, ToolCallContent::Omit);
}

#[test]
fn reasoning_control_targets_all_siliconflow_except_forced_reasoning() {
    use ff_core::ProviderKind as K;
    // Verified #394: GLM-5.2, Kimi-K2.7-Code and DeepSeek-V4-Pro all honor
    // the gateway knobs. The effort dial selects the cap, default Medium.
    for model in [
        "zai-org/GLM-5.2",
        "moonshotai/Kimi-K2.7-Code",
        "deepseek-ai/DeepSeek-V4-Pro",
    ] {
        assert_eq!(
            reasoning_control(K::SiliconFlow, model, ReasoningEffort::Medium),
            ReasoningControl::SiliconFlow {
                effort: ReasoningEffort::Medium
            },
            "{model}"
        );
    }
    // The effort dial flows through unchanged.
    assert_eq!(
        reasoning_control(K::SiliconFlow, "zai-org/GLM-5.2", ReasoningEffort::Low),
        ReasoningControl::SiliconFlow {
            effort: ReasoningEffort::Low
        }
    );
    // Forced-reasoning models (DeepSeek-R1, QwQ) reject enable_thinking, so
    // they are left alone.
    for model in ["deepseek-ai/DeepSeek-R1", "Qwen/QwQ-32B"] {
        assert_eq!(
            reasoning_control(K::SiliconFlow, model, ReasoningEffort::Medium),
            ReasoningControl::None,
            "{model}"
        );
    }
    // Other gateways never emit SiliconFlow-specific params.
    assert_eq!(
        reasoning_control(K::OpenAi, "gpt-4o", ReasoningEffort::High),
        ReasoningControl::None
    );
    assert_eq!(
        reasoning_control(
            K::CandleVllm,
            "any-glm-named-local",
            ReasoningEffort::Medium
        ),
        ReasoningControl::None
    );
}

#[test]
fn reasoning_effort_budgets_match_frontend_dial() {
    assert_eq!(ReasoningEffort::Low.budget_tokens(), 1024);
    assert_eq!(ReasoningEffort::Medium.budget_tokens(), 4096);
    assert_eq!(ReasoningEffort::High.budget_tokens(), 8192);
    assert_eq!(ReasoningEffort::default(), ReasoningEffort::Medium);
    // Every level sits in the Anthropic/Bedrock valid range [1024, 32000).
    for e in [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
    ] {
        assert!((1024..32_000).contains(&e.budget_tokens()), "{e:?}");
    }
}

#[test]
fn chat_message_reasoning_is_never_serialized_through_derive() {
    // The carrier MUST be #[serde(skip)] -- openai::message_to_wire calls
    // serde_json::to_value(msg) and would otherwise leak this field on every
    // gateway, breaking vanilla OpenAI which rejects unknown fields.
    let mut msg = ChatMessage::text("assistant", "");
    msg.reasoning = Some("chain of thought".to_string());
    let v = serde_json::to_value(&msg).unwrap();
    assert!(
        v.get("reasoning").is_none(),
        "reasoning leaked through derive: {v}"
    );
    assert!(v.get("reasoning_content").is_none());
}

// --- #571: rate-limit classification + Retry-After parsing -------------

#[test]
fn rate_limited_is_transient() {
    assert!(LlmError::RateLimited {
        retry_after: None,
        message: "slow down".into()
    }
    .is_transient());
    assert!(LlmError::RateLimited {
        retry_after: Some(std::time::Duration::from_secs(30)),
        message: "tpm".into()
    }
    .is_transient());
}

#[test]
fn is_rate_limit_body_matches_known_signatures() {
    for body in [
        "Rate limit exceeded",
        "rate_limit_exceeded",
        "Too Many Requests",
        "TPM limit reached for this model",
        "RPM quota exceeded",
        "You have exceeded your quota",
    ] {
        assert!(is_rate_limit_body(body), "should match: {body}");
    }
    // A generic 422 body is NOT a rate limit.
    for body in [
        "invalid 'messages': must be a non-empty array",
        "unsupported model",
        "missing required field 'model'",
        // Bare "exceeded" must not match: context-length is a common,
        // non-retryable 422 that has no limit/quota/rate term.
        "This model's maximum context length is 32768 tokens; however you requested 40000 exceeded",
        "maximum context length exceeded",
    ] {
        assert!(!is_rate_limit_body(body), "should not match: {body}");
    }
}

#[test]
fn parse_retry_after_delta_seconds() {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert(reqwest::header::RETRY_AFTER, "30".parse().unwrap());
    assert_eq!(
        parse_retry_after(&h),
        Some(std::time::Duration::from_secs(30))
    );
}

#[test]
fn parse_retry_after_http_date_in_future() {
    // 1 hour from now, formatted as an HTTP-date.
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    let mut h = reqwest::header::HeaderMap::new();
    h.insert(
        reqwest::header::RETRY_AFTER,
        httpdate::fmt_http_date(future).parse().unwrap(),
    );
    let d = parse_retry_after(&h).expect("date parses");
    // Allow a few seconds of slack for clock/rounding between fmt and parse.
    assert!(
        d.as_secs() >= 3590 && d.as_secs() <= 3600,
        "expected ~3600s, got {}",
        d.as_secs()
    );
}

#[test]
fn parse_retry_after_past_date_saturates_to_zero() {
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    let mut h = reqwest::header::HeaderMap::new();
    h.insert(
        reqwest::header::RETRY_AFTER,
        httpdate::fmt_http_date(past).parse().unwrap(),
    );
    assert_eq!(parse_retry_after(&h), Some(std::time::Duration::ZERO));
}

#[test]
fn parse_retry_after_absent_or_garbage_is_none() {
    let empty = reqwest::header::HeaderMap::new();
    assert_eq!(parse_retry_after(&empty), None);
    let mut h = reqwest::header::HeaderMap::new();
    h.insert(reqwest::header::RETRY_AFTER, "soon".parse().unwrap());
    assert_eq!(parse_retry_after(&h), None);
}

#[tokio::test]
async fn read_bounded_body_stops_at_read_limit() {
    // A body far larger than the read limit must not be fully buffered into
    // memory. The bounded read stops after `ERROR_BODY_READ_LIMIT` bytes
    // rather than reading the entire body and truncating afterwards. (#517 nit 2)
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let huge = "x".repeat(100_000);
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(huge))
        .mount(&server)
        .await;

    let resp = build_streaming_http_client()
        .get(server.uri())
        .send()
        .await
        .unwrap();
    let text = read_bounded_body(resp, ERROR_BODY_READ_LIMIT).await;
    assert_eq!(
        text.len(),
        ERROR_BODY_READ_LIMIT,
        "read should stop at the limit, not buffer the full 100 KB body"
    );
}

#[tokio::test]
async fn error_for_status_with_body_surfaces_normal_body_unchanged() {
    // For a normal-sized error body the surfaced message must be identical
    // to the pre-hardening behavior — the bounded read is transparent. (#517 nit 2)
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let body = r#"{"code":20015,"message":"Field required"}"#;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(400).set_body_string(body))
        .mount(&server)
        .await;

    let resp = build_streaming_http_client()
        .get(server.uri())
        .send()
        .await
        .unwrap();
    match error_for_status_with_body(resp).await {
        Err(LlmError::Api { status, message }) => {
            assert_eq!(status, 400);
            assert_eq!(message, body, "normal-sized body surfaced unchanged");
        }
        other => panic!("expected Api 400, got {other:?}"),
    }
}

#[tokio::test]
async fn error_for_status_with_body_truncates_oversized_body() {
    // An oversized error body is truncated to the 2 KB cap with a flag — the
    // same surfaced result as before the hardening, but now the read itself
    // is bounded. (#517 nit 2)
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let huge = "x".repeat(10_000);
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(400).set_body_string(huge))
        .mount(&server)
        .await;

    let resp = build_streaming_http_client()
        .get(server.uri())
        .send()
        .await
        .unwrap();
    match error_for_status_with_body(resp).await {
        Err(LlmError::Api { status, message }) => {
            assert_eq!(status, 400);
            assert!(message.ends_with("...[truncated]"));
            assert!(
                message.len() < 2_100,
                "message not bounded: {}",
                message.len()
            );
        }
        other => panic!("expected Api 400, got {other:?}"),
    }
}
