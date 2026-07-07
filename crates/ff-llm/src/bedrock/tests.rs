use super::*;
use crate::{FunctionCall, ToolCall};
use aws_sdk_bedrockruntime::types::{
    ContentBlockDeltaEvent, ContentBlockStartEvent, ToolUseBlockDelta, ToolUseBlockStart,
};
use base64::Engine as _;
use ff_core::AttachmentSource;

fn budget_of(doc: &Document) -> u64 {
    let Document::Object(top) = doc else {
        panic!("expected object")
    };
    let Document::Object(rc) = &top["reasoning_config"] else {
        panic!("expected reasoning_config object")
    };
    assert_eq!(rc["type"], Document::String("enabled".to_string()));
    match rc["budget_tokens"] {
        Document::Number(Number::PosInt(v)) => v,
        _ => panic!("budget_tokens not a positive int"),
    }
}

#[tokio::test]
async fn client_is_reused_across_calls_and_instances() {
    let creds = || BedrockCreds::IamKeys {
        access_key_id: "AKIAEXAMPLE".into(),
        secret_access_key: "secret".into(),
        session_token: None,
    };
    let a = BedrockProvider::new("us-east-2", creds());
    let b = BedrockProvider::new("us-east-2", creds());

    assert_eq!(
        a.client_cache_key(),
        a.client_cache_key(),
        "cache key must be stable across calls (per-iteration reuse)"
    );
    assert_eq!(
        a.client_cache_key(),
        b.client_cache_key(),
        "identical connection must reuse the cached client across turns"
    );

    let _c1 = a.client().await;
    let _c2 = b.client().await;
}

#[test]
fn client_cache_key_distinguishes_region_and_credentials() {
    let base = BedrockProvider::new(
        "us-east-2",
        BedrockCreds::Profile {
            name: "bedrock-profile".into(),
        },
    );
    let other_region = BedrockProvider::new(
        "us-west-2",
        BedrockCreds::Profile {
            name: "bedrock-profile".into(),
        },
    );
    let other_profile = BedrockProvider::new(
        "us-east-2",
        BedrockCreds::Profile {
            name: "different-profile".into(),
        },
    );
    assert_ne!(
        base.client_cache_key(),
        other_region.client_cache_key(),
        "different region must not share a client"
    );
    assert_ne!(
        base.client_cache_key(),
        other_profile.client_cache_key(),
        "different profile must not share a client"
    );

    let same = BedrockProvider::new(
        "us-east-2",
        BedrockCreds::Profile {
            name: "bedrock-profile".into(),
        },
    );
    assert_eq!(base.client_cache_key(), same.client_cache_key());

    let key1 = BedrockProvider::new(
        "us-east-2",
        BedrockCreds::IamKeys {
            access_key_id: "AKIA".into(),
            secret_access_key: "s1".into(),
            session_token: None,
        },
    )
    .client_cache_key();
    let key2 = BedrockProvider::new(
        "us-east-2",
        BedrockCreds::IamKeys {
            access_key_id: "AKIA".into(),
            secret_access_key: "s2".into(),
            session_token: None,
        },
    )
    .client_cache_key();
    assert_ne!(key1, key2, "a changed secret must not reuse the old client");

    let profile_key = base.client_cache_key();
    let apikey = BedrockProvider::new(
        "us-east-2",
        BedrockCreds::ApiKey {
            token: "bedrock-profile".into(),
        },
    )
    .client_cache_key();
    assert_ne!(
        profile_key, apikey,
        "credential mode must be part of the key"
    );
}

#[test]
fn reasoning_config_doc_enables_thinking_with_budget() {
    let low = ReasoningEffort::Low.budget_tokens();
    let med = ReasoningEffort::Medium.budget_tokens();
    let high = ReasoningEffort::High.budget_tokens();
    assert_eq!(budget_of(&reasoning_config_doc(low)), 1024);
    assert_eq!(budget_of(&reasoning_config_doc(med)), 4096);
    assert_eq!(budget_of(&reasoning_config_doc(high)), 8192);
    for b in [low, med, high] {
        assert!(b + BEDROCK_ANSWER_HEADROOM > b);
    }
}

#[test]
fn adaptive_thinking_doc_emits_type_and_effort() {
    for (effort, label) in [
        (ReasoningEffort::Low, "low"),
        (ReasoningEffort::Medium, "medium"),
        (ReasoningEffort::High, "high"),
    ] {
        let Document::Object(top) = adaptive_thinking_doc(effort) else {
            panic!("expected object")
        };
        let Document::Object(thinking) = &top["thinking"] else {
            panic!("expected thinking object")
        };
        assert_eq!(thinking["type"], Document::String("adaptive".to_string()));
        assert!(
            !thinking.contains_key("effort"),
            "effort must not nest inside thinking"
        );
        let Document::Object(oc) = &top["output_config"] else {
            panic!("expected output_config object")
        };
        assert_eq!(oc["effort"], Document::String(label.to_string()));
        assert!(!top.contains_key("reasoning_config"));
    }
}

