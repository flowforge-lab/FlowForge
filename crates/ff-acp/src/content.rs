//! `AgentEvent` ↔ `wire::SessionUpdate`, plus the stop-reason mapping.
//!
//! Both directions live here because they are inverses of one per-variant
//! decision, and owning both in `ff-acp` stops #1201 from rewriting the inverse
//! later:
//!
//! - **outbound** ([`AgentEvent`](ff_agent::AgentEvent) → [`wire::SessionUpdate`]):
//!   the *server* streaming path (#1201 — FlowForge-as-agent emitting updates
//!   to an ACP client). Built now, consumed by #1201.
//! - **inbound** ([`wire::SessionUpdate`] → [`Inbound`]): the *client* path
//!   (#1202 — FlowForge-as-client surfacing an external agent's stream in a UI
//!   built around [`AgentEvent`](ff_agent::AgentEvent)). Consumed by
//!   [`crate::client`].
//!
//! # The decisions this module encodes
//!
//! Each is somewhere a future reader would otherwise "fix" into a bug, so the
//! reasoning lives at the call site rather than only here.
//!
//! - **`Done` carries no update.** ACP ends a turn with the `session/prompt`
//!   *response* and its `stopReason`, not a `session/update`. Emitting a final
//!   update *and* returning a stop reason would double-signal completion.
//!   [`outbound`] returns `None` for `Done`; the response builder assembles the
//!   stop reason via [`outbound_stop_reason`].
//! - **Housekeeping events become `AgentMessageChunk` notices, not silent
//!   drops and not `_meta` keys.** `MemoryFlushed` / `AttachmentsDropped` /
//!   `EgressMismatch` / `Error` / `Reconnecting` / `ConnectionFailed` carry
//!   user-visible information; dropping them silently loses it (attachments
//!   *disappeared*), and inventing `_meta` keys is non-standard. A text notice
//!   with a provenance prefix is the honest representation.
//! - **`Error` / `ConnectionFailed` are content, not JSON-RPC errors.** ACP
//!   distinguishes a *protocol* error (the JSON-RPC `error` response, which
//!   aborts the turn) from *content the user should see*. These read like the
//!   former but behave like the latter: FlowForge surfaces the message and
//!   continues (or ends the turn via the response), where a JSON-RPC error
//!   would have aborted. So they map to an `AgentMessageChunk` notice, and only
//!   the *response* builder turns a turn-ending failure into the protocol
//!   error.
//! - **ACP-only updates with no FlowForge producer stay in `ff-acp`, not in
//!   `AgentEvent`.** `CurrentModeUpdate` / `Plan` / `UsageUpdate` / etc. have
//!   no [`AgentEvent`] counterpart. Rather than pollute
//!   [`AgentEvent`](ff_agent::AgentEvent) with ACP-specific variants — the
//!   desktop's main emit match is exhaustive
//!   (`apps/desktop/src-tauri/src/lib.rs` ~L3785), so adding a variant is a
//!   desktop-source mutation per `AGENTS.md` for a feature with no current
//!   desktop consumer — they surface as [`Inbound::ModeChanged`] (the one a
//!   client pane genuinely needs to react to) or [`Inbound::Ignored`] (the
//!   rest), keeping ACP vocabulary in `ff-acp` where it belongs.
//! - **`Reasoning` is a first-class thought chunk**, not folded into message
//!   text — ACP models thinking as its own chunk kind, and folding it would
//!   lose the distinction.
//!
//! # Fixture discipline (#1215 lesson carries over)
//!
//! `wire::SessionUpdate` is `#[serde(tag = "sessionUpdate", rename_all =
//! "snake_case")]` — the exact shape #1200 got subtly wrong in a way
//! self-built fixtures cannot catch. Every test here builds a fixture from the
//! **official** `wire::` type, round-trips it through the **official**
//! serializer/parser, asserts the wire-visible tag, then maps — never builds
//! JSON from [`AgentEvent`](ff_agent::AgentEvent). JSON generated from our own
//! types is self-satisfying however much verbatim text is involved.

use crate::wire;
use ff_agent::AgentEvent;
use ff_core::StopReason;

