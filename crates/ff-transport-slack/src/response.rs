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
//!
//! ## Known limitations (T3 scope; RFC 0021 §9 defers)
//! - **Ordering assumption.** `chunk`/`finish` are expected to be called
//!   sequentially for a given stream, as the Router does (one turn drives one
//!   response to completion). The throttle decision reads `last_flush` under the
//!   lock, but the post/edit awaits happen after the lock is dropped; truly
//!   concurrent callers on the *same* stream could therefore interleave a
//!   post-vs-edit. That does not arise on the single sequential Router path.
//! - **No client-side rate limiting.** Slack's ~1 msg/sec-per-channel (burst)
//!   guidance is not enforced here; the [`EDIT_THROTTLE`] coalescing bounds edit
//!   churn but does not cap posts. Global rate limiting is deferred to Phase 2.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use ff_transport::ResponseStream;
use tokio::sync::oneshot;

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
    /// Thread anchor for every post in this reply (#1098). `None` when the trigger
    /// had no thread (non-Slack callers, or a pre-connect fallback), leaving posts
    /// un-threaded exactly as before.
    thread_ts: Option<String>,
    writer: WriterHandle,
    state: Mutex<State>,
}

impl SlackResponseStream {
    /// Open a stream for `channel`, posting into `thread_ts` if set, through the
    /// shared `writer`.
    pub fn new(
        channel: impl Into<String>,
        thread_ts: Option<String>,
        writer: WriterHandle,
    ) -> Self {
        Self {
            channel: channel.into(),
            thread_ts,
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
        //
        // A `Post` step carries its part index so that, once the writer reports
        // the `ts` Slack assigned, we can record it against the right part and
        // edit that message in place on the next flush instead of posting a
        // duplicate.
        enum Step {
            Update { ts: String, text: String },
            Post { part_index: usize, text: String },
        }
        let steps: Vec<Step> = {
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
            let mut steps = Vec::with_capacity(parts.len());
            for (i, part) in parts.iter().enumerate() {
                match st.posted.ts.get(i) {
                    // A message already exists for this part → edit it.
                    Some(ts) => steps.push(Step::Update {
                        ts: ts.clone(),
                        text: part.clone(),
                    }),
                    // New part → post a continuation and learn its `ts`.
                    None => steps.push(Step::Post {
                        part_index: i,
                        text: part.clone(),
                    }),
                }
            }
            st.flushed = body;
            st.last_flush = Some(tokio::time::Instant::now());
            steps
        };

        for step in steps {
            match step {
                Step::Update { ts, text } => {
                    self.writer
                        .send(OutboundOp::Update {
                            channel: self.channel.clone(),
                            ts,
                            text,
                        })
                        .await;
                }
                Step::Post { part_index, text } => {
                    // Await the assigned `ts` before the next flush so we edit
                    // this part in place rather than posting a duplicate.
                    let (ts_tx, ts_rx) = oneshot::channel();
                    self.writer
                        .send(OutboundOp::Post {
                            channel: self.channel.clone(),
                            thread_ts: self.thread_ts.clone(),
                            text,
                            ts_tx,
                        })
                        .await;
                    match ts_rx.await {
                        Ok(ts) => self.record_ts(part_index, ts),
                        // Writer dropped the sender: the post failed or the
                        // socket is gone. Nothing to record; the next flush will
                        // retry a post for this part.
                        Err(_) => tracing::warn!(
                            channel = %self.channel,
                            part_index,
                            "slack post produced no ts (socket closed or post failed); \
                             part will re-post on the next flush"
                        ),
                    }
                }
            }
        }
    }

    /// Record a `ts` the writer assigned to a freshly posted part, so the next
    /// flush edits it in place instead of posting a duplicate.
    fn record_ts(&self, part_index: usize, ts: String) {
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
