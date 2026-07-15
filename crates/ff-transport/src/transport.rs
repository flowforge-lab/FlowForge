use async_trait::async_trait;

use crate::types::{ChannelId, InboundMessage, Notification};

/// A response stream the router writes assistant output into. The transport
/// implementation delivers chunks to the end platform (e.g. editing a Slack
/// message, streaming to a WebSocket).
#[async_trait]
pub trait ResponseStream: Send {
    /// Append a text chunk to the ongoing response.
    async fn chunk(&self, text: &str);
    /// Signal that the response is complete.
    async fn finish(&self);
}

/// The host-facing transport trait. Each messaging platform (Slack, Discord,
/// CLI-over-pipe, etc.) implements this to bridge external messages into
/// FlowForge sessions via the [`Router`](crate::Router).
#[async_trait]
pub trait MessageTransport: Send + Sync {
    /// Human-readable name for logging/config (e.g. "slack", "discord").
    fn name(&self) -> &str;

    /// Establish the connection to the external platform. Called once at startup.
    async fn connect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// Block until the next inbound message arrives. Returns `None` when the
    /// transport is closed (clean exit).
    async fn recv(&mut self) -> Option<InboundMessage>;

    /// Open a response stream for the given channel. The router writes assistant
    /// output into this stream; the transport delivers it to the platform.
    ///
    /// Takes `&self` (not `&mut self`) so the router can hold a transport reference
    /// across an async turn without exclusive borrowing. Transports that need
    /// mutable state (e.g. Slack `chat.update` with a message handle) should use
    /// interior mutability (`Arc<Mutex<...>>`) in the returned `ResponseStream`.
    fn begin_response(&self, channel: &ChannelId) -> Box<dyn ResponseStream>;

    /// Push a non-response notification (typing indicator, tool call label, etc.).
    fn notify(&self, channel: &ChannelId, notification: Notification);
}
