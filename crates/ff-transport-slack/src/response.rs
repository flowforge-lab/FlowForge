//! `SlackResponseStream` — the [`ResponseStream`] the Router writes assistant
//! output into for a Slack channel (#912 T3, RFC 0021 §5.1).
//!
//! ## What a "chunk" is
//! The Router accumulates a turn's token deltas into one buffer and calls
//! [`ResponseStream::chunk`] with the **full text so far** (see
//! `ff-transport/src/router.rs`), not an incremental delta. So each `chunk`
//! call supersedes the previous one. This stream therefore treats the argument
//! as the authoritative current body and re-renders, rather than appending.
//!
//! ## Slack constraints this stream handles
//! - **3000-char message limit.** A body longer than [`SLACK_TEXT_LIMIT`] is
//!   split across multiple Slack messages: the first message is edited in place
//!   (`chat.update`), each overflow part is posted as a new message
//!   (`chat.postMessage`) that continues the reply.
//! - **Rate limits / edit churn.** Rapid successive `chunk` calls are coalesced:
//!   an edit is flushed at most once per [`EDIT_THROTTLE`]. The **final**
//!   [`ResponseStream::finish`] always flushes the last pending body so no text
//!   is lost to throttling.
//!
//! ## Sharing one socket (the #1058 core problem)
//! The stream never owns the WebSocket. It holds a clonable [`WriterHandle`]
//! (an mpsc sender into the single writer task) so the transport and a future
//! interactive approver both drive one connection without contending for a
//! mutable borrow. `chunk`/`finish` take `&self`; all mutable state lives behind
//! a `Mutex` (interior mutability), satisfying the `ResponseStream` contract.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use ff_transport::ResponseStream;

use crate::writer::{OutboundOp, WriterHandle};

/// Slack's hard per-message character limit for `text`.
pub const SLACK_TEXT_LIMIT: usize = 3000;

/// Minimum wall-clock gap between streamed `chat.update` edits. Rapid `chunk`
/// calls within this window coalesce into a single edit; `finish` bypasses it.
pub const EDIT_THROTTLE: Duration = Duration::from_millis(500);

/// A `chat` "message handle": the parts of a single logical reply already
/// posted to Slack, so subsequent edits target the right message `ts`.
#[derive(Debug, Default)]
struct Posted {
    /// `ts` of each Slack message that makes up this reply, in order. The first
    /// is edited in place on every flush; overflow parts are appended as new
    /// messages and thereafter also edited in place.
    ts: Vec<String>,
}

/// Mutable state guarded by a single mutex (interior mutability behind `&self`).
struct State {
    /// The most recent full body handed to `chunk`, not yet flushed.
    pending: Option<String>,
    /// The body last flushed to Slack, used to skip no-op edits.
    flushed: String,
    /// When the last flush happened, for throttling. `None` until the first.
    last_flush: Option<tokio::time::Instant>,
    /// Messages already posted for this reply.
    posted: Posted,
}

/// A response stream bound to one Slack channel, backed by the shared writer.
pub struct SlackResponseStream {
    channel: String,
    writer: WriterHandle,
    state: Mutex<State>,
}

impl SlackResponseStream {
    /// Open a stream for `channel`, sending through the shared `writer`.
    pub fn new(channel: impl Into<String>, writer: WriterHandle) -> Self {
        Self {
            channel: channel.into(),
            writer,
            state: Mutex::new(State {
                pending: None,
                flushed: String::new(),
                last_flush: None,
                posted: Posted::default(),
            }),
        }
    }

    /// Split `body` into Slack-sized parts on a char boundary, never mid-`char`.
    /// Parts are `<= SLACK_TEXT_LIMIT`; a body that fits returns a single part.
    fn split_parts(body: &str) -> Vec<String> {
        if body.len() <= SLACK_TEXT_LIMIT {
            return vec![body.to_string()];
        }
        let mut parts = Vec::new();
        let mut cur = String::new();
        for ch in body.chars() {
            if cur.len() + ch.len_utf8() > SLACK_TEXT_LIMIT {
                parts.push(std::mem::take(&mut cur));
            }
            cur.push(ch);
        }
        if !cur.is_empty() {
            parts.push(cur);
        }
        parts
    }

    /// Deliver the current `pending` body to Slack: edit the first message,
    /// post overflow parts as continuations, edit any part whose text changed.
    /// `force` skips the throttle (used by `finish`).
    async fn flush(&self, force: bool) {
        // Decide what to send and update bookkeeping under the lock; do the
        // awaits (channel sends) after dropping it — the guard is not Send.
        let ops: Vec<OutboundOp> = {
            let mut st = self.state.lock().unwrap();
            let Some(body) = st.pending.take() else {
                return;
            };
            if body == st.flushed {
                return; // nothing new to show
            }
            if !force {
                if let Some(last) = st.last_flush {
                    if last.elapsed() < EDIT_THROTTLE {
                        // Too soon: put the body back and let a later flush
                        // (or `finish`) deliver it. Coalesces edit churn.
                        st.pending = Some(body);
                        return;
                    }
                }
            }

            let parts = Self::split_parts(&body);
            let mut ops = Vec::with_capacity(parts.len());
            for (i, part) in parts.iter().enumerate() {
                match st.posted.ts.get(i) {
                    // A message already exists for this part → edit it.
                    Some(ts) => ops.push(OutboundOp::Update {
                        channel: self.channel.clone(),
                        ts: ts.clone(),
                        text: part.clone(),
                    }),
                    // New part → post a continuation. The writer task fills in
                    // the resulting `ts` and reports it back via `ts_sink`.
                    None => ops.push(OutboundOp::Post {
                        channel: self.channel.clone(),
                        text: part.clone(),
                        part_index: i,
                    }),
                }
            }
            st.flushed = body;
            st.last_flush = Some(tokio::time::Instant::now());
            ops
        };

        for op in ops {
            self.writer.send(op).await;
        }
    }

    /// Record a `ts` the writer assigned to a freshly posted part, so the next
    /// flush edits it in place instead of posting a duplicate.
    pub fn record_ts(&self, part_index: usize, ts: String) {
        let mut st = self.state.lock().unwrap();
        if st.posted.ts.len() == part_index {
            st.posted.ts.push(ts);
        } else if let Some(slot) = st.posted.ts.get_mut(part_index) {
            *slot = ts;
        }
    }
}

#[async_trait]
impl ResponseStream for SlackResponseStream {
    async fn chunk(&self, text: &str) {
        {
            let mut st = self.state.lock().unwrap();
            st.pending = Some(text.to_string());
        }
        self.flush(false).await;
    }

    async fn finish(&self) {
        // Force the final body out regardless of throttle so no text is lost.
        self.flush(true).await;
    }
}
