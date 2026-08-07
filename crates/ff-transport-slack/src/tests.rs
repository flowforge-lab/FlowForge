//! Offline parser tests for Slack Socket Mode envelopes (#912 T2).
//!
//! Fixtures mirror the real Socket Mode wire format documented at
//! <https://docs.slack.dev/apis/socket-mode> and the interactivity /
//! Events API references: an outer `{type, envelope_id, payload}` frame
//! wrapping an Events API event or an interactive `block_actions` payload,
//! plus the `hello` / `disconnect` control frames.

use crate::{parse_envelope, ParseError, SlackEnvelope};

const HELLO: &str = r#"{
  "type": "hello",
  "num_connections": 1,
  "debug_info": { "host": "applink-1" },
  "connection_info": { "app_id": "A01234567" }
}"#;

const DISCONNECT: &str = r#"{
  "type": "disconnect",
  "reason": "refresh_requested",
  "debug_info": { "host": "applink-1" }
}"#;

const DISCONNECT_NO_REASON: &str = r#"{ "type": "disconnect" }"#;

const USER_MESSAGE: &str = r#"{
  "envelope_id": "abc-envelope-1",
  "type": "events_api",
  "accepts_response_payload": false,
  "payload": {
    "type": "event_callback",
    "event": {
      "type": "message",
      "channel": "C01234567",
      "user": "U99999999",
      "text": "deploy the thing",
      "ts": "1548261231.000200"
    }
  }
}"#;

const BOT_MESSAGE: &str = r#"{
  "envelope_id": "abc-envelope-bot",
  "type": "events_api",
  "payload": {
    "type": "event_callback",
    "event": {
      "type": "message",
      "bot_id": "B0123",
      "channel": "C01234567",
      "user": "U00000000",
      "text": "I am a bot echo",
      "ts": "1548261231.000300"
    }
  }
}"#;

const MESSAGE_SUBTYPE: &str = r#"{
  "envelope_id": "abc-envelope-edit",
  "type": "events_api",
  "payload": {
    "type": "event_callback",
    "event": {
      "type": "message",
      "subtype": "message_changed",
      "channel": "C01234567",
      "ts": "1548261231.000400"
    }
  }
}"#;

const NON_MESSAGE_EVENT: &str = r#"{
  "envelope_id": "abc-envelope-react",
  "type": "events_api",
  "payload": {
    "type": "event_callback",
    "event": {
      "type": "reaction_added",
      "user": "U99999999",
      "reaction": "thumbsup"
    }
  }
}"#;

const BLOCK_ACTIONS: &str = r#"{
  "envelope_id": "abc-envelope-2",
  "type": "interactive",
  "payload": {
    "type": "block_actions",
    "user": { "id": "U99999999", "username": "tony" },
    "channel": { "id": "C01234567", "name": "dev" },
    "message": { "ts": "1548261231.000200", "text": "Approve deploy?" },
    "response_url": "https://hooks.slack.com/actions/T0/1/xyz",
    "actions": [
      {
        "type": "button",
        "action_id": "approve",
        "block_id": "gate",
        "value": "decision-42"
      }
    ]
  }
}"#;

const BLOCK_ACTIONS_NO_VALUE: &str = r#"{
  "envelope_id": "abc-envelope-3",
  "type": "interactive",
  "payload": {
    "type": "block_actions",
    "user": { "id": "U11111111" },
    "channel": { "id": "C77777777" },
    "actions": [ { "type": "button", "action_id": "deny" } ]
  }
}"#;

const SLASH_COMMAND: &str = r#"{
  "envelope_id": "abc-envelope-slash",
  "type": "slash_commands",
  "payload": { "command": "/flowforge", "text": "status" }
}"#;

#[test]
fn hello_control_frame() {
    assert!(matches!(
        parse_envelope(HELLO).unwrap(),
        SlackEnvelope::Hello
    ));
}

#[test]
fn disconnect_carries_reason() {
    match parse_envelope(DISCONNECT).unwrap() {
        SlackEnvelope::Disconnect { reason } => assert_eq!(reason, "refresh_requested"),
        other => panic!("expected Disconnect, got {other:?}"),
    }
}

#[test]
fn disconnect_defaults_reason_when_absent() {
    match parse_envelope(DISCONNECT_NO_REASON).unwrap() {
        SlackEnvelope::Disconnect { reason } => assert_eq!(reason, "unspecified"),
        other => panic!("expected Disconnect, got {other:?}"),
    }
}

#[test]
fn user_message_maps_to_inbound_message() {
    match parse_envelope(USER_MESSAGE).unwrap() {
        SlackEnvelope::Message {
            envelope_id,
            message,
        } => {
            assert_eq!(envelope_id, "abc-envelope-1");
            assert_eq!(message.channel.transport, "slack");
            assert_eq!(message.channel.platform_id, "C01234567");
            assert_eq!(message.sender_id, "U99999999");
            assert_eq!(message.text, "deploy the thing");
            // "1548261231.000200" → whole-second part.
            assert_eq!(message.timestamp, 1548261231);
            // No `thread_ts` on the event → reply anchors to the message's own
            // `ts`, opening a thread on it (#1098).
            assert_eq!(message.reply_thread.as_deref(), Some("1548261231.000200"));
        }
        other => panic!("expected Message, got {other:?}"),
    }
}

