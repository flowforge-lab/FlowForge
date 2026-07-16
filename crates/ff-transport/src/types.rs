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
