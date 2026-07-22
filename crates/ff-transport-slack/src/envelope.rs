//! Offline parsing of Slack Socket Mode envelopes (#912 T2, RFC 0021 §5.1).
//!
//! Socket Mode wraps every payload in an outer envelope:
//! `{ "type": "...", "envelope_id": "...", "payload": { ... } }` (plus control
//! frames `hello` and `disconnect` that carry no `payload`). [`parse_envelope`]
//! turns a raw WebSocket text frame into a typed [`SlackEnvelope`] so the T3
//! transport can demux user messages (→ Router) from interaction callbacks
//! (→ approver) without re-touching the wire format.
//!
//! This module is pure and offline: no socket, no ack, no Router. It preserves
//! `envelope_id` on the frames that require one so T3 can acknowledge them.

use ff_transport::{ChannelId, InboundMessage};
use serde::Deserialize;

/// A parsed Slack Socket Mode frame, demux-ready.
#[derive(Debug)]
pub enum SlackEnvelope {
    /// A user message (Events API `message` event) → an [`InboundMessage`] the
    /// Router turns into an agent turn. Carries the `envelope_id` T3 must ack.
    Message {
        envelope_id: String,
        message: InboundMessage,
    },
    /// A `block_actions` interaction (button click) → routed to the approver in
    /// T3, never to a Router turn. Carries the `envelope_id` T3 must ack.
    Interaction {
        envelope_id: String,
        interaction: SlackInteraction,
    },
    /// The connection-confirmation control frame Slack sends on connect. No ack.
    Hello,
    /// Slack asked us to reconnect (URL refresh / server maintenance). No ack.
    Disconnect { reason: String },
}

/// A Slack `block_actions` interaction payload, reduced to the fields the
/// approver needs to correlate a button click with a pending gate decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackInteraction {
    /// The `action_id` of the clicked block element (e.g. `"approve"`).
    pub action_id: String,
    /// The element's `value` (e.g. an encoded decision id), if present.
    pub value: Option<String>,
    /// Channel the interactive message lives in.
    pub channel: ChannelId,
    /// The Slack user who clicked.
    pub user_id: String,
    /// `ts` of the message the action belongs to, for correlation / edits.
    pub message_ts: Option<String>,
    /// One-time URL for responding to this interaction (used by T3).
    pub response_url: Option<String>,
}

/// Why an envelope could not be turned into a [`SlackEnvelope`].
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// The frame was not valid JSON.
    #[error("invalid JSON envelope: {0}")]
    Json(#[from] serde_json::Error),
    /// A frame type T2 does not model (e.g. `slash_commands`, `events_api`
    /// non-message events). T3 logs and drops these rather than crashing.
    #[error("unsupported envelope: {0}")]
    Unsupported(String),
    /// A modelled frame was missing a field it must carry.
    #[error("malformed {kind} envelope: missing {field}")]
    Malformed {
        kind: &'static str,
        field: &'static str,
    },
}

// ---- Wire structs (deserialization only) -----------------------------------

#[derive(Deserialize)]
struct Outer {
    #[serde(rename = "type")]
    kind: String,
    envelope_id: Option<String>,
    payload: Option<serde_json::Value>,
    // `disconnect` carries `reason` at the top level, not inside `payload`.
    reason: Option<String>,
}

#[derive(Deserialize)]
struct EventsApiPayload {
    event: EventInner,
}

#[derive(Deserialize)]
struct EventInner {
    #[serde(rename = "type")]
    kind: String,
    /// Present on real user messages; bot echoes set `bot_id` and are skipped.
    #[serde(default)]
    bot_id: Option<String>,
    /// A message `subtype` (e.g. `message_changed`, `channel_join`) marks a
    /// non-user event we don't turn into a turn.
    #[serde(default)]
    subtype: Option<String>,
    user: Option<String>,
    text: Option<String>,
    channel: Option<String>,
    ts: Option<String>,
}

#[derive(Deserialize)]
struct InteractivePayload {
    #[serde(rename = "type")]
    kind: String,
    user: InteractiveUser,
    channel: Option<InteractiveChannel>,
    actions: Option<Vec<InteractiveAction>>,
    message: Option<InteractiveMessage>,
    response_url: Option<String>,
}

#[derive(Deserialize)]
struct InteractiveUser {
    id: String,
}

#[derive(Deserialize)]
struct InteractiveChannel {
    id: String,
}