/// The FlowForge-side surface for a single inbound `session/update`.
///
/// One ACP update maps to one of these. Natural counterparts become
/// [`AgentEvent`]; ACP-only updates surface as dedicated variants so the host
/// can react without [`AgentEvent`] growing ACP-specific vocabulary.
///
/// Deliberately not `PartialEq`: [`AgentEvent`] isn't `PartialEq` (a
/// streaming delta has no meaningful equality), and a `match` is the right way
/// to inspect an `Inbound` anyway.
///
/// `AgentEvent::Done` is large (many perf/counters `Option` fields), which
/// trips `clippy::large_enum_variant`. Boxing it would add a heap alloc per
/// streamed token for no benefit — an `Inbound` is a transient channel item,
/// moved once and consumed immediately, never copied or stored in arrays. The
/// allow is the right call here, not a redesign.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Inbound {
    /// A [`AgentEvent`] the host forwards to its existing streaming surface.
    Agent(AgentEvent),
    /// The external agent changed its mode. `mode_id` is opaque — the external
    /// agent's vocabulary may differ from FlowForge's [`Mode`](ff_core::Mode).
    ModeChanged { mode_id: String },
    /// An ACP update with no current FlowForge surface (`Plan`,
    /// `UsageUpdate`, `ConfigOptionUpdate`, `SessionInfoUpdate`,
    /// `AvailableCommandsUpdate`, `UserMessageChunk`, or a future variant).
    /// Surfaced as nothing today; revisit when the desktop grows a surface.
    Ignored,
}

/// Map an inbound `session/update` notification onto the FlowForge surface.
///
/// `fallback_message_id` is the session's current message id, used for chunks
/// and tool calls that don't carry their own — ACP leaves `messageId` optional
/// on a `ContentChunk`, and a `ToolCall` has no message id at all, but
/// [`AgentEvent`] keys every event on a message id. The client tracks the
/// current id (from the prompt response / first chunk) and supplies it here.
pub fn inbound(update: &wire::SessionUpdate, fallback_message_id: &str) -> Inbound {
    match update {
        wire::SessionUpdate::AgentMessageChunk(chunk) => {
            match text_of_chunk(chunk) {
                Some(delta) => Inbound::Agent(AgentEvent::Token {
                    message_id: message_id_of(chunk, fallback_message_id),
                    delta,
                }),
                None => Inbound::Ignored, // image / audio / resource block
            }
        }
        wire::SessionUpdate::AgentThoughtChunk(chunk) => match text_of_chunk(chunk) {
            Some(delta) => Inbound::Agent(AgentEvent::Reasoning {
                message_id: message_id_of(chunk, fallback_message_id),
                delta,
            }),
            None => Inbound::Ignored,
        },
        // The agent echoing the user's own message back. We authored the
        // prompt, so this carries no new information for FlowForge's surface.
        wire::SessionUpdate::UserMessageChunk(_) => Inbound::Ignored,
        wire::SessionUpdate::ToolCall(tc) => Inbound::Agent(AgentEvent::ToolCallStarted {
            message_id: fallback_message_id.to_owned(),
            call_id: tc.tool_call_id.0.to_string(),
            name: tc.title.clone(),
            args: tc.raw_input.clone().unwrap_or(serde_json::Value::Null),
        }),
        wire::SessionUpdate::ToolCallUpdate(upd) => inbound_tool_update(upd, fallback_message_id),
        wire::SessionUpdate::CurrentModeUpdate(m) => Inbound::ModeChanged {
            mode_id: m.current_mode_id.0.to_string(),
        },
        // Plan, AvailableCommandsUpdate, ConfigOptionUpdate, SessionInfoUpdate,
        // UsageUpdate, and any future variant: no FlowForge surface today.
        _ => Inbound::Ignored,
    }
}

