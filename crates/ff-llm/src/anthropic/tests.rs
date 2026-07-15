use super::*;
use crate::{FunctionCall, ToolCall};

fn tool_call(id: &str, name: &str, args: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        kind: "function".into(),
        function: FunctionCall {
            name: name.into(),
            arguments: args.into(),
        },
    }
}

fn assistant_with_calls(calls: Vec<ToolCall>) -> ChatMessage {
    ChatMessage {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(calls),
        tool_call_id: None,
        name: None,

        attachments: Vec::new(),
        reasoning: None,
    }
}

fn tool_msg(id: &str, content: &str) -> ChatMessage {
    ChatMessage {
        role: "tool".into(),
        content: Some(content.into()),
        tool_calls: None,
        tool_call_id: Some(id.into()),
        name: None,

        attachments: Vec::new(),
        reasoning: None,
    }
}

// --- history -> messages translation ------------------------------------

#[test]
fn system_messages_become_system_string() {
    let msgs = vec![
        ChatMessage::text("system", "be brief"),
        ChatMessage::text("user", "hi"),
    ];
    let (system, out) = to_anthropic_messages(&msgs);
    assert_eq!(system, vec!["be brief"]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["role"], "user");
    assert_eq!(out[0]["content"][0]["text"], "hi");
}

#[test]
fn user_summary_stays_in_messages_not_hoisted() {
    // A compaction summary uses role=user precisely so it keeps its
    // chronological slot: only system-role messages are hoisted into the
    // top-level system param, which would tear it before the recent tail.
    let msgs = vec![
        ChatMessage::text("system", "be brief"),
        ChatMessage::text("user", "Summary of 40 earlier messages"),
        ChatMessage::text("assistant", "recent verbatim reply"),
    ];
    let (system, out) = to_anthropic_messages(&msgs);
    assert_eq!(system, vec!["be brief"]);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0]["role"], "user");
    assert_eq!(
        out[0]["content"][0]["text"],
        "Summary of 40 earlier messages"
    );
    assert_eq!(out[1]["role"], "assistant");
    assert_eq!(out[1]["content"][0]["text"], "recent verbatim reply");
}

#[test]
fn multiple_system_messages_join() {
    let msgs = vec![
        ChatMessage::text("system", "a"),
        ChatMessage::text("system", "b"),
        ChatMessage::text("user", "hi"),
    ];
    let (system, _) = to_anthropic_messages(&msgs);
    assert_eq!(system, vec!["a", "b"]);
}

#[test]
fn consecutive_same_role_messages_merge() {
    let msgs = vec![
        ChatMessage::text("user", "one"),
        ChatMessage::text("user", "two"),
    ];
    let (_, out) = to_anthropic_messages(&msgs);
    assert_eq!(out.len(), 1);
    let content = out[0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["text"], "one");
    assert_eq!(content[1]["text"], "two");
}

#[test]
fn tool_role_maps_to_user_tool_result() {
    let (_, out) = to_anthropic_messages(&[tool_msg("toolu_1", "42")]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["role"], "user");
    let block = &out[0]["content"][0];
    assert_eq!(block["type"], "tool_result");
    assert_eq!(block["tool_use_id"], "toolu_1");
    assert_eq!(block["content"], "42");
}

#[test]
fn tool_message_without_id_is_skipped() {
    let msg = ChatMessage {
        role: "tool".into(),
        content: Some("orphan".into()),
        tool_calls: None,
        tool_call_id: None,
        name: None,

        attachments: Vec::new(),
        reasoning: None,
    };
    let (_, out) = to_anthropic_messages(&[msg]);
    assert!(out.is_empty());
}

#[test]
fn assistant_tool_calls_become_tool_use_blocks() {
    let msg = ChatMessage {
        role: "assistant".into(),
        content: Some("let me check".into()),
        tool_calls: Some(vec![tool_call(
            "toolu_1",
            "get_weather",
            r#"{"location":"SF"}"#,
        )]),
        tool_call_id: None,
        name: None,

        attachments: Vec::new(),
        reasoning: None,
    };
    let (_, out) = to_anthropic_messages(&[msg]);
    let content = out[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "let me check");
    assert_eq!(content[1]["type"], "tool_use");
    assert_eq!(content[1]["id"], "toolu_1");
    assert_eq!(content[1]["name"], "get_weather");
    assert_eq!(content[1]["input"]["location"], "SF");
}

