use serde::{Deserialize, Serialize};

/// Identifies a channel on a specific transport (e.g. a Slack channel, a Discord
/// thread, a CLI stdin session). The router maps channels to FlowForge sessions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChannelId {
    /// Transport name (e.g. "slack", "discord", "cli").
    pub transport: String,
    /// Platform-specific identifier (channel ID, thread ID, etc.).
    pub platform_id: String,
}

impl ChannelId {
    pub fn new(transport: impl Into<String>, platform_id: impl Into<String>) -> Self {
        Self {
            transport: transport.into(),
            platform_id: platform_id.into(),
        }
    }
}

/// An inbound message from an external transport.
#[derive(Debug, Clone)]
pub struct InboundMessage {
    pub channel: ChannelId,
    pub text: String,
    pub sender_id: String,
    pub timestamp: i64,
    /// Opaque, transport-specific anchor for replying in the same conversation
    /// thread as this message. Slack sets it to the thread the reply should land
    /// in (`thread_ts` if the message is already threaded, else the message's own
    /// `ts` so the reply opens a thread on it). Transports without threading
    /// (CLI, mock) leave it `None`, and a transport that ignores it still behaves
    /// exactly as before.
    pub reply_thread: Option<String>,
}

/// Notifications the router can push to a transport (non-response events).
#[derive(Debug, Clone)]
pub enum Notification {
    TurnStarted,
    ToolCall {
        name: String,
    },
    TurnFinished,
    /// Fatal turn-level failure (not recoverable by the agent). Tool-level and
    /// loop-level errors that the model may retry are intentionally not surfaced
    /// as notifications.
    Error(String),
}