/// Map an outbound [`AgentEvent`] onto a `session/update` notification.
///
/// Returns `None` for events that carry no update — `Done` (the turn's
/// completion is the `session/prompt` *response*, not an update) and
/// housekeeping events that the response builder handles separately from the
/// stream. The stop reason lives on the response; see
/// [`outbound_stop_reason`].
pub fn outbound(event: &AgentEvent) -> Option<wire::SessionUpdate> {
    match event {
        AgentEvent::Token { message_id, delta } => Some(agent_chunk(message_id, delta.clone())),
        AgentEvent::Reasoning { message_id, delta } => {
            Some(thought_chunk(message_id, delta.clone()))
        }
        AgentEvent::ToolCallStarted {
            call_id,
            name,
            args,
            ..
        } => Some(tool_call_started(call_id, name, args)),
        AgentEvent::ToolCallFinished {
            call_id,
            success,
            result,
            ..
        } => Some(tool_call_finished(call_id, *success, result)),
        AgentEvent::ToolOutputChunk { call_id, delta, .. } => {
            Some(tool_output_chunk(call_id, delta))
        }
        AgentEvent::MemoryFlushed { message_id, writes } => Some(agent_notice(
            Some(message_id),
            format!("[memory] auto-wrote {writes} fact(s)"),
        )),
        AgentEvent::AttachmentsDropped {
            message_id,
            count,
            reason,
        } => Some(agent_notice(
            Some(message_id),
            match reason {
                Some(r) => format!("[attachments] {count} dropped: {r}"),
                None => format!("[attachments] {count} dropped"),
            },
        )),
        AgentEvent::EgressMismatch {
            message_id,
            kind,
            model,
        } => Some(agent_notice(
            Some(message_id),
            // `ProviderKind` carries no `Display`, so its Debug form is the
            // closest stable surface for a provenance notice.
            format!(
                "[egress] {kind:?} model {model} is hosted; prompt content may leave the machine"
            ),
        )),
        AgentEvent::Error { message } => Some(agent_notice(None, format!("[error] {message}"))),
        AgentEvent::Reconnecting {
            message_id,
            attempt,
            max_attempts,
        } => Some(agent_notice(
            Some(message_id),
            format!("[reconnecting] attempt {attempt}/{max_attempts}"),
        )),
        AgentEvent::ConnectionFailed {
            message_id,
            message,
        } => Some(agent_notice(
            Some(message_id),
            format!("[connection failed] {message}"),
        )),
        // Done terminates the stream; its stop reason becomes the prompt
        // response's `stopReason`, not an update. Double-signalling completion
        // (a final update *and* a stop reason) would be a bug.
        AgentEvent::Done { .. } => None,
    }
}

/// Map the ACP `stopReason` (on a `session/prompt` response) onto FlowForge's
/// internal [`StopReason`].
///
/// `None` does not mean "unknown" — it means the turn ended normally and
/// FlowForge has no internal stop-reason to record. `EndTurn` / `MaxTokens` /
/// `Refusal` are all "the agent finished" from our perspective; only
/// `Cancelled` and `MaxTurnRequests` carry an internal analogue.
pub fn inbound_stop_reason(reason: wire::StopReason) -> Option<StopReason> {
    match reason {
        wire::StopReason::Cancelled => Some(StopReason::Cancelled),
        wire::StopReason::MaxTurnRequests => Some(StopReason::ToolLimit),
        // EndTurn / MaxTokens / Refusal, and any future variant.
        _ => None,
    }
}

/// Map FlowForge's internal [`StopReason`] onto the ACP `stopReason` for a
/// `session/prompt` response. Used by #1201's response builder.
///
/// A `None` (normal completion) maps to `EndTurn`. `Interrupted` (app
/// shutdown / task cancellation) maps to `Cancelled` — the user-visible
/// outcome is the same. `Stall` / `EmptyResponse` / `MalformedToolCall` have
/// no precise ACP equivalent; `EndTurn` is the honest "the turn ended".
pub fn outbound_stop_reason(reason: Option<StopReason>) -> wire::StopReason {
    match reason {
        Some(StopReason::Cancelled) | Some(StopReason::Interrupted) => wire::StopReason::Cancelled,
        Some(StopReason::ToolLimit) => wire::StopReason::MaxTurnRequests,
        // EmptyResponse / Stall / MalformedToolCall / None.
        _ => wire::StopReason::EndTurn,
    }
}

// --- outbound helpers --------------------------------------------------------

fn agent_chunk(message_id: &str, delta: String) -> wire::SessionUpdate {
    wire::SessionUpdate::AgentMessageChunk(content_chunk(message_id, delta))
}