#[derive(Deserialize)]
struct InteractiveMessage {
    ts: Option<String>,
}

#[derive(Deserialize)]
struct InteractiveAction {
    action_id: String,
    value: Option<String>,
}

// ---- Parsing ----------------------------------------------------------------

/// Parse one raw Socket Mode text frame into a typed [`SlackEnvelope`].
pub fn parse_envelope(raw: &str) -> Result<SlackEnvelope, ParseError> {
    let outer: Outer = serde_json::from_str(raw)?;

    match outer.kind.as_str() {
        "hello" => Ok(SlackEnvelope::Hello),
        "disconnect" => Ok(SlackEnvelope::Disconnect {
            reason: outer.reason.unwrap_or_else(|| "unspecified".to_string()),
        }),
        "events_api" => parse_events_api(outer),
        "interactive" => parse_interactive(outer),
        other => Err(ParseError::Unsupported(other.to_string())),
    }
}

fn envelope_id(outer: &Outer, kind: &'static str) -> Result<String, ParseError> {
    outer.envelope_id.clone().ok_or(ParseError::Malformed {
        kind,
        field: "envelope_id",
    })
}

fn parse_events_api(outer: Outer) -> Result<SlackEnvelope, ParseError> {
    let id = envelope_id(&outer, "events_api")?;
    let payload = outer.payload.clone().ok_or(ParseError::Malformed {
        kind: "events_api",
        field: "payload",
    })?;
    let payload: EventsApiPayload = serde_json::from_value(payload)?;
    let event = payload.event;

    // Only plain user messages become turns: skip bot echoes and message
    // subtypes (edits, joins, etc.) so the Router isn't fed non-user noise.
    if event.kind != "message" {
        return Err(ParseError::Unsupported(format!(
            "events_api:{}",
            event.kind
        )));
    }
    if event.bot_id.is_some() {
        return Err(ParseError::Unsupported(
            "events_api:message(bot)".to_string(),
        ));
    }
    if let Some(subtype) = &event.subtype {
        return Err(ParseError::Unsupported(format!(
            "events_api:message.{subtype}"
        )));
    }

    let channel = event.channel.ok_or(ParseError::Malformed {
        kind: "events_api",
        field: "event.channel",
    })?;
    let sender_id = event.user.ok_or(ParseError::Malformed {
        kind: "events_api",
        field: "event.user",
    })?;
    let ts = event.ts.ok_or(ParseError::Malformed {
        kind: "events_api",
        field: "event.ts",
    })?;

    let message = InboundMessage {
        channel: ChannelId::new("slack", channel),
        text: event.text.unwrap_or_default(),
        sender_id,
        timestamp: parse_slack_ts(&ts),
    };
    Ok(SlackEnvelope::Message {
        envelope_id: id,
        message,
    })
}

fn parse_interactive(outer: Outer) -> Result<SlackEnvelope, ParseError> {
    let id = envelope_id(&outer, "interactive")?;
    let payload = outer.payload.clone().ok_or(ParseError::Malformed {
        kind: "interactive",
        field: "payload",
    })?;
    let payload: InteractivePayload = serde_json::from_value(payload)?;

    if payload.kind != "block_actions" {
        return Err(ParseError::Unsupported(format!(
            "interactive:{}",
            payload.kind
        )));
    }

    let action = payload
        .actions
        .and_then(|mut a| {
            if a.is_empty() {
                None
            } else {
                Some(a.remove(0))
            }
        })
        .ok_or(ParseError::Malformed {
            kind: "interactive",
            field: "actions[0]",
        })?;
    let channel = payload.channel.ok_or(ParseError::Malformed {
        kind: "interactive",
        field: "channel",
    })?;

    let interaction = SlackInteraction {
        action_id: action.action_id,
        value: action.value,
        channel: ChannelId::new("slack", channel.id),
        user_id: payload.user.id,
        message_ts: payload.message.and_then(|m| m.ts),
        response_url: payload.response_url,
    };
    Ok(SlackEnvelope::Interaction {
        envelope_id: id,
        interaction,
    })
}

/// Slack timestamps are `"<seconds>.<micros>"` strings; the router's
/// `InboundMessage::timestamp` is integer seconds, so we take the whole-second
/// part. A missing / unparseable value degrades to `0` rather than failing —
/// the timestamp is advisory, not a correlation key.
fn parse_slack_ts(ts: &str) -> i64 {
    ts.split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}