#[test]
fn empty_args_tool_use_becomes_empty_object() {
    let (_, out) =
        to_anthropic_messages(&[assistant_with_calls(vec![tool_call("toolu_1", "noop", "")])]);
    assert_eq!(out[0]["content"][0]["input"], json!({}));
}

#[test]
fn invalid_args_tool_use_becomes_empty_object() {
    let (_, out) = to_anthropic_messages(&[assistant_with_calls(vec![tool_call(
        "toolu_1", "noop", "not json",
    )])]);
    assert_eq!(out[0]["content"][0]["input"], json!({}));
}

#[test]
fn null_args_tool_use_becomes_empty_object() {
    let (_, out) = to_anthropic_messages(&[assistant_with_calls(vec![tool_call(
        "toolu_1", "noop", "null",
    )])]);
    assert_eq!(out[0]["content"][0]["input"], json!({}));
}

#[test]
fn object_args_are_preserved() {
    let (_, out) = to_anthropic_messages(&[assistant_with_calls(vec![tool_call(
        "toolu_1",
        "f",
        r#"{"a":1,"b":"x"}"#,
    )])]);
    assert_eq!(out[0]["content"][0]["input"], json!({"a":1,"b":"x"}));
}

// --- tool_use / tool_result pairing repair ------------------------------

#[test]
fn dangling_tool_use_gets_synthetic_result() {
    let msgs = vec![
        assistant_with_calls(vec![tool_call("toolu_1", "f", "{}")]),
        ChatMessage::text("user", "next"),
    ];
    let (_, out) = to_anthropic_messages(&msgs);
    assert_eq!(out.len(), 2);
    let user = out[1]["content"].as_array().unwrap();
    assert_eq!(user[0]["type"], "tool_result");
    assert_eq!(user[0]["tool_use_id"], "toolu_1");
    assert_eq!(user[1]["text"], "next");
}

#[test]
fn trailing_tool_use_gets_synthetic_result() {
    let (_, out) =
        to_anthropic_messages(&[assistant_with_calls(vec![tool_call("toolu_1", "f", "{}")])]);
    assert_eq!(out.len(), 2);
    assert_eq!(out[1]["role"], "user");
    assert_eq!(out[1]["content"][0]["type"], "tool_result");
    assert_eq!(out[1]["content"][0]["tool_use_id"], "toolu_1");
}

#[test]
fn well_formed_pairing_is_unchanged() {
    let msgs = vec![
        assistant_with_calls(vec![tool_call("toolu_1", "f", "{}")]),
        tool_msg("toolu_1", "ok"),
    ];
    let (_, out) = to_anthropic_messages(&msgs);
    assert_eq!(out.len(), 2);
    let user = out[1]["content"].as_array().unwrap();
    assert_eq!(user.len(), 1);
    assert_eq!(user[0]["content"], "ok");
}

#[test]
fn partial_tool_results_backfilled() {
    let msgs = vec![
        assistant_with_calls(vec![
            tool_call("toolu_1", "f", "{}"),
            tool_call("toolu_2", "g", "{}"),
        ]),
        tool_msg("toolu_1", "ok"),
    ];
    let (_, out) = to_anthropic_messages(&msgs);
    let user = out[1]["content"].as_array().unwrap();
    assert_eq!(user.len(), 2);
    let ids: Vec<&str> = user
        .iter()
        .map(|b| b["tool_use_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"toolu_1"));
    assert!(ids.contains(&"toolu_2"));
}

// --- tool schema mapping ------------------------------------------------

#[test]
fn tools_built_from_openai_spec() {
    let tools = vec![json!({
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "weather",
            "parameters": { "type": "object", "properties": { "q": { "type": "string" } } }
        }
    })];
    let out = to_anthropic_tools(&tools).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["name"], "get_weather");
    assert_eq!(out[0]["description"], "weather");
    assert_eq!(out[0]["input_schema"]["type"], "object");
    assert_eq!(out[0]["input_schema"]["properties"]["q"]["type"], "string");
}

