//! `SlackTransport` — the [`MessageTransport`] the Router drives over Slack
//! Socket Mode (#912 T3, RFC 0021 §5.1).
//!
//! ## Connection lifecycle
//! [`connect`](SlackTransport::connect) calls the Web API `apps.connections.open`
//! with the app-level token to obtain a single-use WSS URL, dials it, then
//! `split`s the socket into a write half (given to the [writer task](crate::writer))
//! and a read half (driven by the [reader task](Self::spawn_reader)).
//!
//! ## Demux (the #1058 core)
//! One socket, two consumers. The reader task parses each frame and fans out:
//! - **user messages** → an inbound `mpsc` the Router drains via [`recv`];
//! - **interactions** (button clicks) → a separate `mpsc` a future approver (T4)
//!   drains — never a Router turn;
//! - **control frames** (`hello`, `disconnect`) → handled inline.
//!
//! Every frame that requires an ack is acked by enqueuing an [`OutboundOp::Ack`]
//! on the shared writer, so acks and outbound messages serialize through the one
//! task that owns the write half. No `&mut` to the socket is ever shared.

use async_trait::async_trait;
use futures_util::stream::SplitStream;
use futures_util::StreamExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use ff_transport::{ChannelId, InboundMessage, Notification};
use ff_transport::{MessageTransport, ResponseStream};

use crate::api::SlackApi;
use crate::envelope::{parse_envelope, SlackEnvelope, SlackInteraction};
use crate::response::SlackResponseStream;
use crate::writer::{spawn_writer, OutboundOp, WriterHandle};

/// The transport name reported to the Router and stamped on every [`ChannelId`].
pub const TRANSPORT_NAME: &str = "slack";

/// The WebSocket read half owned by the reader task.
type WsStream = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

/// A Slack Socket Mode transport.
///
/// Constructed with the two Slack tokens, then [`connect`](Self::connect)ed once
/// before the Router runs. After connect, [`recv`](Self::recv) yields inbound
/// user messages and [`begin_response`](Self::begin_response) opens a streaming
/// reply.
pub struct SlackTransport {
    /// App-level token (`xapp-...`) — opens the Socket Mode connection.
    app_token: String,
    /// Bot token (`xoxb-...`) — authenticates Web API message posts/edits.
    bot_token: String,
    /// Web API base, overridable for tests.
    api_base: Option<String>,
    /// `apps.connections.open` endpoint, overridable for tests.
    connections_open_url: String,
    /// Inbound user messages, produced by the reader task. `None` until connect.
    inbound_rx: Option<mpsc::Receiver<InboundMessage>>,
    /// Interactions (button clicks) for the approver (T4). `None` until connect.
    /// Held here so a future `flowforge serve` can wire it to the approver; T3
    /// only guarantees they are demuxed off the Router path.
    interaction_rx: Option<mpsc::Receiver<SlackInteraction>>,
    /// Shared writer handle. `None` until connect.
    writer: Option<WriterHandle>,
}

impl SlackTransport {
    /// Build a transport from the app-level and bot tokens.
    pub fn new(app_token: impl Into<String>, bot_token: impl Into<String>) -> Self {
        Self {
            app_token: app_token.into(),
            bot_token: bot_token.into(),
            api_base: None,
            connections_open_url: "https://slack.com/api/apps.connections.open".to_string(),
            inbound_rx: None,
            interaction_rx: None,
            writer: None,
        }
    }