fn thought_chunk(message_id: &str, delta: String) -> wire::SessionUpdate {
    wire::SessionUpdate::AgentThoughtChunk(content_chunk(message_id, delta))
}

fn content_chunk(message_id: &str, delta: String) -> wire::ContentChunk {
    wire::ContentChunk::new(wire::ContentBlock::from(delta)).message_id(message_id)
}

fn tool_call_started(call_id: &str, name: &str, args: &serde_json::Value) -> wire::SessionUpdate {
    let mut tc = wire::ToolCall::new(wire::ToolCallId::new(call_id.to_owned()), name.to_owned())
        .status(wire::ToolCallStatus::InProgress);
    // A null `args` (the model called a no-arg tool) serialises to `null` on
    // the wire if set; leaving `raw_input` absent is cleaner and what the spec
    // expects for "no parameters".
    if !args.is_null() {
        tc = tc.raw_input(args.clone());
    }
    wire::SessionUpdate::ToolCall(tc)
}

fn tool_call_finished(call_id: &str, success: bool, result: &str) -> wire::SessionUpdate {
    let status = if success {
        wire::ToolCallStatus::Completed
    } else {
        wire::ToolCallStatus::Failed
    };
    let fields = wire::ToolCallUpdateFields::new()
        .status(status)
        .content(vec![text_toolcall_content(result.to_owned())]);
    wire::SessionUpdate::ToolCallUpdate(wire::ToolCallUpdate::new(
        wire::ToolCallId::new(call_id.to_owned()),
        fields,
    ))
}

fn tool_output_chunk(call_id: &str, delta: &str) -> wire::SessionUpdate {
    let fields = wire::ToolCallUpdateFields::new()
        .status(wire::ToolCallStatus::InProgress)
        .content(vec![text_toolcall_content(delta.to_owned())]);
    wire::SessionUpdate::ToolCallUpdate(wire::ToolCallUpdate::new(
        wire::ToolCallId::new(call_id.to_owned()),
        fields,
    ))
}

fn text_toolcall_content(text: String) -> wire::ToolCallContent {
    wire::ToolCallContent::from(wire::ContentBlock::from(text))
}

/// A `AgentMessageChunk` carrying a short provenance-marked notice, used for
/// housekeeping events (`MemoryFlushed`, `AttachmentsDropped`, …) that have no
/// dedicated ACP update kind. `message_id` is `None` for `Error`, which carries
/// no message id of its own — ACP leaves `messageId` optional on a chunk.
fn agent_notice(message_id: Option<&str>, text: String) -> wire::SessionUpdate {
    let mut chunk = wire::ContentChunk::new(wire::ContentBlock::from(text));
    if let Some(id) = message_id {
        chunk = chunk.message_id(id);
    }
    wire::SessionUpdate::AgentMessageChunk(chunk)
}

// --- inbound helpers ---------------------------------------------------------

fn inbound_tool_update(upd: &wire::ToolCallUpdate, fallback_message_id: &str) -> Inbound {
    let call_id = upd.tool_call_id.0.to_string();
    let text = text_of_toolcall_content(&upd.fields.content);
    match upd.fields.status {
        Some(wire::ToolCallStatus::Completed) => Inbound::Agent(AgentEvent::ToolCallFinished {
            message_id: fallback_message_id.to_owned(),
            call_id,
            success: true,
            result: text,
            observer_intent: None,
        }),
        Some(wire::ToolCallStatus::Failed) => Inbound::Agent(AgentEvent::ToolCallFinished {
            message_id: fallback_message_id.to_owned(),
            call_id,
            success: false,
            result: text,
            observer_intent: None,
        }),
        // InProgress / Pending / absent with streaming content: a live output
        // chunk. ACP doesn't distinguish stdout/stderr on tool-call content,
        // so the stream kind defaults to stdout.
        _ if !text.is_empty() => Inbound::Agent(AgentEvent::ToolOutputChunk {
            message_id: fallback_message_id.to_owned(),
            call_id,
            stream: ff_tools::OutputStream::Stdout,
            delta: text,
        }),
        _ => Inbound::Ignored,
    }
}