#[test]
fn no_tools_yields_none() {
    assert!(to_anthropic_tools(&[]).is_none());
}

#[test]
fn schema_missing_type_gets_object_injected() {
    let tools = vec![json!({
        "function": { "name": "f", "parameters": { "properties": { "q": { "type": "string" } } } }
    })];
    let out = to_anthropic_tools(&tools).unwrap();
    assert_eq!(out[0]["input_schema"]["type"], "object");
    assert_eq!(out[0]["input_schema"]["properties"]["q"]["type"], "string");
}

#[test]
fn empty_or_missing_schema_becomes_object() {
    let tools = vec![json!({ "function": { "name": "f" } })];
    let out = to_anthropic_tools(&tools).unwrap();
    assert_eq!(
        out[0]["input_schema"],
        json!({ "type": "object", "properties": {} })
    );
}

// --- SSE event -> Chunk -------------------------------------------------

#[test]
fn text_delta_maps_to_chunk() {
    let line = br#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
    let chunk = parse_anthropic_line(line).unwrap().unwrap();
    assert_eq!(chunk.delta, "Hello");
    assert!(!chunk.done);
}

#[test]
fn thinking_delta_maps_to_reasoning() {
    let line = br#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#;
    let chunk = parse_anthropic_line(line).unwrap().unwrap();
    assert_eq!(chunk.reasoning_delta, "hmm");
}

#[test]
fn tool_use_start_then_input_json_delta_preserves_args() {
    let start = br#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}"#;
    let chunk = parse_anthropic_line(start).unwrap().unwrap();
    assert_eq!(chunk.tool_calls.len(), 1);
    let tc = &chunk.tool_calls[0];
    assert_eq!(tc.index, 1);
    assert_eq!(tc.id.as_deref(), Some("toolu_1"));
    assert_eq!(tc.name.as_deref(), Some("get_weather"));
    assert_eq!(tc.arguments, "");

    let delta = br#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"location\":"}}"#;
    let chunk = parse_anthropic_line(delta).unwrap().unwrap();
    let tc = &chunk.tool_calls[0];
    assert_eq!(tc.index, 1);
    assert!(tc.id.is_none());
    assert!(tc.arguments.contains("location"));
}

