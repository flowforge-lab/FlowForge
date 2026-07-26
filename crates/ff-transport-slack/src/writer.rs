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

use futures_util::SinkExt;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::api::SlackApi;

/// One unit of outbound work for the writer task.
#[derive(Debug)]
pub enum OutboundOp {
    /// Acknowledge a Socket Mode envelope (must be sent on the socket, quickly,
    /// or Slack redelivers and eventually disconnects).
    Ack { envelope_id: String },
    /// Post a new message (a first reply, or an overflow continuation part).
    Post {
        channel: String,
        text: String,
        /// The writer sends the `ts` Slack assigned back through this channel so
        /// the posting response stream can edit the message in place next flush
        /// instead of posting a duplicate. Dropped (never sent) if the post
        /// fails — the receiver observes that as a closed channel.
        ts_tx: oneshot::Sender<String>,
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

/// Spawn the writer task and return a clonable handle onto it.
///
/// The task runs until its queue is closed (all handles dropped) or a socket
/// write fails. `api` performs the HTTPS Web API calls; the `ts` each post
/// returns is sent back through that op's `ts_tx` so the posting stream can edit
/// in place next flush.
pub fn spawn_writer<S>(mut ws_sink: S, api: SlackApi) -> WriterHandle
where
    S: futures_util::Sink<Message> + Unpin + Send + 'static,
{
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
                    ts_tx,
                } => {
                    if let Ok(ts) = api.post_message(&channel, &text).await {
                        // Report the assigned `ts` back to the posting stream. A
                        // send error means the stream is gone (nothing to edit);
                        // ignore it.
                        let _ = ts_tx.send(ts);
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