    /// Point the Web API and `apps.connections.open` at `base` (for tests).
    /// `base` is a URL prefix; `apps.connections.open` becomes `{base}/apps.connections.open`.
    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        let base = base.into();
        self.connections_open_url = format!("{base}/apps.connections.open");
        self.api_base = Some(base);
        self
    }

    /// Take the interaction receiver so a caller (T4 approver wiring) can drain
    /// button clicks. Returns `None` before `connect` or if already taken.
    pub fn take_interaction_rx(&mut self) -> Option<mpsc::Receiver<SlackInteraction>> {
        self.interaction_rx.take()
    }

    /// Ask Slack for a single-use WSS URL via `apps.connections.open`.
    async fn open_connection_url(
        &self,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        #[derive(serde::Deserialize)]
        struct OpenResp {
            ok: bool,
            url: Option<String>,
            error: Option<String>,
        }
        let resp: OpenResp = reqwest::Client::new()
            .post(&self.connections_open_url)
            .bearer_auth(&self.app_token)
            .send()
            .await?
            .json()
            .await?;
        if !resp.ok {
            return Err(format!(
                "apps.connections.open failed: {}",
                resp.error.unwrap_or_else(|| "unknown".to_string())
            )
            .into());
        }
        resp.url
            .ok_or_else(|| "apps.connections.open: missing url".into())
    }

    /// Spawn the reader task: parse frames and fan out to inbound / interaction
    /// queues, acking through the shared writer. Ends when the socket closes.
    fn spawn_reader(
        mut ws_stream: WsStream,
        writer: WriterHandle,
        inbound_tx: mpsc::Sender<InboundMessage>,
        interaction_tx: mpsc::Sender<SlackInteraction>,
    ) {
        tokio::spawn(async move {
            while let Some(frame) = ws_stream.next().await {
                let text = match frame {
                    Ok(Message::Text(t)) => t,
                    // Slack pings keep-alive; tungstenite auto-pongs. Ignore
                    // other frame kinds; a Close ends the stream.
                    Ok(Message::Close(_)) => break,
                    Ok(_) => continue,
                    Err(_) => break, // socket error → reader ends, transport recv() returns None
                };
                let env = match parse_envelope(&text) {
                    Ok(env) => env,
                    // A frame we can't model is not fatal — skip it.
                    Err(_) => continue,
                };
                match env {
                    SlackEnvelope::Message {
                        envelope_id,
                        message,
                    } => {
                        writer.send(OutboundOp::Ack { envelope_id }).await;
                        // If the Router dropped the receiver, we're shutting
                        // down; stop reading.
                        if inbound_tx.send(message).await.is_err() {
                            break;
                        }
                    }
                    SlackEnvelope::Interaction {
                        envelope_id,
                        interaction,
                    } => {
                        writer.send(OutboundOp::Ack { envelope_id }).await;
                        // No approver draining yet (T4) → drop is fine.
                        let _ = interaction_tx.send(interaction).await;
                    }
                    // Control frames: hello needs nothing; a disconnect means
                    // Slack is closing this socket (reconnect is Phase 2).
                    SlackEnvelope::Hello => {}
                    SlackEnvelope::Disconnect { .. } => break,
                }
            }
        });
    }
}

#[async_trait]
impl MessageTransport for SlackTransport {
    fn name(&self) -> &str {
        TRANSPORT_NAME
    }

    async fn connect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let url = self.open_connection_url().await?;
        let (ws, _resp) = tokio_tungstenite::connect_async(&url).await?;
        let (sink, stream) = ws.split();

        let mut api = SlackApi::new(self.bot_token.clone());
        if let Some(base) = &self.api_base {
            api = api.with_base(base.clone());
        }

        // Writer owns the sink; reader owns the stream. Both share the handle.
        let writer = spawn_writer(sink, api, None);

        let (inbound_tx, inbound_rx) = mpsc::channel::<InboundMessage>(64);
        let (interaction_tx, interaction_rx) = mpsc::channel::<SlackInteraction>(64);
        Self::spawn_reader(stream, writer.clone(), inbound_tx, interaction_tx);

        self.writer = Some(writer);
        self.inbound_rx = Some(inbound_rx);
        self.interaction_rx = Some(interaction_rx);
        Ok(())
    }

    async fn recv(&mut self) -> Option<InboundMessage> {
        // `None` (not connected) also means "closed" to the Router — a clean stop.
        match self.inbound_rx.as_mut() {
            Some(rx) => rx.recv().await,
            None => None,
        }
    }

    fn begin_response(&self, channel: &ChannelId) -> Box<dyn ResponseStream> {
        // If called before connect (shouldn't happen in the Router flow), fall
        // back to a stream over a closed writer so chunks are harmlessly dropped.
        let writer = self
            .writer
            .clone()
            .unwrap_or_else(crate::writer::WriterHandle::disconnected);
        Box::new(SlackResponseStream::new(
            channel.platform_id.clone(),
            writer,
        ))
    }

    fn notify(&self, _channel: &ChannelId, _notification: Notification) {
        // Typing indicators / tool-call labels map to Slack later (Block Kit).
        // T3 no-ops so the Router's notify calls are harmless.
    }
}