fn message_id_of(chunk: &wire::ContentChunk, fallback: &str) -> String {
    chunk
        .message_id
        .as_ref()
        .map(|m| m.0.to_string())
        .unwrap_or_else(|| fallback.to_owned())
}

fn text_of_chunk(chunk: &wire::ContentChunk) -> Option<String> {
    text_from_block(&chunk.content).map(str::to_owned)
}

fn text_from_block(block: &wire::ContentBlock) -> Option<&str> {
    match block {
        wire::ContentBlock::Text(t) => Some(t.text.as_str()),
        _ => None,
    }
}

fn text_of_toolcall_content(content: &Option<Vec<wire::ToolCallContent>>) -> String {
    let Some(items) = content else {
        return String::new();
    };
    let mut out = String::new();
    for item in items {
        if let wire::ToolCallContent::Content(c) = item {
            if let Some(t) = text_from_block(&c.content) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ff_core::StopReason as F;

    // Round-trip a fixture through the **official** serializer/parser before
    // mapping. This is the #1215 discipline: a fixture built from `wire::` is
    // schema-derived (wire *is* the schema), and the round-trip catches a
    // wrong serde attribute that a self-built JSON fixture could not.
    fn round_trip(update: &wire::SessionUpdate) -> wire::SessionUpdate {
        let bytes = serde_json::to_string(update).unwrap();
        serde_json::from_str(&bytes).unwrap()
    }

    fn wire_tag(update: &wire::SessionUpdate) -> String {
        serde_json::to_value(update).unwrap()["sessionUpdate"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn text_chunk(message_id: &str, delta: &str) -> wire::ContentChunk {
        wire::ContentChunk::new(wire::ContentBlock::from(delta.to_owned())).message_id(message_id)
    }

    fn done(message_id: &str, stop_reason: Option<StopReason>) -> AgentEvent {
        AgentEvent::Done {
            message_id: message_id.to_owned(),
            final_message: None,
            stop_reason,
            turns: None,
            token_count: None,
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
        }
    }

    fn tool_started(call_id: &str, name: &str, args: serde_json::Value) -> AgentEvent {
        AgentEvent::ToolCallStarted {
            message_id: "m".to_owned(),
            call_id: call_id.to_owned(),
            name: name.to_owned(),
            args,
        }
    }

    fn tool_finished(call_id: &str, success: bool, result: &str) -> AgentEvent {
        AgentEvent::ToolCallFinished {
            message_id: "m".to_owned(),
            call_id: call_id.to_owned(),
            success,
            result: result.to_owned(),
            observer_intent: None,
        }
    }

    // --- inbound: the wire-visible tags first, then the mapping -------------

    #[test]
    fn agent_message_chunk_round_trips_with_the_wire_tag() {
        let u = wire::SessionUpdate::AgentMessageChunk(text_chunk("m1", "hi"));
        assert_eq!(wire_tag(&u), "agent_message_chunk");
        assert!(matches!(
            inbound(&round_trip(&u), "fallback"),
            Inbound::Agent(AgentEvent::Token { delta, .. }) if delta == "hi"
        ));
    }

    #[test]
    fn agent_thought_chunk_maps_to_reasoning_not_message() {
        let u = wire::SessionUpdate::AgentThoughtChunk(text_chunk("m1", "thinking"));
        assert_eq!(wire_tag(&u), "agent_thought_chunk");
        assert!(matches!(
            inbound(&round_trip(&u), "fallback"),
            Inbound::Agent(AgentEvent::Reasoning { delta, .. }) if delta == "thinking"
        ));
    }

    #[test]
    fn user_message_chunk_is_ignored_we_authored_it() {
        let u = wire::SessionUpdate::UserMessageChunk(text_chunk("m1", "you said"));
        assert_eq!(wire_tag(&u), "user_message_chunk");
        assert!(matches!(inbound(&round_trip(&u), "f"), Inbound::Ignored));
    }

    #[test]
    fn a_chunk_without_message_id_uses_the_fallback() {
        let chunk = wire::ContentChunk::new(wire::ContentBlock::from("x".to_owned()));
        let u = wire::SessionUpdate::AgentMessageChunk(chunk);
        match inbound(&round_trip(&u), "fallback-id") {
            Inbound::Agent(AgentEvent::Token { message_id, .. }) => {
                assert_eq!(message_id, "fallback-id");
            }
            other => panic!("expected Token, got {other:?}"),
        }
    }

    #[test]
    fn a_non_text_chunk_is_ignored() {
        // An image block carries no text delta for the Token surface.
        let img = wire::ImageContent::new("iVBORw0KGgo=".to_owned(), "image/png".to_owned());
        let chunk = wire::ContentChunk::new(wire::ContentBlock::Image(img));
        let u = wire::SessionUpdate::AgentMessageChunk(chunk);
        assert!(matches!(inbound(&round_trip(&u), "f"), Inbound::Ignored));
    }

    #[test]
    fn tool_call_maps_to_started() {
        let tc = wire::ToolCall::new(wire::ToolCallId::new("c1"), "bash")
            .status(wire::ToolCallStatus::InProgress)
            .raw_input(serde_json::json!({"cmd": "ls"}));
        let u = wire::SessionUpdate::ToolCall(tc);
        assert_eq!(wire_tag(&u), "tool_call");
        match inbound(&round_trip(&u), "msg") {
            Inbound::Agent(AgentEvent::ToolCallStarted {
                call_id,
                name,
                args,
                ..
            }) => {
                assert_eq!(call_id, "c1");
                assert_eq!(name, "bash");
                assert_eq!(args, serde_json::json!({"cmd": "ls"}));
            }
            other => panic!("expected ToolCallStarted, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_update_completed_maps_to_finished_success() {
        let fields = wire::ToolCallUpdateFields::new()
            .status(wire::ToolCallStatus::Completed)
            .content(vec![text_toolcall_content("done".to_owned())]);
        let u = wire::SessionUpdate::ToolCallUpdate(wire::ToolCallUpdate::new(
            wire::ToolCallId::new("c1"),
            fields,
        ));
        assert_eq!(wire_tag(&u), "tool_call_update");
        match inbound(&round_trip(&u), "msg") {
            Inbound::Agent(AgentEvent::ToolCallFinished {
                call_id,
                success,
                result,
                ..
            }) => {
                assert_eq!(call_id, "c1");
                assert!(success);
                assert_eq!(result, "done");
            }
            other => panic!("expected ToolCallFinished, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_update_failed_maps_to_finished_failure() {
        let fields = wire::ToolCallUpdateFields::new().status(wire::ToolCallStatus::Failed);
        let u = wire::SessionUpdate::ToolCallUpdate(wire::ToolCallUpdate::new(
            wire::ToolCallId::new("c1"),
            fields,
        ));
        match inbound(&round_trip(&u), "msg") {
            Inbound::Agent(AgentEvent::ToolCallFinished { success: false, .. }) => {}
            other => panic!("expected failed ToolCallFinished, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_update_in_progress_with_content_maps_to_output_chunk() {
        let fields = wire::ToolCallUpdateFields::new()
            .status(wire::ToolCallStatus::InProgress)
            .content(vec![text_toolcall_content("streaming…".to_owned())]);
        let u = wire::SessionUpdate::ToolCallUpdate(wire::ToolCallUpdate::new(
            wire::ToolCallId::new("c1"),
            fields,
        ));
        match inbound(&round_trip(&u), "msg") {
            Inbound::Agent(AgentEvent::ToolOutputChunk { delta, .. }) => {
                assert_eq!(delta, "streaming…");
            }
            other => panic!("expected ToolOutputChunk, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_update_with_no_content_and_no_terminal_status_is_ignored() {
        let fields = wire::ToolCallUpdateFields::new(); // no status, no content
        let u = wire::SessionUpdate::ToolCallUpdate(wire::ToolCallUpdate::new(
            wire::ToolCallId::new("c1"),
            fields,
        ));
        assert!(matches!(inbound(&round_trip(&u), "msg"), Inbound::Ignored));
    }

    #[test]
    fn completed_with_no_content_still_finishes_with_empty_result() {
        // A bare status-only Completed update must not be swallowed as Ignored.
        let fields = wire::ToolCallUpdateFields::new().status(wire::ToolCallStatus::Completed);
        let u = wire::SessionUpdate::ToolCallUpdate(wire::ToolCallUpdate::new(
            wire::ToolCallId::new("c1"),
            fields,
        ));
        match inbound(&round_trip(&u), "msg") {
            Inbound::Agent(AgentEvent::ToolCallFinished {
                success: true,
                result,
                ..
            }) => assert!(result.is_empty()),
            other => panic!("expected ToolCallFinished, got {other:?}"),
        }
    }

    #[test]
    fn current_mode_update_surfaces_as_mode_changed() {
        let u = wire::SessionUpdate::CurrentModeUpdate(wire::CurrentModeUpdate::new("act"));
        assert_eq!(wire_tag(&u), "current_mode_update");
        match inbound(&round_trip(&u), "f") {
            Inbound::ModeChanged { mode_id } => assert_eq!(mode_id, "act"),
            other => panic!("expected ModeChanged, got {other:?}"),
        }
    }

    #[test]
    fn plan_and_usage_updates_are_ignored_today() {
        // These have no FlowForge producer/surface; revisit when one exists.
        // (`Plan` is behind the schema's `unstable_plan_operations` feature and
        // is therefore not constructible here; the `_` arm in `inbound` covers
        // it regardless of whether the feature is on.)
        let usage = wire::SessionUpdate::UsageUpdate(wire::UsageUpdate::new(0, 0));
        assert!(matches!(
            inbound(&round_trip(&usage), "f"),
            Inbound::Ignored
        ));
    }

    // --- outbound -----------------------------------------------------------

    #[test]
    fn token_maps_to_agent_message_chunk() {
        let e = AgentEvent::Token {
            message_id: "m1".to_owned(),
            delta: "hi".to_owned(),
        };
        let u = outbound(&e).unwrap();
        assert_eq!(wire_tag(&u), "agent_message_chunk");
        let json = serde_json::to_value(&u).unwrap();
        assert_eq!(json["content"]["type"], "text");
        assert_eq!(json["content"]["text"], "hi");
        assert_eq!(json["messageId"], "m1");
    }

    #[test]
    fn reasoning_maps_to_agent_thought_chunk_not_message() {
        let e = AgentEvent::Reasoning {
            message_id: "m1".to_owned(),
            delta: "thinking".to_owned(),
        };
        let u = outbound(&e).unwrap();
        assert_eq!(wire_tag(&u), "agent_thought_chunk");
    }

    #[test]
    fn tool_call_started_maps_to_a_tool_call() {
        let e = tool_started("c1", "bash", serde_json::json!({"cmd": "ls"}));
        let u = outbound(&e).unwrap();
        assert_eq!(wire_tag(&u), "tool_call");
        let json = serde_json::to_value(&u).unwrap();
        assert_eq!(json["toolCallId"], "c1");
        assert_eq!(json["title"], "bash");
        assert_eq!(json["status"], "in_progress");
        assert_eq!(json["rawInput"], serde_json::json!({"cmd": "ls"}));
    }

    #[test]
    fn tool_call_started_with_null_args_omits_raw_input() {
        let e = tool_started("c1", "noop", serde_json::Value::Null);
        let json = serde_json::to_value(outbound(&e).unwrap()).unwrap();
        assert!(
            json.get("rawInput").is_none() || json["rawInput"].is_null(),
            "null args must not set rawInput on the wire"
        );
    }

    #[test]
    fn tool_call_finished_maps_to_a_completed_update() {
        let e = tool_finished("c1", true, "ok");
        let u = outbound(&e).unwrap();
        assert_eq!(wire_tag(&u), "tool_call_update");
        let json = serde_json::to_value(&u).unwrap();
        assert_eq!(json["toolCallId"], "c1");
        assert_eq!(json["status"], "completed");
        assert_eq!(json["content"][0]["content"]["text"], "ok");
    }

    #[test]
    fn tool_call_finished_failed_maps_to_failed_status() {
        let e = tool_finished("c1", false, "boom");
        let json = serde_json::to_value(outbound(&e).unwrap()).unwrap();
        assert_eq!(json["status"], "failed");
    }

    #[test]
    fn tool_output_chunk_maps_to_an_in_progress_update() {
        let e = AgentEvent::ToolOutputChunk {
            message_id: "m".to_owned(),
            call_id: "c1".to_owned(),
            stream: ff_tools::OutputStream::Stdout,
            delta: "streaming…".to_owned(),
        };
        let json = serde_json::to_value(outbound(&e).unwrap()).unwrap();
        assert_eq!(json["status"], "in_progress");
        assert_eq!(json["content"][0]["content"]["text"], "streaming…");
    }

    #[test]
    fn housekeeping_events_become_agent_message_notices() {
        let cases: Vec<(AgentEvent, &'static str)> = vec![
            (
                AgentEvent::MemoryFlushed {
                    message_id: "m".to_owned(),
                    writes: 3,
                },
                "[memory] auto-wrote 3 fact(s)",
            ),
            (
                AgentEvent::AttachmentsDropped {
                    message_id: "m".to_owned(),
                    count: 2,
                    reason: None,
                },
                "[attachments] 2 dropped",
            ),
            (
                AgentEvent::AttachmentsDropped {
                    message_id: "m".to_owned(),
                    count: 2,
                    reason: Some("too big".to_owned()),
                },
                "[attachments] 2 dropped: too big",
            ),
            (
                AgentEvent::Reconnecting {
                    message_id: "m".to_owned(),
                    attempt: 1,
                    max_attempts: 3,
                },
                "[reconnecting] attempt 1/3",
            ),
            (
                AgentEvent::Error {
                    message: "boom".to_owned(),
                },
                "[error] boom",
            ),
            (
                AgentEvent::ConnectionFailed {
                    message_id: "m".to_owned(),
                    message: "drop".to_owned(),
                },
                "[connection failed] drop",
            ),
        ];
        for (event, expected) in cases {
            let u = outbound(&event).unwrap();
            assert_eq!(wire_tag(&u), "agent_message_chunk", "{event:?}");
            let json = serde_json::to_value(&u).unwrap();
            assert_eq!(json["content"]["text"], expected, "{event:?}");
        }
    }

    #[test]
    fn error_notice_carries_no_message_id() {
        let e = AgentEvent::Error {
            message: "boom".to_owned(),
        };
        let json = serde_json::to_value(outbound(&e).unwrap()).unwrap();
        assert!(
            json.get("messageId").is_none(),
            "Error has no message id of its own; the chunk must not invent one"
        );
    }

    #[test]
    fn done_carries_no_update() {
        assert!(outbound(&done("m", None)).is_none());
        assert!(outbound(&done("m", Some(F::Cancelled))).is_none());
    }

    // --- stop-reason round-trip ---------------------------------------------

    #[test]
    fn cancelled_round_trips_through_both_directions() {
        assert_eq!(
            inbound_stop_reason(wire::StopReason::Cancelled),
            Some(F::Cancelled)
        );
        assert_eq!(
            outbound_stop_reason(Some(F::Cancelled)),
            wire::StopReason::Cancelled
        );
    }

    #[test]
    fn max_turn_requests_and_tool_limit_are_inverses() {
        assert_eq!(
            inbound_stop_reason(wire::StopReason::MaxTurnRequests),
            Some(F::ToolLimit)
        );
        assert_eq!(
            outbound_stop_reason(Some(F::ToolLimit)),
            wire::StopReason::MaxTurnRequests
        );
    }

    #[test]
    fn interrupted_maps_to_cancelled_outbound() {
        assert_eq!(
            outbound_stop_reason(Some(F::Interrupted)),
            wire::StopReason::Cancelled
        );
    }

    #[test]
    fn end_turn_is_normal_completion_inbound() {
        assert_eq!(inbound_stop_reason(wire::StopReason::EndTurn), None);
        assert_eq!(outbound_stop_reason(None), wire::StopReason::EndTurn);
    }

    #[test]
    fn stall_empty_malformed_collapse_to_end_turn() {
        for r in [F::Stall, F::EmptyResponse, F::MalformedToolCall] {
            assert_eq!(
                outbound_stop_reason(Some(r)),
                wire::StopReason::EndTurn,
                "{r:?} has no precise ACP equivalent"
            );
        }
    }
}