const THREADED_USER_MESSAGE: &str = r#"{
  "envelope_id": "abc-envelope-thread",
  "type": "events_api",
  "accepts_response_payload": false,
  "payload": {
    "type": "event_callback",
    "event": {
      "type": "message",
      "channel": "C01234567",
      "user": "U99999999",
      "text": "and again",
      "ts": "1548261300.000500",
      "thread_ts": "1548261231.000200"
    }
  }
}"#;

#[test]
fn threaded_message_anchors_reply_to_the_existing_thread() {
    // The trigger is already inside a thread → reply into that same thread
    // (`thread_ts`), not the message's own `ts` (#1098).
    match parse_envelope(THREADED_USER_MESSAGE).unwrap() {
        SlackEnvelope::Message { message, .. } => {
            assert_eq!(message.reply_thread.as_deref(), Some("1548261231.000200"));
            assert_ne!(
                message.reply_thread.as_deref(),
                Some("1548261300.000500"),
                "must anchor to the thread root, not this message's own ts"
            );
        }
        other => panic!("expected Message, got {other:?}"),
    }
}

#[test]
fn bot_echo_is_unsupported() {
    assert!(matches!(
        parse_envelope(BOT_MESSAGE),
        Err(ParseError::Unsupported(_))
    ));
}

#[test]
fn message_subtype_is_unsupported() {
    assert!(matches!(
        parse_envelope(MESSAGE_SUBTYPE),
        Err(ParseError::Unsupported(_))
    ));
}

#[test]
fn non_message_event_is_unsupported() {
    assert!(matches!(
        parse_envelope(NON_MESSAGE_EVENT),
        Err(ParseError::Unsupported(_))
    ));
}

#[test]
fn block_actions_maps_to_interaction() {
    match parse_envelope(BLOCK_ACTIONS).unwrap() {
        SlackEnvelope::Interaction {
            envelope_id,
            interaction,
        } => {
            assert_eq!(envelope_id, "abc-envelope-2");
            assert_eq!(interaction.action_id, "approve");
            assert_eq!(interaction.value.as_deref(), Some("decision-42"));
            assert_eq!(interaction.channel.transport, "slack");
            assert_eq!(interaction.channel.platform_id, "C01234567");
            assert_eq!(interaction.user_id, "U99999999");
            assert_eq!(interaction.message_ts.as_deref(), Some("1548261231.000200"));
            assert_eq!(
                interaction.response_url.as_deref(),
                Some("https://hooks.slack.com/actions/T0/1/xyz")
            );
        }
        other => panic!("expected Interaction, got {other:?}"),
    }
}

#[test]
fn block_actions_without_value_or_message() {
    match parse_envelope(BLOCK_ACTIONS_NO_VALUE).unwrap() {
        SlackEnvelope::Interaction { interaction, .. } => {
            assert_eq!(interaction.action_id, "deny");
            assert_eq!(interaction.value, None);
            assert_eq!(interaction.message_ts, None);
            assert_eq!(interaction.response_url, None);
            assert_eq!(interaction.channel.platform_id, "C77777777");
        }
        other => panic!("expected Interaction, got {other:?}"),
    }
}

#[test]
fn slash_command_is_unsupported() {
    match parse_envelope(SLASH_COMMAND) {
        Err(ParseError::Unsupported(kind)) => assert_eq!(kind, "slash_commands"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn invalid_json_is_a_parse_error() {
    assert!(matches!(
        parse_envelope("not json {"),
        Err(ParseError::Json(_))
    ));
}

#[test]
fn events_api_without_envelope_id_is_malformed() {
    let raw = r#"{ "type": "events_api", "payload": { "event": { "type": "message" } } }"#;
    assert!(matches!(
        parse_envelope(raw),
        Err(ParseError::Malformed {
            field: "envelope_id",
            ..
        })
    ));
}

#[test]
fn unknown_top_level_type_is_unsupported() {
    match parse_envelope(r#"{ "type": "future_frame" }"#) {
        Err(ParseError::Unsupported(kind)) => assert_eq!(kind, "future_frame"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

const EMPTY_TEXT_MESSAGE: &str = r#"{
  "envelope_id": "abc-envelope-empty",
  "type": "events_api",
  "payload": {
    "type": "event_callback",
    "event": {
      "type": "message",
      "channel": "C01234567",
      "user": "U99999999",
      "text": "   ",
      "ts": "1548261231.000500"
    }
  }
}"#;

const BLOCK_ACTIONS_NO_USER: &str = r#"{
  "envelope_id": "abc-envelope-nouser",
  "type": "interactive",
  "payload": {
    "type": "block_actions",
    "channel": { "id": "C01234567", "name": "dev" },
    "actions": [
      { "type": "button", "action_id": "approve", "block_id": "gate", "value": "v" }
    ]
  }
}"#;

// A message whose text is empty/whitespace carries nothing actionable, so the
// parser rejects it as Unsupported rather than handing the Router an empty turn.
#[test]
fn empty_text_message_is_unsupported() {
    match parse_envelope(EMPTY_TEXT_MESSAGE) {
        Err(ParseError::Unsupported(kind)) => assert_eq!(kind, "events_api:message(empty)"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

// A block_actions payload missing the required nested `user` field fails during
// `serde_json::from_value`, so it surfaces as `Json` (the catch-all for missing
// nested required fields) rather than `Malformed`. Pins the error-model split
// documented on `ParseError::Json`.
#[test]
fn block_actions_missing_user_is_json_error() {
    assert!(matches!(
        parse_envelope(BLOCK_ACTIONS_NO_USER),
        Err(ParseError::Json(_))
    ));
}
