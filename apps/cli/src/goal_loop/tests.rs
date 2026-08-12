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
    assert!(!state.ledger.completed());

    state.handle_event(&AgentEvent::ToolCallFinished {
        message_id: "m1".into(),
        call_id: call_id.clone(),
        success: true,
        result: "done".into(),
        observer_intent: None,
    });
    assert!(state.ledger.completed());
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

/// The CLI had two divergent copies of `build_registry_with_memory` — `main.rs`'s
/// (used by `run`/`chat`/`serve`) and `goal_loop.rs`'s — and they drifted: goal mode
/// was missing PubMed search and all three memory tools, while `run` was missing
/// `goal_complete`. Divergence here is invisible at compile time and shows up only as
/// "why can't the agent recall anything in goal mode", so pin the two toolsets as
/// equal rather than trusting that a future edit touches both copies (#1207).
///
/// With the copies merged there is no longer a second function to compare against, so
/// the guard is now on the *contents* of the single seam: it asserts the union that the
/// merge produced. Comparing the seam to itself would pass unconditionally and pin
/// nothing — the union is what a future edit could silently shrink.
#[tokio::test]
async fn the_shared_registry_seam_carries_every_previously_forked_tool() {
    let (registry, _memory, index) = crate::build_registry_with_memory().await;

    // `goal_complete` was present only in goal mode's copy; `pubmed_search` only in
    // `main.rs`'s. Every CLI path must now see both.
    for tool in ["goal_complete", "pubmed_search", "web_search"] {
        assert!(
            registry.get(tool).is_some(),
            "the shared registry seam is missing `{tool}`; before #1207 the CLI had two \
             divergent copies and each was missing some of these"
        );
    }

    // The memory trio is registered only when the FTS5 index opens, so it is asserted
    // against that condition rather than unconditionally — an unconditional assert would
    // fail in a sandbox with no writable memory root, which is a harness artefact and not
    // the fork this test guards.
    if index.is_some() {
        for tool in ["memory_search", "memory_get", "memory_write"] {
            assert!(
                registry.get(tool).is_some(),
                "the index opened, so `{tool}` must be registered; goal mode's old \
                 registry copy omitted the memory trio entirely"
            );
        }
    }
}

/// The observation state machine now lives in `ff_agent::TurnLedger` and is
/// unit-tested there (#1226). The CLI's remaining responsibility is *routing*:
/// `handle_event` must forward tool-call events to the shared ledger rather than
/// swallow them. Pin that seam with one collected step; the detailed
/// commit/discard/interleave behaviour is ff-agent's to guard.
#[test]
fn handle_event_routes_goal_steps_to_the_shared_ledger() {
    let mut state = IterationState::new();
    state.handle_event(&AgentEvent::ToolCallStarted {
        message_id: "m1".into(),
        call_id: "c1".into(),
        name: ff_tools::GOAL_STEP_TOOL_NAME.into(),
        args: serde_json::json!({ "claim": "ran the suite", "verdict": "match" }),
    });
    state.handle_event(&AgentEvent::ToolCallFinished {
        message_id: "m1".into(),
        call_id: "c1".into(),
        success: true,
        result: String::new(),
        observer_intent: None,
    });

    let steps = state.ledger.into_steps();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].claim, "ran the suite");
}