#[test]
fn thinking_request_config_emits_legacy_budget_or_adaptive_effort() {
    let (legacy, legacy_max) =
        thinking_request_config("anthropic.claude-sonnet-4-5", ReasoningEffort::High);
    assert_eq!(budget_of(&legacy), 8192);
    assert_eq!(
        legacy_max,
        Some((8192 + BEDROCK_ANSWER_HEADROOM) as i32),
        "legacy Converse thinking pins maxTokens above the budget"
    );

    let (adaptive, adaptive_max) =
        thinking_request_config("us.anthropic.claude-opus-4-8", ReasoningEffort::High);
    assert_eq!(
        adaptive_max,
        Some(ADAPTIVE_THINKING_MAX_TOKENS),
        "adaptive thinking pins a generous maxTokens so thinking cannot starve tool output (#528)"
    );
    let Document::Object(top) = adaptive else {
        panic!("expected object")
    };
    assert!(
        !top.contains_key("reasoning_config"),
        "adaptive models must not emit deprecated budget_tokens"
    );
    let Document::Object(output_config) = &top["output_config"] else {
        panic!("expected output_config object")
    };
    assert_eq!(
        output_config["effort"],
        Document::String(ReasoningEffort::High.effort_str().to_string())
    );
}

#[test]
fn high_effort_provider_emits_legacy_budget_or_adaptive_effort() {
    let provider = BedrockProvider::new(
        "us-east-2",
        BedrockCreds::ApiKey {
            token: "secret".into(),
        },
    )
    .with_reasoning_effort(ReasoningEffort::High);

    let (legacy, legacy_max) = provider.thinking_config_for("anthropic.claude-sonnet-4-5");
    assert_eq!(budget_of(&legacy), 8192);
    assert_eq!(
        legacy_max,
        Some((8192 + BEDROCK_ANSWER_HEADROOM) as i32),
        "legacy Converse thinking pins maxTokens above the budget"
    );

    let (adaptive, adaptive_max) = provider.thinking_config_for("us.anthropic.claude-opus-4-8");
    assert_eq!(
        adaptive_max,
        Some(ADAPTIVE_THINKING_MAX_TOKENS),
        "adaptive thinking pins a generous maxTokens so thinking cannot starve tool output (#528)"
    );
    let Document::Object(top) = adaptive else {
        panic!("expected object")
    };
    assert!(
        !top.contains_key("reasoning_config"),
        "adaptive models must not emit deprecated budget_tokens"
    );
    let Document::Object(output_config) = &top["output_config"] else {
        panic!("expected output_config object")
    };
    assert_eq!(
        output_config["effort"],
        Document::String(ReasoningEffort::High.effort_str().to_string())
    );
}

#[test]
fn uses_adaptive_thinking_splits_by_model_generation() {
    for m in [
        "us.anthropic.claude-opus-4-8",
        "us.anthropic.claude-opus-4-6",
        "us.anthropic.claude-opus-4-7",
        "claude-sonnet-4-6",
        "us.anthropic.claude-opus-4-9",
        "us.anthropic.claude-opus-5-0",
        "claude-mythos-5",
        "claude-fable-5",
    ] {
        assert!(uses_adaptive_thinking(m), "expected adaptive: {m}");
    }
    for m in [
        "us.anthropic.claude-opus-4-5",
        "us.anthropic.claude-sonnet-4-5",
        "anthropic.claude-3-5-sonnet-20241022-v2:0",
        "us.anthropic.claude-3-opus-20240229-v1:0",
        "meta.llama3-70b",
    ] {
        assert!(!uses_adaptive_thinking(m), "expected legacy: {m}");
    }
}

fn assistant_with_call(args: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "bash".into(),
                arguments: args.into(),
            },
        }]),
        tool_call_id: None,
        name: None,

        attachments: Vec::new(),
        reasoning: None,
    }
}

#[test]
fn system_messages_become_system_blocks() {
    let (system, messages) = to_converse(&[
        ChatMessage::text("system", "be brief"),
        ChatMessage::text("user", "hi"),
    ]);
    assert_eq!(system.len(), 1);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, ConversationRole::User);
}

#[test]
fn user_summary_stays_in_messages_not_hoisted() {
    let (system, messages) = to_converse(&[
        ChatMessage::text("system", "be brief"),
        ChatMessage::text("user", "Summary of 40 earlier messages"),
        ChatMessage::text("assistant", "recent verbatim reply"),
    ]);
    assert_eq!(system.len(), 1);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, ConversationRole::User);
    assert_eq!(messages[1].role, ConversationRole::Assistant);
}

#[test]
fn consecutive_same_role_messages_merge() {
    let (_, messages) = to_converse(&[
        ChatMessage::text("user", "a"),
        ChatMessage::text("user", "b"),
        ChatMessage::text("assistant", "c"),
    ]);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].content.len(), 2);
    assert_eq!(messages[1].role, ConversationRole::Assistant);
}

#[test]
fn tool_role_maps_to_user_tool_result() {
    let msg = ChatMessage {
        role: "tool".into(),
        content: Some("result body".into()),
        tool_calls: None,
        tool_call_id: Some("call_1".into()),
        name: Some("bash".into()),

        attachments: Vec::new(),
        reasoning: None,
    };
    let (_, messages) = to_converse(&[msg]);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, ConversationRole::User);
    assert!(matches!(
        messages[0].content[0],
        ContentBlock::ToolResult(_)
    ));
}

