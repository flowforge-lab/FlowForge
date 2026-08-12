use async_trait::async_trait;

use crate::types::{ChannelId, InboundMessage, Notification};

/// A response stream the router writes assistant output into. The transport
/// implementation delivers chunks to the end platform (e.g. editing a Slack
/// message, streaming to a WebSocket).
#[async_trait]
pub trait ResponseStream: Send + Sync {
    /// Append a text chunk to the ongoing response.
    ///
    /// The argument is the **full text so far**, superseding any earlier chunk —
    /// the Router flushes the accumulated buffer, and [`SlackResponseStream`]
    /// treats each `chunk` as the authoritative current body. Transports throttle
    /// and coalesce as they like (Slack edits at most every ~500ms and skips
    /// unchanged bodies), so a caller may re-deliver freely.
    ///
    /// `+ Sync` is required because the Router hands the stream to a background
    /// flusher task for mid-turn streaming edits (RFC 0021 §5.1): `chunk` takes
    /// `&self`, so a stream shared across an await point must be `Sync`.
    async fn chunk(&self, text: &str);
    /// Signal that the response is complete.
    async fn finish(&self);
}

/// A cross-task handle for stopping a transport gracefully.
///
/// Held separately from the transport itself because [`Router::run`](crate::Router::run)
/// borrows the transport mutably for its whole lifetime, so a signal handler
/// cannot reach it. Cloneable and cheap; calling [`Self::shutdown`] more than
/// once, or after the transport is gone, is a no-op.
#[derive(Clone, Default)]
pub struct ShutdownHandle {
    notify: Option<std::sync::Arc<tokio::sync::Notify>>,
}

impl ShutdownHandle {
    /// Create a connected handle plus the [`tokio::sync::Notify`] a transport
    /// waits on.
    pub fn new() -> (Self, std::sync::Arc<tokio::sync::Notify>) {
        let notify = std::sync::Arc::new(tokio::sync::Notify::new());
        (
            Self {
                notify: Some(notify.clone()),
            },
            notify,
        )
    }

    /// Ask the transport to stop accepting new messages. Work already accepted
    /// still completes — this closes the inbound side, it does not abort a turn.
    pub fn shutdown(&self) {
        if let Some(notify) = &self.notify {
            notify.notify_waiters();
            // A permit makes this edge-triggered signal safe to send before the
            // transport starts waiting, which is the ordering `serve` has when
            // Ctrl-C arrives during startup.
            notify.notify_one();
        }
    }
}

impl std::fmt::Debug for ShutdownHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShutdownHandle")
            .field("connected", &self.notify.is_some())
            .finish()
    }
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
    /// `reply_thread` is the triggering message's [`InboundMessage::reply_thread`]
    /// anchor: a transport that supports threading (Slack) posts the reply into it,
    /// so answers land in the thread they were asked in (#1098). Transports without
    /// threading ignore it and reply exactly as before.
    ///
    /// Takes `&self` (not `&mut self`) so the router can hold a transport reference
    /// across an async turn without exclusive borrowing. Transports that need
    /// mutable state (e.g. Slack `chat.update` with a message handle) should use
    /// interior mutability (`Arc<Mutex<...>>`) in the returned `ResponseStream`.
    fn begin_response(
        &self,
        channel: &ChannelId,
        reply_thread: Option<&str>,
    ) -> Box<dyn ResponseStream>;

    /// Push a non-response notification (typing indicator, tool call label, etc.).
    fn notify(&self, channel: &ChannelId, notification: Notification);

    /// Hand out a handle that stops this transport gracefully.
    ///
    /// Defaults to a disconnected handle, so a transport with no external input
    /// to close (or one that has not implemented shutdown yet) keeps compiling —
    /// its host simply has nothing to signal. Implementors that own an inbound
    /// channel should override this and arrange for [`Self::recv`] to yield
    /// `None`, which the router already treats as a clean stop.
    fn shutdown_handle(&mut self) -> ShutdownHandle {
        ShutdownHandle::default()
    }
}
