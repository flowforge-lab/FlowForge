use super::*;
use ff_agent::AgentEvent;

#[test]
fn iteration_state_detects_goal_complete_from_events() {
    let mut state = IterationState::new();
    let call_id = "gc-1".to_string();

    state.handle_event(&AgentEvent::ToolCallStarted {
        message_id: "m1".into(),
        call_id: call_id.clone(),
        name: ff_tools::GOAL_COMPLETE_TOOL_NAME.into(),
        args: serde_json::json!({}),
    });
    assert!(!state.completed);

    state.handle_event(&AgentEvent::ToolCallFinished {
        message_id: "m1".into(),
        call_id: call_id.clone(),
        success: true,
        result: "done".into(),
        observer_intent: None,
    });
    assert!(state.completed);
}

#[test]
fn iteration_state_counts_tokens_from_done() {
    let mut state = IterationState::new();
    state.handle_event(&AgentEvent::Done {
        message_id: "m1".into(),
        final_message: None,
        stop_reason: None,
        turns: None,
        token_count: Some(42),
        prefill_estimates: None,
        prompt_latency_ms: None,
        tier2_ms: None,
        tier1_fires: None,
        tier2_fires: None,
        retrieve_calls: None,
        cache_hit_tokens: None,
        cache_miss_tokens: None,
        breakdown: None,
        usage: None,
        budget_tokens: None,
    });
    assert_eq!(state.tokens, 42);
}

#[test]
fn iteration_state_flags_cancelled_on_error_message() {
    let mut state = IterationState::new();
    state.handle_event(&AgentEvent::Error {
        message: "turn was cancelled by user".into(),
    });
    assert!(state.cancelled);
}

#[test]
fn goal_continue_nudge_does_not_inline_the_objective() {
    assert!(
        GOAL_CONTINUE_NUDGE.contains("goal_complete"),
        "nudge should still point at the completion tool"
    );
    assert!(
        GOAL_CONTINUE_NUDGE
            .to_lowercase()
            .contains("continue toward the goal"),
        "nudge should be a neutral continue"
    );
    assert!(
        GOAL_CONTINUE_NUDGE.contains("described in your instructions"),
        "nudge must defer to the system-prompt goal block for the objective"
    );
    assert!(
        !GOAL_CONTINUE_NUDGE.contains("{}") && !GOAL_CONTINUE_NUDGE.contains("{0}"),
        "nudge must be a static string, not an objective-interpolating format"
    );
}