#[test]
fn assistant_tool_calls_become_tool_use_blocks() {
    let (_, messages) = to_converse(&[assistant_with_call(r#"{"command":"ls"}"#)]);
    let block = &messages[0].content[0];
    match block {
        ContentBlock::ToolUse(b) => {
            assert_eq!(b.tool_use_id, "call_1");
            assert_eq!(b.name, "bash");
        }
        _ => panic!("expected tool-use block"),
    }
}

fn tool_use_input(args: &str) -> serde_json::Value {
    let (_, messages) = to_converse(&[assistant_with_call(args)]);
    match &messages[0].content[0] {
        ContentBlock::ToolUse(b) => doc_to_json(&b.input),
        _ => panic!("expected tool-use block"),
    }
}

#[test]
fn empty_args_tool_use_becomes_empty_object() {
    assert_eq!(tool_use_input(""), serde_json::json!({}));
}

#[test]
fn invalid_args_tool_use_becomes_empty_object() {
    assert_eq!(tool_use_input("not json"), serde_json::json!({}));
}

#[test]
fn null_args_tool_use_becomes_empty_object() {
    assert_eq!(tool_use_input("null"), serde_json::json!({}));
}

#[test]
fn object_args_are_preserved() {
    assert_eq!(
        tool_use_input(r#"{"command":"ls"}"#),
        serde_json::json!({"command": "ls"})
    );
}

fn assistant_with_calls(ids: &[&str]) -> ChatMessage {
    ChatMessage {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(
            ids.iter()
                .map(|id| ToolCall {
                    id: (*id).into(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: "bash".into(),
                        arguments: r#"{"command":"ls"}"#.into(),
                    },
                })
                .collect(),
        ),
        tool_call_id: None,
        name: None,

        attachments: Vec::new(),
        reasoning: None,
    }
}

fn tool_result(id: &str) -> ChatMessage {
    ChatMessage {
        role: "tool".into(),
        content: Some("ok".into()),
        tool_calls: None,
        tool_call_id: Some(id.into()),
        name: Some("bash".into()),

        attachments: Vec::new(),
        reasoning: None,
    }
}

fn result_ids(msg: &Message) -> Vec<&str> {
    msg.content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolResult(r) => Some(r.tool_use_id.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn dangling_tool_use_gets_synthetic_result() {
    let (_, messages) = to_converse(&[
        assistant_with_calls(&["call_1"]),
        ChatMessage::text("user", "what next?"),
    ]);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].role, ConversationRole::User);
    assert_eq!(result_ids(&messages[1]), vec!["call_1"]);
    assert!(matches!(messages[1].content[1], ContentBlock::Text(_)));
}

#[test]
fn trailing_tool_use_gets_synthetic_result() {
    let (_, messages) = to_converse(&[assistant_with_calls(&["call_1"])]);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].role, ConversationRole::User);
    assert_eq!(result_ids(&messages[1]), vec!["call_1"]);
}

#[test]
fn partial_tool_results_backfilled() {
    let (_, messages) = to_converse(&[
        assistant_with_calls(&["call_1", "call_2"]),
        tool_result("call_1"),
    ]);
    assert_eq!(messages.len(), 2);
    let mut ids = result_ids(&messages[1]);
    ids.sort_unstable();
    assert_eq!(ids, vec!["call_1", "call_2"]);
}

#[test]
fn well_formed_history_is_unchanged() {
    let (_, messages) = to_converse(&[assistant_with_calls(&["call_1"]), tool_result("call_1")]);
    assert_eq!(messages.len(), 2);
    assert_eq!(result_ids(&messages[1]), vec!["call_1"]);
    assert_eq!(messages[1].content.len(), 1);
}

#[test]
fn text_delta_maps_to_chunk() {
    let event = ConverseStreamOutput::ContentBlockDelta(
        ContentBlockDeltaEvent::builder()
            .delta(ContentBlockDelta::Text("Hello".into()))
            .content_block_index(0)
            .build()
            .unwrap(),
    );
    let chunk = event_to_chunk(event).unwrap();
    assert_eq!(chunk.delta, "Hello");
    assert!(!chunk.done);
}

#[test]
fn reasoning_delta_maps_to_reasoning() {
    let event = ConverseStreamOutput::ContentBlockDelta(
        ContentBlockDeltaEvent::builder()
            .delta(ContentBlockDelta::ReasoningContent(
                ReasoningContentBlockDelta::Text("thinking".into()),
            ))
            .content_block_index(0)
            .build()
            .unwrap(),
    );
    let chunk = event_to_chunk(event).unwrap();
    assert_eq!(chunk.reasoning_delta, "thinking");
}

#[test]
fn tool_use_start_then_delta_preserves_json_args() {
    let start = ConverseStreamOutput::ContentBlockStart(
        ContentBlockStartEvent::builder()
            .start(ContentBlockStart::ToolUse(
                ToolUseBlockStart::builder()
                    .tool_use_id("call_9")
                    .name("bash")
                    .build()
                    .unwrap(),
            ))
            .content_block_index(1)
            .build()
            .unwrap(),
    );
    let chunk = event_to_chunk(start).unwrap();
    assert_eq!(chunk.tool_calls[0].index, 1);
    assert_eq!(chunk.tool_calls[0].id.as_deref(), Some("call_9"));
    assert_eq!(chunk.tool_calls[0].name.as_deref(), Some("bash"));

    let delta = ConverseStreamOutput::ContentBlockDelta(
        ContentBlockDeltaEvent::builder()
            .delta(ContentBlockDelta::ToolUse(
                ToolUseBlockDelta::builder()
                    .input(r#"{"command":"ls"}"#)
                    .build()
                    .unwrap(),
            ))
            .content_block_index(1)
            .build()
            .unwrap(),
    );
    let chunk = event_to_chunk(delta).unwrap();
    assert_eq!(chunk.tool_calls[0].arguments, r#"{"command":"ls"}"#);
    assert!(chunk.tool_calls[0].id.is_none());
}

#[test]
fn message_stop_marks_done() {
    let event = ConverseStreamOutput::MessageStop(
        aws_sdk_bedrockruntime::types::MessageStopEvent::builder()
            .stop_reason(aws_sdk_bedrockruntime::types::StopReason::EndTurn)
            .build()
            .unwrap(),
    );
    let chunk = event_to_chunk(event).unwrap();
    assert!(chunk.done);
    assert!(!chunk.truncated, "EndTurn is a clean stop, not truncation");
}

#[test]
fn message_stop_max_tokens_marks_truncated() {
    let event = ConverseStreamOutput::MessageStop(
        aws_sdk_bedrockruntime::types::MessageStopEvent::builder()
            .stop_reason(StopReason::MaxTokens)
            .build()
            .unwrap(),
    );
    let chunk = event_to_chunk(event).unwrap();
    assert!(chunk.done);
    assert!(
        chunk.truncated,
        "MaxTokens means the output cap cut the turn off mid-stream (#528)"
    );
}

#[test]
fn adaptive_thinking_pins_a_generous_max_tokens() {
    let (_, max_tokens) =
        thinking_request_config("us.anthropic.claude-opus-4-8", ReasoningEffort::High);
    assert_eq!(
        max_tokens,
        Some(ADAPTIVE_THINKING_MAX_TOKENS),
        "adaptive thinking must pin maxTokens so thinking cannot starve tool-call output (#528)"
    );
}

#[test]
fn tool_config_built_from_openai_spec() {
    let spec = serde_json::json!({
        "type": "function",
        "function": {
            "name": "bash",
            "description": "run a shell command",
            "parameters": {
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            }
        }
    });
    let cfg = to_tool_config(&[spec], false).unwrap();
    assert_eq!(cfg.tools.len(), 1);
    match &cfg.tools[0] {
        Tool::ToolSpec(s) => assert_eq!(s.name, "bash"),
        _ => panic!("expected tool spec"),
    }
}

#[test]
fn no_tools_yields_no_config() {
    assert!(to_tool_config(&[], false).is_none());
}

#[test]
fn cache_point_appended_to_tools_when_enabled() {
    let spec = serde_json::json!({
        "type": "function",
        "function": { "name": "bash", "parameters": { "type": "object", "properties": {} } }
    });
    let off = to_tool_config(std::slice::from_ref(&spec), false).unwrap();
    assert_eq!(off.tools.len(), 1);
    assert!(matches!(off.tools[0], Tool::ToolSpec(_)));
    let on = to_tool_config(&[spec], true).unwrap();
    assert_eq!(on.tools.len(), 2);
    assert!(matches!(on.tools[0], Tool::ToolSpec(_)));
    assert!(matches!(on.tools[1], Tool::CachePoint(_)));
}

#[test]
fn model_cache_support_allowlist() {
    for m in [
        "amazon.nova-pro-v1:0",
        "anthropic.claude-3-7-sonnet-20250219-v1:0",
        "anthropic.claude-3-5-haiku-20241022-v1:0",
        "anthropic.claude-3-5-sonnet-20241022-v2:0",
        "anthropic.claude-opus-4-20250514-v1:0",
        "us.anthropic.claude-opus-4-6",
        "anthropic.claude-sonnet-4-20250514-v1:0",
        "anthropic.claude-haiku-5-20260101-v1:0",
        "us.anthropic.claude-opus-5-20260101-v1:0",
    ] {
        assert!(model_supports_cache_point(m), "expected supported: {m}");
    }
    for m in [
        "anthropic.claude-3-5-sonnet-20240620-v1:0",
        "anthropic.claude-3-opus-20240229-v1:0",
        "anthropic.claude-3-haiku-20240307-v1:0",
        "meta.llama3-70b",
        "deepseek-v4-pro",
    ] {
        assert!(!model_supports_cache_point(m), "expected unsupported: {m}");
    }
}

#[test]
fn schema_with_object_type_passes_through() {
    let params = serde_json::json!({
        "type": "object",
        "properties": { "x": { "type": "string" } },
        "required": ["x"]
    });
    assert_eq!(normalize_object_schema(Some(&params)), params);
}

#[test]
fn schema_missing_type_gets_object_injected() {
    let params = serde_json::json!({ "properties": { "x": { "type": "string" } } });
    let out = normalize_object_schema(Some(&params));
    assert_eq!(out["type"], "object");
    assert_eq!(out["properties"]["x"]["type"], "string");
}

#[test]
fn empty_or_non_object_schemas_become_object() {
    let want = serde_json::json!({ "type": "object", "properties": {} });
    assert_eq!(normalize_object_schema(Some(&serde_json::json!({}))), want);
    assert_eq!(
        normalize_object_schema(Some(&serde_json::json!({ "type": "string" }))),
        want
    );
    assert_eq!(normalize_object_schema(None), want);
}

#[test]
fn to_tool_always_sends_object_typed_schema() {
    let spec = serde_json::json!({
        "type": "function",
        "function": {
            "name": "weird",
            "description": "no top-level type",
            "parameters": { "properties": { "q": { "type": "string" } } }
        }
    });
    let cfg = to_tool_config(&[spec], false).unwrap();
    match &cfg.tools[0] {
        Tool::ToolSpec(s) => match s.input_schema.as_ref().unwrap() {
            ToolInputSchema::Json(doc) => {
                assert_eq!(doc_to_json(doc)["type"], "object");
            }
            _ => panic!("expected json input schema"),
        },
        _ => panic!("expected tool spec"),
    }
}

#[test]
fn json_document_round_trips() {
    let value = serde_json::json!({
        "s": "text",
        "n": 42,
        "neg": -7,
        "f": 1.5,
        "b": true,
        "nil": null,
        "arr": [1, 2, 3],
        "nested": { "k": "v" }
    });
    let doc = json_to_doc(&value);
    assert_eq!(doc_to_json(&doc), value);
}

#[test]
fn creds_debug_redacts_secrets() {
    let iam = BedrockCreds::IamKeys {
        access_key_id: "AKIAEXAMPLE".into(),
        secret_access_key: "super-secret-key".into(),
        session_token: Some("super-secret-token".into()),
    };
    let s = format!("{iam:?}");
    assert!(
        !s.contains("super-secret-key"),
        "secret access key leaked: {s}"
    );
    assert!(
        !s.contains("super-secret-token"),
        "session token leaked: {s}"
    );
    assert!(
        s.contains("AKIAEXAMPLE"),
        "access key id should be shown: {s}"
    );

    let api = BedrockCreds::ApiKey {
        token: "br-super-secret-bearer".into(),
    };
    let s = format!("{api:?}");
    assert!(
        !s.contains("br-super-secret-bearer"),
        "bearer token leaked: {s}"
    );

    let iam_none = BedrockCreds::IamKeys {
        access_key_id: "AKIA2".into(),
        secret_access_key: "k".into(),
        session_token: None,
    };
    let s = format!("{iam_none:?}");
    assert!(
        s.contains("None"),
        "absent session token should read None: {s}"
    );
}

#[test]
fn creds_modes_construct() {
    let _ = BedrockProvider::new(
        "us-east-2",
        BedrockCreds::ApiKey {
            token: "secret".into(),
        },
    );
    let _ = BedrockProvider::new(
        "us-east-2",
        BedrockCreds::Profile {
            name: "default".into(),
        },
    );
    let _ = BedrockProvider::new(
        "us-east-2",
        BedrockCreds::IamKeys {
            access_key_id: "AKIA".into(),
            secret_access_key: "secret".into(),
            session_token: None,
        },
    );
}

fn inline_b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn image_msg(media_type: &str, source: AttachmentSource) -> ChatMessage {
    ChatMessage::multimodal(
        "user",
        "look at this",
        vec![Attachment {
            kind: ff_core::AttachmentKind::Image,
            media_type: media_type.into(),
            source,
            name: Some("shot.png".into()),
            bytes: 4,
        }],
    )
}

#[test]
fn multimodal_user_message_carries_image_block() {
    let msg = image_msg(
        "image/png",
        AttachmentSource::Inline(inline_b64(&[0x89, 0x50, 0x4e, 0x47])),
    );
    let (_, messages) = to_converse(&[msg]);
    assert_eq!(messages.len(), 1);
    let content = &messages[0].content;
    assert_eq!(content.len(), 2, "text block then image block");
    assert!(matches!(content[0], ContentBlock::Text(_)));
    match &content[1] {
        ContentBlock::Image(img) => assert_eq!(img.format, ImageFormat::Png),
        other => panic!("expected image block, got {other:?}"),
    }
}

#[test]
fn document_attachment_maps_to_document_block_with_sanitized_name() {
    let msg = ChatMessage::multimodal(
        "user",
        "summarize",
        vec![Attachment {
            kind: ff_core::AttachmentKind::Document,
            media_type: "application/pdf".into(),
            source: AttachmentSource::Inline(inline_b64(b"%PDF-1.4")),
            name: Some("Q3 report (final)/v2.pdf".into()),
            bytes: 8,
        }],
    );
    let (_, messages) = to_converse(&[msg]);
    match &messages[0].content[1] {
        ContentBlock::Document(doc) => {
            assert_eq!(doc.format, DocumentFormat::Pdf);
            assert_eq!(doc.name, "Q3 report (final) v2 pdf");
        }
        other => panic!("expected document block, got {other:?}"),
    }
}

#[test]
fn multiple_unnamed_documents_get_unique_names() {
    let doc = |src: &str| Attachment {
        kind: ff_core::AttachmentKind::Document,
        media_type: "application/pdf".into(),
        source: AttachmentSource::Inline(inline_b64(src.as_bytes())),
        name: None,
        bytes: src.len() as u64,
    };
    let msg = ChatMessage::multimodal("user", "", vec![doc("a"), doc("b")]);
    let (_, messages) = to_converse(&[msg]);
    let names: Vec<&str> = messages[0]
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Document(d) => Some(d.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec!["document", "document-2"]);
}

#[test]
fn image_only_message_is_not_dropped() {
    let msg = ChatMessage::multimodal(
        "user",
        "",
        vec![Attachment {
            kind: ff_core::AttachmentKind::Image,
            media_type: "image/jpeg".into(),
            source: AttachmentSource::Inline(inline_b64(&[0xff, 0xd8, 0xff])),
            name: None,
            bytes: 3,
        }],
    );
    let (_, messages) = to_converse(&[msg]);
    assert_eq!(messages.len(), 1, "image-only turn must not be dropped");
    assert_eq!(messages[0].content.len(), 1);
    assert!(matches!(messages[0].content[0], ContentBlock::Image(_)));
}

#[test]
fn unsupported_media_type_is_skipped() {
    let msg = image_msg(
        "image/svg+xml",
        AttachmentSource::Inline(inline_b64(b"<svg/>")),
    );
    let (_, messages) = to_converse(&[msg]);
    assert_eq!(messages.len(), 1, "turn still sent");
    assert_eq!(messages[0].content.len(), 1, "only the text block remains");
    assert!(matches!(messages[0].content[0], ContentBlock::Text(_)));
}

#[test]
fn unreadable_path_attachment_is_skipped() {
    let msg = image_msg(
        "image/png",
        AttachmentSource::Path("/nonexistent/flowforge/does-not-exist.png".into()),
    );
    let (_, messages) = to_converse(&[msg]);
    assert_eq!(messages[0].content.len(), 1, "unreadable file dropped");
    assert!(matches!(messages[0].content[0], ContentBlock::Text(_)));
}

#[test]
fn undecodable_inline_base64_is_skipped() {
    let msg = image_msg("image/png", AttachmentSource::Inline("not!base64!".into()));
    let (_, messages) = to_converse(&[msg]);
    assert_eq!(messages[0].content.len(), 1, "bad base64 dropped");
}

#[test]
fn document_format_falls_back_to_extension() {
    let msg = ChatMessage::multimodal(
        "user",
        "",
        vec![Attachment {
            kind: ff_core::AttachmentKind::Document,
            media_type: "application/octet-stream".into(),
            source: AttachmentSource::Inline(inline_b64(b"col1,col2")),
            name: Some("data.csv".into()),
            bytes: 9,
        }],
    );
    let (_, messages) = to_converse(&[msg]);
    match &messages[0].content[0] {
        ContentBlock::Document(d) => assert_eq!(d.format, DocumentFormat::Csv),
        other => panic!("expected document block, got {other:?}"),
    }
}

#[test]
fn json_document_maps_to_txt() {
    for (media, name) in [
        ("application/json", "config.json"),
        ("application/octet-stream", "config.json"),
    ] {
        let msg = ChatMessage::multimodal(
            "user",
            "",
            vec![Attachment {
                kind: ff_core::AttachmentKind::Document,
                media_type: media.into(),
                source: AttachmentSource::Inline(inline_b64(b"{\"k\":1}")),
                name: Some(name.into()),
                bytes: 7,
            }],
        );
        let (_, messages) = to_converse(&[msg]);
        match &messages[0].content[0] {
            ContentBlock::Document(d) => assert_eq!(d.format, DocumentFormat::Txt),
            other => panic!("expected document block, got {other:?}"),
        }
    }
}

#[test]
fn python_document_maps_to_txt() {
    // #842: `.py` has no Bedrock DocumentFormat variant, so it routes to Txt —
    // whether the browser reported `text/x-python` or nothing (extension fallback).
    for (media, name) in [
        ("text/x-python", "script.py"),
        ("application/x-python-code", "script.py"),
        ("application/octet-stream", "script.py"),
        ("", "script.py"),
    ] {
        let msg = ChatMessage::multimodal(
            "user",
            "",
            vec![Attachment {
                kind: ff_core::AttachmentKind::Document,
                media_type: media.into(),
                source: AttachmentSource::Inline(inline_b64(b"print('hi')\n")),
                name: Some(name.into()),
                bytes: 12,
            }],
        );
        let (_, messages) = to_converse(&[msg]);
        match &messages[0].content[0] {
            ContentBlock::Document(d) => assert_eq!(
                d.format,
                DocumentFormat::Txt,
                "py via media={media:?} should map to Txt"
            ),
            other => panic!("expected document block, got {other:?}"),
        }
    }
}

#[test]
fn text_only_message_is_unchanged() {
    let (_, messages) = to_converse(&[ChatMessage::text("user", "plain turn")]);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content.len(), 1);
    match &messages[0].content[0] {
        ContentBlock::Text(t) => assert_eq!(t, "plain turn"),
        other => panic!("expected text block, got {other:?}"),
    }
}

#[test]
fn assistant_message_with_attachments_emits_no_image_block() {
    let msg = ChatMessage::multimodal(
        "assistant",
        "here you go",
        vec![Attachment {
            kind: ff_core::AttachmentKind::Image,
            media_type: "image/png".into(),
            source: AttachmentSource::Inline(inline_b64(&[0x89, 0x50, 0x4e, 0x47])),
            name: None,
            bytes: 4,
        }],
    );
    let (_, messages) = to_converse(&[msg]);
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0]
            .content
            .iter()
            .all(|b| !matches!(b, ContentBlock::Image(_) | ContentBlock::Document(_))),
        "assistant turn must not carry image/document blocks"
    );
}

#[test]
fn vision_off_strips_attachments_before_converse() {
    let msg = image_msg(
        "image/png",
        AttachmentSource::Inline(inline_b64(&[0x89, 0x50, 0x4e, 0x47])),
    );
    let wire = crate::messages_for_wire(std::slice::from_ref(&msg), false, false);
    let (_, messages) = to_converse(&wire);
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0]
            .content
            .iter()
            .all(|b| !matches!(b, ContentBlock::Image(_) | ContentBlock::Document(_))),
        "vision off: no image/document block reaches Converse"
    );
}