#[test]
fn message_stop_marks_done() {
    let chunk = parse_anthropic_line(br#"data: {"type":"message_stop"}"#)
        .unwrap()
        .unwrap();
    assert!(chunk.done);
}

#[test]
fn signature_delta_is_ignored() {
    let line = br#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc"}}"#;
    assert!(parse_anthropic_line(line).is_none());
}

#[test]
fn payload_carrying_events_yield_nothing() {
    assert!(parse_anthropic_line(br#"data: {"type":"ping"}"#).is_none());
    assert!(parse_anthropic_line(
        br#"data: {"type":"message_start","message":{"id":"m","content":[]}}"#
    )
    .is_none());
    assert!(parse_anthropic_line(br#"data: {"type":"content_block_stop","index":0}"#).is_none());
    assert!(parse_anthropic_line(
        br#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#
    )
        .is_none());
}

#[test]
fn non_data_and_blank_lines_skipped() {
    assert!(parse_anthropic_line(b"").is_none());
    assert!(parse_anthropic_line(b"event: message_stop").is_none());
    assert!(parse_anthropic_line(b": ping comment").is_none());
}

#[test]
fn error_event_maps_to_api_error() {
    let line =
        br#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
    let err = parse_anthropic_line(line).unwrap().unwrap_err();
    match err {
        LlmError::Api { message, .. } => assert_eq!(message, "Overloaded"),
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[test]
fn malformed_json_is_decode_error() {
    let err = parse_anthropic_line(br#"data: {not json}"#)
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, LlmError::Decode(_)));
}

// --- request body -------------------------------------------------------

#[test]
fn request_includes_required_max_tokens() {
    let req = ChatRequest {
        model: "claude-x".into(),
        messages: vec![ChatMessage::text("user", "hi")],
        tools: vec![],
        thinking: false,
        max_tokens: None,
        cache_messages: false,
    };
    let body = to_anthropic_request(&req, 4096, ReasoningEffort::Medium);
    assert_eq!(body["max_tokens"], 4096);
    assert_eq!(body["model"], "claude-x");
    assert_eq!(body["stream"], true);
    assert!(body.get("thinking").is_none());
    assert!(body.get("system").is_none());
}

#[test]
fn thinking_request_sets_budget_below_max_tokens() {
    let req = ChatRequest {
        model: "claude-x".into(),
        messages: vec![ChatMessage::text("user", "hi")],
        tools: vec![],
        thinking: true,
        max_tokens: None,
        cache_messages: false,
    };
    // Medium budget is 4096; with the default 4096 cap, max_tokens is bumped
    // so budget stays strictly below it (Anthropic requirement).
    let body = to_anthropic_request(&req, 4096, ReasoningEffort::Medium);
    assert_eq!(body["thinking"]["type"], "enabled");
    let budget = body["thinking"]["budget_tokens"].as_u64().unwrap();
    let max_tokens = body["max_tokens"].as_u64().unwrap();
    assert_eq!(budget, 4096);
    assert!(
        budget < max_tokens,
        "budget {budget} !< max_tokens {max_tokens}"
    );
}

#[test]
fn thinking_budget_scales_with_effort() {
    let req = ChatRequest {
        model: "claude-x".into(),
        messages: vec![ChatMessage::text("user", "hi")],
        tools: vec![],
        thinking: true,
        max_tokens: None,
        cache_messages: false,
    };
    // Budgets are uniform and concrete regardless of max_tokens.
    let low = to_anthropic_request(&req, 32000, ReasoningEffort::Low);
    assert_eq!(low["thinking"]["budget_tokens"], 1024);
    let med = to_anthropic_request(&req, 32000, ReasoningEffort::Medium);
    assert_eq!(med["thinking"]["budget_tokens"], 4096);
    let high = to_anthropic_request(&req, 32000, ReasoningEffort::High);
    assert_eq!(high["thinking"]["budget_tokens"], 8192);
    // A generous cap is left untouched (only bumped when too low).
    assert_eq!(high["max_tokens"], 32000);
}

/// #395 acceptance: the provider's private `reasoning_effort` dial (set via
/// `with_reasoning_effort`) must reach the emitted Anthropic wire body, not
/// just `to_anthropic_request`'s return value when the effort is passed
/// directly.  High → `thinking.budget_tokens = 8192`.
#[test]
fn high_effort_provider_emits_8192_thinking_budget() {
    let req = ChatRequest {
        model: "claude-x".into(),
        messages: vec![ChatMessage::text("user", "hi")],
        tools: vec![],
        thinking: true,
        max_tokens: None,
        cache_messages: false,
    };
    let provider = AnthropicProvider::new("sk-ant-test")
        .with_max_tokens(32000)
        .with_reasoning_effort(ReasoningEffort::High);

    // The effort comes from the provider's private field — not passed
    // explicitly. This proves `chat_stream`'s code path threads the dial.
    let body = provider.emitted_body_for(&req);
    assert_eq!(body["thinking"]["budget_tokens"], 8192);
}

#[test]
fn request_hoists_system_and_tools() {
    let req = ChatRequest {
        model: "claude-x".into(),
        messages: vec![
            ChatMessage::text("system", "sys"),
            ChatMessage::text("user", "hi"),
        ],
        tools: vec![
            json!({"function":{"name":"f","parameters":{"type":"object","properties":{}}}}),
        ],
        thinking: false,
        max_tokens: None,
        cache_messages: false,
    };
    let body = to_anthropic_request(&req, 100, ReasoningEffort::Medium);
    // System is now a block array with a cache breakpoint (#437).
    assert_eq!(body["system"][0]["text"], "sys");
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["tools"][0]["name"], "f");
}

// #437: the system prefix and the *last* tool each carry a cache breakpoint,
// so the stable tools+system prefix is cached from turn 2 onward.
#[test]
fn system_and_last_tool_carry_cache_control() {
    let req = ChatRequest {
        model: "claude-x".into(),
        messages: vec![ChatMessage::text("system", "sys")],
        tools: vec![
            json!({"function":{"name":"a","parameters":{"type":"object","properties":{}}}}),
            json!({"function":{"name":"b","parameters":{"type":"object","properties":{}}}}),
        ],
        thinking: false,
        max_tokens: None,
        cache_messages: false,
    };
    let body = to_anthropic_request(&req, 100, ReasoningEffort::Medium);
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    // Only the last tool gets the breakpoint; the first stays plain.
    assert!(body["tools"][0].get("cache_control").is_none());
    assert_eq!(body["tools"][1]["name"], "b");
    assert_eq!(body["tools"][1]["cache_control"]["type"], "ephemeral");
}

// --- message-level caching (#763) ----------------------------------------

#[test]
fn cache_messages_marks_penultimate_and_first() {
    let req = ChatRequest {
        model: "claude-x".into(),
        messages: vec![
            ChatMessage::text("user", "turn 1"),
            ChatMessage::text("assistant", "reply 1"),
            ChatMessage::text("user", "turn 2"),
            ChatMessage::text("assistant", "reply 2"),
            ChatMessage::text("user", "turn 3"),
        ],
        tools: vec![],
        thinking: false,
        max_tokens: None,
        cache_messages: true,
    };
    let body = to_anthropic_request(&req, 4096, ReasoningEffort::Medium);
    let msgs = body["messages"].as_array().unwrap();
    // 5 messages -> penultimate is index 3, and index 0 gets a breakpoint (len >= 4).
    assert_eq!(
        msgs[3]["content"][0]["cache_control"]["type"], "ephemeral",
        "penultimate message should have cache_control"
    );
    assert_eq!(
        msgs[0]["content"][0]["cache_control"]["type"], "ephemeral",
        "first message should have cache_control when len >= 4"
    );
    // Middle messages should NOT have cache_control.
    assert!(msgs[1]["content"][0].get("cache_control").is_none());
    assert!(msgs[2]["content"][0].get("cache_control").is_none());
}

#[test]
fn cache_messages_short_history_only_marks_penultimate() {
    let req = ChatRequest {
        model: "claude-x".into(),
        messages: vec![
            ChatMessage::text("user", "turn 1"),
            ChatMessage::text("assistant", "reply 1"),
            ChatMessage::text("user", "turn 2"),
        ],
        tools: vec![],
        thinking: false,
        max_tokens: None,
        cache_messages: true,
    };
    let body = to_anthropic_request(&req, 4096, ReasoningEffort::Medium);
    let msgs = body["messages"].as_array().unwrap();
    // 3 messages -> penultimate is index 1; index 0 NOT marked (len < 4).
    assert_eq!(
        msgs[1]["content"][0]["cache_control"]["type"], "ephemeral",
        "penultimate message should have cache_control"
    );
    assert!(
        msgs[0]["content"][0].get("cache_control").is_none(),
        "first message should NOT have cache_control when len < 4"
    );
}

#[test]
fn cache_messages_false_leaves_messages_unmarked() {
    let req = ChatRequest {
        model: "claude-x".into(),
        messages: vec![
            ChatMessage::text("user", "turn 1"),
            ChatMessage::text("assistant", "reply 1"),
            ChatMessage::text("user", "turn 2"),
            ChatMessage::text("assistant", "reply 2"),
            ChatMessage::text("user", "turn 3"),
        ],
        tools: vec![],
        thinking: false,
        max_tokens: None,
        cache_messages: false,
    };
    let body = to_anthropic_request(&req, 4096, ReasoningEffort::Medium);
    let msgs = body["messages"].as_array().unwrap();
    for msg in msgs {
        assert!(
            msg["content"][0].get("cache_control").is_none(),
            "no message should have cache_control when disabled"
        );
    }
}

// --- creds --------------------------------------------------------------

#[test]
fn debug_redacts_api_key() {
    let p = AnthropicProvider::new("sk-ant-secret");
    let dbg = format!("{p:?}");
    assert!(!dbg.contains("sk-ant-secret"));
    assert!(dbg.contains("<redacted>"));
}
