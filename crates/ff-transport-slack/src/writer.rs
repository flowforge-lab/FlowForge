//! The **writer task**: the single owner of the WebSocket write half, plus the
//! clonable [`WriterHandle`] every sender uses to reach it (#912 T3, RFC 0021
//! §5.1).
//!
//! The core of #1058: the Router (via [`crate::response::SlackResponseStream`])
//! and a future interactive approver both need to send on one socket. Rather
//! than share a `&mut` to the write half, we give the write half to exactly one
//! task and hand out clonable `mpsc::Sender`s. No contention, no second mutable
//! borrow.
//!
//! Slack Socket Mode sends application replies over the Web API (HTTPS), not the
//! socket — the socket only carries inbound events and the required `ack`
//! frames. So the writer task multiplexes two sinks:
//! - **socket acks** ([`OutboundOp::Ack`]) → the WS write half;
//! - **message posts/edits** ([`OutboundOp::Post`] / [`OutboundOp::Update`]) →
//!   `chat.postMessage` / `chat.update` via [`SlackApi`].

use futures_util::stream::SplitSink;
use futures_util::SinkExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::api::SlackApi;

/// The WebSocket write half owned solely by the writer task.
type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

/// One unit of outbound work for the writer task.
#[derive(Debug, Clone)]
pub enum OutboundOp {
    /// Acknowledge a Socket Mode envelope (must be sent on the socket, quickly,
    /// or Slack redelivers and eventually disconnects).
    Ack { envelope_id: String },
    /// Post a new message (a first reply, or an overflow continuation part).
    Post {
        channel: String,
        text: String,
        /// Which part of the logical reply this is, echoed back with the
        /// assigned `ts` so the response stream can edit it next flush.
        part_index: usize,
    },
    /// Edit an existing message in place.
    Update {
        channel: String,
        ts: String,
        text: String,
    },
}

/// A clonable handle onto the writer task's inbound queue. Cheap to clone; every
/// clone feeds the same single writer.
#[derive(Clone)]
pub struct WriterHandle {
    tx: mpsc::Sender<OutboundOp>,
}

impl WriterHandle {
    /// Enqueue one outbound op. Silently drops if the writer task has exited
    /// (connection closed) — send failures are a lifecycle signal, not a
    /// per-message error the agent can act on.
    pub async fn send(&self, op: OutboundOp) {
        let _ = self.tx.send(op).await;
    }

    /// A handle whose writer task never existed: every `send` is dropped. Used
    /// as a harmless fallback when a response is opened before `connect`.
    pub fn disconnected() -> Self {
        let (tx, _rx) = mpsc::channel::<OutboundOp>(1);
        Self { tx }
    }

    /// Test-only: a handle plus the raw receiver, so a test can inspect the ops
    /// a producer emits without spawning the real writer task (which would make
    /// HTTP calls and consume timing nondeterministically).
    #[cfg(test)]
    pub(crate) fn channel_for_test() -> (Self, mpsc::Receiver<OutboundOp>) {
        let (tx, rx) = mpsc::channel::<OutboundOp>(256);
        (Self { tx }, rx)
    }
}

/// Reports the `ts` Slack assigned to a freshly posted part back to whichever
/// response stream posted it, so the next flush edits rather than re-posts.
///
/// T3 wires this to the active [`crate::response::SlackResponseStream`]. Kept as
/// a trait object so the writer task has no back-reference to the stream map.
pub trait TsSink: Send + Sync {
    /// Record that `part_index` of the current reply now lives at `ts`.
    fn record(&self, part_index: usize, ts: String);
}

/// Spawn the writer task and return a clonable handle onto it.
///
/// The task runs until its queue is closed (all handles dropped) or a socket
/// write fails. `api` performs the HTTPS Web API calls; `ts_sink`, when present,
/// receives the `ts` of each posted part.
pub fn spawn_writer(
    mut ws_sink: WsSink,
    api: SlackApi,
    ts_sink: Option<Box<dyn TsSink>>,
) -> WriterHandle {
    // A small buffer: acks and edits are low-volume; back-pressure here just
    // means a sender awaits briefly, which is fine.
    let (tx, mut rx) = mpsc::channel::<OutboundOp>(64);

    tokio::spawn(async move {
        while let Some(op) = rx.recv().await {
            match op {
                OutboundOp::Ack { envelope_id } => {
                    let payload = serde_json::json!({ "envelope_id": envelope_id });
                    if ws_sink
                        .send(Message::Text(payload.to_string()))
                        .await
                        .is_err()
                    {
                        break; // socket gone; stop the writer
                    }
                }
                OutboundOp::Post {
                    channel,
                    text,
                    part_index,
                } => {
                    if let Ok(ts) = api.post_message(&channel, &text).await {
                        if let Some(sink) = &ts_sink {
                            sink.record(part_index, ts);
                        }
                    }
                }
                OutboundOp::Update { channel, ts, text } => {
                    let _ = api.update_message(&channel, &ts, &text).await;
                }
            }
        }
    });

    WriterHandle { tx }
}