#[test]
fn vision_on_keeps_image_block() {
    let msg = image_msg(
        "image/png",
        AttachmentSource::Inline(inline_b64(&[0x89, 0x50, 0x4e, 0x47])),
    );
    let wire = crate::messages_for_wire(std::slice::from_ref(&msg), true, true);
    let (_, messages) = to_converse(&wire);
    assert!(
        messages[0]
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Image(_))),
        "vision on: the image block is emitted"
    );
}

fn tool_use_block(id: &str, name: &str) -> ContentBlock {
    ContentBlock::ToolUse(
        ToolUseBlock::builder()
            .tool_use_id(id)
            .name(name)
            .input(Document::Object(HashMap::new()))
            .build()
            .unwrap(),
    )
}

fn tool_result_block(id: &str) -> ContentBlock {
    ContentBlock::ToolResult(
        ToolResultBlock::builder()
            .tool_use_id(id)
            .content(ToolResultContentBlock::Text("ok".to_string()))
            .build()
            .unwrap(),
    )
}

fn msg(role: ConversationRole, blocks: Vec<ContentBlock>) -> Message {
    Message::builder()
        .role(role)
        .set_content(Some(blocks))
        .build()
        .unwrap()
}

#[test]
fn enforce_strips_orphaned_results_from_parallel_loop() {
    let messages = vec![
        msg(
            ConversationRole::Assistant,
            vec![tool_use_block("A", "bash"), tool_use_block("B", "bash")],
        ),
        msg(ConversationRole::User, vec![tool_result_block("A")]),
        msg(
            ConversationRole::Assistant,
            vec![tool_use_block("C", "bash")],
        ),
        msg(ConversationRole::User, vec![tool_result_block("B")]),
        msg(
            ConversationRole::Assistant,
            vec![ContentBlock::Text("[stopped]".to_string())],
        ),
        msg(ConversationRole::User, vec![tool_result_block("C")]),
    ];

    let fixed = enforce_tool_result_pairing(messages);

    for (i, m) in fixed.iter().enumerate() {
        if m.role == ConversationRole::User && i > 0 {
            let result_count = m
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::ToolResult(_)))
                .count();
            let prev_use_count = fixed[i - 1]
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::ToolUse(_)))
                .count();
            assert!(
                result_count <= prev_use_count,
                "turn {i}: {result_count} toolResults > {prev_use_count} toolUses"
            );
        }
    }

    let first_user = &fixed[1];
    let first_user_ids: Vec<&str> = first_user
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolResult(r) => Some(r.tool_use_id.as_str()),
            _ => None,
        })
        .collect();
    assert!(first_user_ids.contains(&"A"));
    assert!(first_user_ids.contains(&"B"));
    assert_eq!(first_user_ids.len(), 2);
}

#[test]
fn enforce_strips_results_after_no_tooluse_assistant() {
    let messages = vec![
        msg(
            ConversationRole::Assistant,
            vec![tool_use_block("X", "bash")],
        ),
        msg(ConversationRole::User, vec![tool_result_block("X")]),
        msg(
            ConversationRole::Assistant,
            vec![ContentBlock::Text("[stopped]".to_string())],
        ),
        msg(ConversationRole::User, vec![tool_result_block("Y")]),
    ];

    let fixed = enforce_tool_result_pairing(messages);

    assert_eq!(
        fixed.len(),
        3,
        "orphaned user turn should be removed; got {:?}",
        fixed.iter().map(|m| &m.role).collect::<Vec<_>>()
    );
    assert_eq!(fixed[2].role, ConversationRole::Assistant);
}

#[test]
fn enforce_merges_adjacent_assistants_after_orphan_removal() {
    let messages = vec![
        msg(
            ConversationRole::Assistant,
            vec![tool_use_block("A", "bash")],
        ),
        msg(ConversationRole::User, vec![tool_result_block("A")]),
        msg(
            ConversationRole::Assistant,
            vec![ContentBlock::Text("[stopped: interrupted]".to_string())],
        ),
        msg(ConversationRole::User, vec![tool_result_block("B")]),
        msg(
            ConversationRole::Assistant,
            vec![ContentBlock::Text("[stopped: interrupted]".to_string())],
        ),
        msg(
            ConversationRole::User,
            vec![ContentBlock::Text("continue".to_string())],
        ),
    ];

    let fixed = enforce_tool_result_pairing(messages);

    for i in 1..fixed.len() {
        assert_ne!(
            fixed[i].role,
            fixed[i - 1].role,
            "alternation violated at index {i}: {:?} == {:?}",
            fixed[i].role,
            fixed[i - 1].role
        );
    }
    let assistant_texts: Vec<&str> = fixed
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::Text(t) if t.contains("interrupted") => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        assistant_texts.len(),
        2,
        "both interrupted texts should be in the merged assistant"
    );
}

/// A user-role mode-switch marker stays IN-POSITION in the Converse API
/// conversation flow, unlike a system-role message which gets lifted into the
/// flat system parameter (#848).
#[test]
fn user_role_mode_switch_stays_in_conversation_position() {
    let wire = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("## Mode: Act\n\nYou are in Act mode.".into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            attachments: Vec::new(),
            reasoning: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("I'm in Plan mode. Please switch to Act.".into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            attachments: Vec::new(),
            reasoning: None,
        },
        // This is the fix: Role::User instead of Role::System
        ChatMessage {
            role: "user".into(),
            content: Some("[system: Mode switched to Act. Full tool access enabled.]".into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            attachments: Vec::new(),
            reasoning: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("Go".into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            attachments: Vec::new(),
            reasoning: None,
        },
    ];

    let (system_blocks, messages) = to_converse(&wire);

    // Only the actual system prompt is in system blocks (not the mode-switch marker).
    assert_eq!(system_blocks.len(), 1);

    // The conversation has: assistant + merged-user (mode-switch + "Go").
    // The mode-switch marker is IN the conversation flow, not lifted out.
    assert_eq!(messages.len(), 2); // assistant, then merged user
    let user_content: Vec<&str> = messages[1]
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    // Both user messages merged: mode-switch marker + "Go"
    assert_eq!(user_content.len(), 2);
    assert!(user_content[0].contains("Mode switched to Act"));
    assert_eq!(user_content[1], "Go");
}

#[test]
fn messages_have_tool_blocks_detects_tool_use() {
    let messages = vec![
        Message::builder()
            .role(ConversationRole::User)
            .content(ContentBlock::Text("hello".to_string()))
            .build()
            .unwrap(),
        Message::builder()
            .role(ConversationRole::Assistant)
            .content(ContentBlock::ToolUse(
                ToolUseBlock::builder()
                    .tool_use_id("call_1")
                    .name("bash")
                    .input(Document::Object(std::collections::HashMap::new()))
                    .build()
                    .unwrap(),
            ))
            .build()
            .unwrap(),
    ];
    assert!(messages_have_tool_blocks(&messages));
}

#[test]
fn messages_have_tool_blocks_false_for_text_only() {
    let messages = vec![
        Message::builder()
            .role(ConversationRole::User)
            .content(ContentBlock::Text("hello".to_string()))
            .build()
            .unwrap(),
        Message::builder()
            .role(ConversationRole::Assistant)
            .content(ContentBlock::Text("hi there".to_string()))
            .build()
            .unwrap(),
    ];
    assert!(!messages_have_tool_blocks(&messages));
}

#[test]
fn noop_tool_config_builds_successfully() {
    let cfg = noop_tool_config();
    assert!(cfg.is_some(), "noop_tool_config should build");
    let cfg = cfg.unwrap();
    let tools = cfg.tools();
    assert_eq!(tools.len(), 1);
    match &tools[0] {
        Tool::ToolSpec(spec) => {
            assert_eq!(spec.name(), "_noop");
            // Schema must be {"type":"object","properties":{}} — not bare {}.
            // Bedrock rejects empty schemas (normalize_object_schema documents this).
            if let Some(ToolInputSchema::Json(doc)) = spec.input_schema() {
                if let Document::Object(map) = doc {
                    assert!(
                        map.contains_key("type"),
                        "schema must contain 'type' key; got: {map:?}"
                    );
                } else {
                    panic!("expected Object document");
                }
            } else {
                panic!("expected Some(Json) input schema");
            }
        }
        _ => panic!("expected ToolSpec"),
    }
}
