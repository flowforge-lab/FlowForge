//! T3 integration tests (#912 T3, RFC 0021 §5.1): the streaming response
//! stream (chunking + throttle) and the Slack Web API round-trip.
//!
//! The parser fixtures live in `tests.rs`; this module exercises the live-path
//! pieces T3 adds. Chunk/throttle are tested against the raw `OutboundOp` queue
//! (no writer task, no network, `tokio::time` paused) so they are deterministic;
//! the Web API post/edit is tested against a `wiremock` server.

use std::time::Duration;

use ff_transport::ResponseStream;
use tokio::sync::mpsc;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::api::SlackApi;
use crate::response::{SlackResponseStream, EDIT_THROTTLE, SLACK_TEXT_LIMIT};
use crate::writer::{OutboundOp, WriterHandle};

/// A writer-task stand-in for the stream tests.
///
/// The real writer, on a `Post`, calls `chat.postMessage`, gets a `ts`, and
/// reports it back through the op's `ts_tx`. `SlackResponseStream::flush` now
/// *awaits* that `ts` before returning (so the next flush edits in place rather
/// than posting a duplicate — the bug this all guards). A test that fed a bare
/// receiver would therefore deadlock. This double closes the loop: it answers
/// every `Post` with a synthetic, monotonically increasing `ts` and mirrors each
/// op — as an inspectable [`SentOp`] with the `ts` it assigned — onto a channel
/// the test drains.
#[derive(Debug, Clone, PartialEq)]
enum SentOp {
    Ack {
        envelope_id: String,
    },
    Post {
        channel: String,
        thread_ts: Option<String>,
        text: String,
        ts: String,
    },
    Update {
        channel: String,
        ts: String,
        text: String,
    },
}

/// Spawn the double. Returns the [`WriterHandle`] the stream posts through and a
/// receiver of the mirrored [`SentOp`]s, in the order the double handled them.
fn auto_answer_writer() -> (WriterHandle, mpsc::Receiver<SentOp>) {
    let (writer, mut raw_rx) = WriterHandle::channel_for_test();
    let (obs_tx, obs_rx) = mpsc::channel::<SentOp>(256);
    tokio::spawn(async move {
        let mut next_ts = 100u64;
        while let Some(op) = raw_rx.recv().await {
            let sent = match op {
                OutboundOp::Ack { envelope_id } => SentOp::Ack { envelope_id },
                OutboundOp::Post {
                    channel,
                    thread_ts,
                    text,
                    ts_tx,
                } => {
                    next_ts += 1;
                    let ts = format!("{next_ts}.000");
                    // Mirror the real writer: report the assigned ts back so the
                    // stream records it. Ignore send errors (stream gone).
                    let _ = ts_tx.send(ts.clone());
                    SentOp::Post {
                        channel,
                        thread_ts,
                        text,
                        ts,
                    }
                }
                OutboundOp::Update { channel, ts, text } => SentOp::Update { channel, ts, text },
            };
            if obs_tx.send(sent).await.is_err() {
                break;
            }
        }
    });
    (writer, obs_rx)
}

/// Drain every mirrored op the double has handled so far, without blocking.
fn drain(rx: &mut mpsc::Receiver<SentOp>) -> Vec<SentOp> {
    let mut out = Vec::new();
    while let Ok(op) = rx.try_recv() {
        out.push(op);
    }
    out
}

/// Block until at least `n` ops have been mirrored, then return all drained so
/// far. Because `flush` awaits each `Post`'s `ts`, once the stream call returns
/// the double has already handled and mirrored that op — so a bounded wait is a
/// deterministic sync point, not a race.
async fn recv_at_least(rx: &mut mpsc::Receiver<SentOp>, n: usize) -> Vec<SentOp> {
    let mut out = Vec::new();
    while out.len() < n {
        match rx.recv().await {
            Some(op) => out.push(op),
            None => break,
        }
    }
    out.extend(drain(rx));
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn short_reply_posts_one_message() {
    let (writer, mut rx) = auto_answer_writer();
    let stream = SlackResponseStream::new("C123", None, writer);

    stream.chunk("hello world").await;
    stream.finish().await;

    let ops = recv_at_least(&mut rx, 1).await;
    // One part → one Post; finish re-flushes the same body, which is a no-op
    // (nothing changed), so no duplicate Post and no spurious Update.
    assert_eq!(ops.len(), 1, "expected a single post, got {ops:?}");
    match &ops[0] {
        SentOp::Post { channel, text, .. } => {
            assert_eq!(channel, "C123");
            assert_eq!(text, "hello world");
        }
        other => panic!("expected Post, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reply_over_limit_splits_into_parts() {
    let (writer, mut rx) = auto_answer_writer();
    let stream = SlackResponseStream::new("C1", None, writer);

    // 3000 + 500 chars → two parts (first exactly at the limit, second the rest).
    let body = "x".repeat(SLACK_TEXT_LIMIT + 500);
    stream.chunk(&body).await;
    stream.finish().await;

    let ops = recv_at_least(&mut rx, 2).await;
    let posts: Vec<&SentOp> = ops
        .iter()
        .filter(|o| matches!(o, SentOp::Post { .. }))
        .collect();
    assert_eq!(posts.len(), 2, "expected 2 continuation parts, got {ops:?}");
    for op in &posts {
        if let SentOp::Post { text, .. } = op {
            assert!(
                text.len() <= SLACK_TEXT_LIMIT,
                "each part must respect the 3000-char limit, got {}",
                text.len()
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_overflow_part_carries_the_same_thread_anchor() {
    // A reply split across parts must post every part into the same thread as the
    // trigger, not just the first (#1098, AC 2).
    let (writer, mut rx) = auto_answer_writer();
    let stream = SlackResponseStream::new("C1", Some("111.222".to_string()), writer);

    let body = "x".repeat(SLACK_TEXT_LIMIT + 500);
    stream.chunk(&body).await;
    stream.finish().await;

    let ops = recv_at_least(&mut rx, 2).await;
    let posts: Vec<&SentOp> = ops
        .iter()
        .filter(|o| matches!(o, SentOp::Post { .. }))
        .collect();
    assert_eq!(posts.len(), 2, "expected 2 continuation parts, got {ops:?}");
    for op in &posts {
        if let SentOp::Post { thread_ts, .. } = op {
            assert_eq!(
                thread_ts.as_deref(),
                Some("111.222"),
                "every part must anchor to the trigger's thread"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_anchor_posts_to_the_channel_root() {
    // A transport with no thread anchor (or a non-Slack caller) posts to the
    // channel root exactly as before — threading is additive, not mandatory.
    let (writer, mut rx) = auto_answer_writer();
    let stream = SlackResponseStream::new("C1", None, writer);

    stream.chunk("hi").await;
    stream.finish().await;

    let ops = recv_at_least(&mut rx, 1).await;
    let post = ops
        .iter()
        .find(|o| matches!(o, SentOp::Post { .. }))
        .expect("one post");
    if let SentOp::Post { thread_ts, .. } = post {
        assert_eq!(thread_ts.as_deref(), None, "no anchor → channel root");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_message_sends_thread_ts_to_slack() {
    // AC 1: chat.postMessage carries thread_ts so the reply lands in the thread.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .and(body_string_contains("\"thread_ts\":\"1548261231.000200\""))
        .and(body_string_contains("threaded reply"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "ok": true, "ts": "999.001" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let api = SlackApi::new("xoxb-test").with_base(server.uri());
    let ts = api
        .post_message("C9", Some("1548261231.000200"), "threaded reply")
        .await
        .expect("post ok");
    assert_eq!(ts, "999.001");
    // `.expect(1)` on drop verifies the thread_ts-bearing body was sent exactly once.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_message_without_thread_ts_omits_the_field() {
    // The un-threaded path must not send a thread_ts key at all (not "null", not
    // empty) — Slack treats a present-but-empty thread_ts as an error.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .and(move |req: &wiremock::Request| {
            let v: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            v.get("thread_ts").is_none()
        })
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "ok": true, "ts": "999.002" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let api = SlackApi::new("xoxb-test").with_base(server.uri());
    api.post_message("C9", None, "plain reply")
        .await
        .expect("post ok");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multibyte_split_never_breaks_a_char() {
    let (writer, mut rx) = auto_answer_writer();
    let stream = SlackResponseStream::new("C1", None, writer);

    // '€' is 3 bytes; a body of them forces a split that must land on a char
    // boundary or `String` construction would panic.
    let body = "€".repeat(SLACK_TEXT_LIMIT); // ~3x over the byte limit
    stream.chunk(&body).await;
    stream.finish().await;

    let ops = recv_at_least(&mut rx, 3).await;
    let total: String = ops
        .iter()
        .filter_map(|o| match o {
            SentOp::Post { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    // Reassembled parts equal the original — nothing lost or corrupted.
    assert_eq!(total, body);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn rapid_chunks_coalesce_then_finish_flushes() {
    // With time paused, successive `chunk`s inside the throttle window must
    // coalesce to a single edit; `finish` then forces the final body out.
    let (writer, mut rx) = auto_answer_writer();
    let stream = SlackResponseStream::new("C1", None, writer);

    // First chunk flushes immediately (no prior flush) → one Post(part 0). The
    // double answers its ts, so the stream now knows to edit rather than re-post.
    stream.chunk("v1").await;
    let first = recv_at_least(&mut rx, 1).await;
    assert_eq!(first.len(), 1);
    let posted_ts = match &first[0] {
        SentOp::Post { ts, .. } => ts.clone(),
        other => panic!("expected initial Post, got {other:?}"),
    };

    // These arrive within EDIT_THROTTLE (time is paused, 0 elapsed) → coalesced,
    // no new ops emitted.
    stream.chunk("v1 v2").await;
    stream.chunk("v1 v2 v3").await;

    let after_rapid = drain(&mut rx);
    assert!(
        after_rapid.is_empty(),
        "rapid follow-ups within the window coalesce, emitting nothing: {after_rapid:?}"
    );

    // finish() bypasses the throttle and flushes the latest body as an edit.
    stream.finish().await;
    let after_finish = recv_at_least(&mut rx, 1).await;
    assert_eq!(
        after_finish.len(),
        1,
        "finish flushes once: {after_finish:?}"
    );
    match &after_finish[0] {
        SentOp::Update { ts, text, .. } => {
            assert_eq!(*ts, posted_ts, "edit targets the posted message");
            assert_eq!(text, "v1 v2 v3", "final coalesced body wins");
        }
        other => panic!("expected an Update on finish, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn edit_after_throttle_window_flushes() {
    let (writer, mut rx) = auto_answer_writer();
    let stream = SlackResponseStream::new("C1", None, writer);

    stream.chunk("first").await; // immediate post
    let first = recv_at_least(&mut rx, 1).await;
    let posted_ts = match &first[0] {
        SentOp::Post { ts, .. } => ts.clone(),
        other => panic!("expected initial Post, got {other:?}"),
    };

    // Advance past the throttle window; the next chunk should flush as an edit.
    tokio::time::advance(Duration::from_millis(600)).await;
    stream.chunk("first second").await;

    let ops = recv_at_least(&mut rx, 1).await;
    assert_eq!(ops.len(), 1, "a chunk past the window flushes: {ops:?}");
    match &ops[0] {
        SentOp::Update { ts, text, .. } => {
            assert_eq!(*ts, posted_ts);
            assert_eq!(text, "first second");
        }
        other => panic!("expected Update, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_api_round_trip_post_and_update() {
    // The Slack Web API side: post returns a ts; update edits it. Uses a
    // wiremock server so we assert the transport actually calls the endpoints
    // with the right payloads.
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .and(body_string_contains("\"channel\":\"C9\""))
        .and(body_string_contains("streamed reply"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "ok": true, "ts": "333.001" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat.update"))
        .and(body_string_contains("\"ts\":\"333.001\""))
        .and(body_string_contains("edited reply"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
        .expect(1)
        .mount(&server)
        .await;

    let api = SlackApi::new("xoxb-test").with_base(server.uri());

    let ts = api
        .post_message("C9", None, "streamed reply")
        .await
        .expect("post ok");
    assert_eq!(ts, "333.001");
    api.update_message("C9", &ts, "edited reply")
        .await
        .expect("update ok");
    // wiremock's `.expect(1)` verifies both endpoints were hit exactly once on drop.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_api_surfaces_slack_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "ok": false, "error": "channel_not_found" })),
        )
        .mount(&server)
        .await;

    let api = SlackApi::new("xoxb-test").with_base(server.uri());
    let err = api.post_message("CBAD", None, "hi").await.unwrap_err();
    assert!(
        err.to_string().contains("channel_not_found"),
        "error should surface Slack's reason: {err}"
    );
}

/// End-to-end regression lock for the `ts` round-trip (Isaac review #1/#2): the
/// **real** writer task + **real** `SlackApi` (pointed at a wiremock) + **real**
/// `SlackResponseStream`. A first flush posts; a later flush must *edit that same
/// message* — i.e. exactly one `chat.postMessage` then one `chat.update`.
///
/// Before the fix, the writer never reported the assigned `ts` back to the
/// stream, so the second flush posted a *duplicate* instead of editing. That
/// bug produced two `chat.postMessage` calls and zero `chat.update` — which this
/// test's `.expect(1)` mounts would fail on. (Verified by mutation: dropping the
/// `ts_tx.send` in the writer turns this red.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_to_end_post_then_edit_through_real_writer() {
    use futures_util::sink::drain;
    use tokio_tungstenite::tungstenite::Message;

    let server = MockServer::start().await;

    // postMessage returns a ts; assert it's called exactly once.
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "ok": true, "ts": "1700000000.000100" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    // The edit must target the ts postMessage returned, and fire exactly once.
    Mock::given(method("POST"))
        .and(path("/chat.update"))
        .and(body_string_contains("1700000000.000100"))
        .and(body_string_contains("hello world"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "ok": true, "ts": "1700000000.000100" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    // Real writer over a no-op socket sink; real API at the mock server.
    let api = SlackApi::new("xoxb-test").with_base(server.uri());
    let writer = crate::writer::spawn_writer(drain::<Message>(), api);
    let stream = SlackResponseStream::new("C1", None, writer);

    // First flush → postMessage; the writer reports the ts back and the stream
    // records it (flush awaits that before returning).
    stream.chunk("hello").await;
    // A later, changed body must edit the same message, not post a new one.
    tokio::time::sleep(Duration::from_millis(EDIT_THROTTLE.as_millis() as u64 + 50)).await;
    stream.chunk("hello world").await;
    stream.finish().await;

    // Give the async writer task a beat to drive the chat.update call before the
    // server's `.expect(1)` verification on drop.
    tokio::time::sleep(Duration::from_millis(100)).await;
    // MockServer verifies the `.expect(1)` counts on drop: exactly one post,
    // exactly one update. Two posts (the old bug) or zero updates fails here.
}

// ── reader demux (#912 T3 acceptance: envelopes fan out; interactions split
// off the Router path; every inbound frame is acked) ──────────────────────────
//
// `spawn_reader` is generic over the frame stream, so these drive it with a
// scripted in-memory stream instead of a live TLS socket — the reader path the
// PR claims to deliver but previously had zero coverage.
mod reader {
    use super::*;
    use crate::envelope::SlackInteraction;
    use crate::transport::SlackTransport;
    use ff_transport::InboundMessage;
    use tokio_tungstenite::tungstenite::Message;

    const USER_MESSAGE: &str = r#"{
      "envelope_id": "env-msg-1",
      "type": "events_api",
      "payload": {
        "type": "event_callback",
        "event": {
          "type": "message",
          "channel": "C01234567",
          "user": "U99999999",
          "text": "deploy the thing",
          "ts": "1548261231.000200"
        }
      }
    }"#;

    const BLOCK_ACTIONS: &str = r#"{
      "envelope_id": "env-int-1",
      "type": "interactive",
      "payload": {
        "type": "block_actions",
        "user": { "id": "U99999999", "username": "tony" },
        "channel": { "id": "C01234567", "name": "dev" },
        "message": { "ts": "1548261231.000200", "text": "Approve deploy?" },
        "response_url": "https://hooks.slack.com/actions/T0/1/xyz",
        "actions": [
          { "type": "button", "action_id": "approve", "block_id": "gate", "value": "decision-42" }
        ]
      }
    }"#;

    /// Feed a scripted list of text frames through `spawn_reader` and return the
    /// three receivers: inbound messages, interactions, and the writer's ops
    /// (where acks land). `Never` is an error type the stream never produces.
    ///
    /// Allowlists the scripted sender (`U99999999`) so these demux tests exercise
    /// the fan-out, not the §10 gate; [`allowlist`] covers the gate itself.
    enum Never {}
    fn run_reader(
        frames: Vec<Message>,
    ) -> (
        mpsc::Receiver<InboundMessage>,
        mpsc::Receiver<SlackInteraction>,
        mpsc::Receiver<SentOp>,
    ) {
        run_reader_with_allowlist(frames, ["U99999999"])
    }

    fn run_reader_with_allowlist<I, S>(
        frames: Vec<Message>,
        allowed: I,
    ) -> (
        mpsc::Receiver<InboundMessage>,
        mpsc::Receiver<SlackInteraction>,
        mpsc::Receiver<SentOp>,
    )
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let stream = futures_util::stream::iter(
            frames
                .into_iter()
                .map(Ok::<Message, Never>)
                .collect::<Vec<_>>(),
        );
        let (writer, ops_rx) = auto_answer_writer();
        let (inbound_tx, inbound_rx) = mpsc::channel::<InboundMessage>(64);
        let (interaction_tx, interaction_rx) = mpsc::channel::<SlackInteraction>(64);
        SlackTransport::spawn_reader(
            stream,
            writer,
            inbound_tx,
            interaction_tx,
            allowed.into_iter().map(Into::into).collect(),
        );
        (inbound_rx, interaction_rx, ops_rx)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn message_envelope_fans_out_and_is_acked() {
        let (mut inbound_rx, _int_rx, mut ops_rx) =
            run_reader(vec![Message::Text(USER_MESSAGE.to_string())]);

        let msg = inbound_rx
            .recv()
            .await
            .expect("a message on the inbound path");
        assert_eq!(msg.text, "deploy the thing");
        assert_eq!(msg.channel.platform_id, "C01234567");
        assert_eq!(msg.sender_id, "U99999999");

        // Every inbound envelope must be acked on the socket.
        let ops = recv_at_least(&mut ops_rx, 1).await;
        assert!(
            ops.iter()
                .any(|o| matches!(o, SentOp::Ack { envelope_id } if envelope_id == "env-msg-1")),
            "message envelope must be acked: {ops:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interaction_splits_off_router_path_and_is_acked() {
        let (mut inbound_rx, mut int_rx, mut ops_rx) =
            run_reader(vec![Message::Text(BLOCK_ACTIONS.to_string())]);

        let int = int_rx
            .recv()
            .await
            .expect("an interaction on the side path");
        assert_eq!(int.action_id, "approve");
        assert_eq!(int.value.as_deref(), Some("decision-42"));
        assert_eq!(int.channel.platform_id, "C01234567");

        let ops = recv_at_least(&mut ops_rx, 1).await;
        assert!(
            ops.iter()
                .any(|o| matches!(o, SentOp::Ack { envelope_id } if envelope_id == "env-int-1")),
            "interaction envelope must be acked: {ops:?}"
        );

        // Interactions must NOT reach the Router's inbound queue.
        assert!(
            inbound_rx.try_recv().is_err(),
            "interaction must not land on the inbound (Router) path"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hello_has_no_side_effects() {
        let hello = r#"{ "type": "hello" }"#;
        let (mut inbound_rx, mut int_rx, mut ops_rx) =
            run_reader(vec![Message::Text(hello.to_string())]);

        // Nothing fanned out, nothing acked; the stream then ends.
        assert!(inbound_rx.recv().await.is_none());
        assert!(int_rx.try_recv().is_err());
        assert!(drain(&mut ops_rx).is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disconnect_ends_the_reader_loop() {
        // A disconnect before a second message means the second is never seen.
        let disconnect = r#"{ "type": "disconnect", "reason": "refresh" }"#;
        let (mut inbound_rx, _int_rx, _ops_rx) = run_reader(vec![
            Message::Text(disconnect.to_string()),
            Message::Text(USER_MESSAGE.to_string()),
        ]);
        assert!(
            inbound_rx.recv().await.is_none(),
            "reader stops at disconnect; the trailing message is never delivered"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn undrained_interactions_never_wedge_the_reader() {
        // Regression (Isaac review): interactions fan out on a bounded queue that
        // no one drains until T4. A blocking `send().await` would wedge the
        // reader once that queue filled (64), starving the *inbound* path too.
        // Feed well over capacity, then a user message, and assert the message
        // still arrives — the reader must drop-and-continue, not block.
        let mut frames: Vec<Message> = (0..200)
            .map(|_| Message::Text(BLOCK_ACTIONS.to_string()))
            .collect();
        frames.push(Message::Text(USER_MESSAGE.to_string()));

        // Note: `_int_rx` is intentionally never drained here.
        let (mut inbound_rx, _int_rx, _ops_rx) = run_reader(frames);

        let msg = tokio::time::timeout(Duration::from_secs(5), inbound_rx.recv())
            .await
            .expect("reader must not wedge on a full interaction queue")
            .expect("the user message must still reach the inbound path");
        assert_eq!(msg.text, "deploy the thing");
    }
    /// RFC 0021 §10: only allowlisted users may drive the agent. Missed by T2/T3
    /// (#1144) — `sender_id` was parsed and then never checked, so anyone in a
    /// channel the bot could see could start a turn.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_sender_outside_the_allowlist_never_becomes_a_turn() {
        // Scripted sender is U99999999; allowlist holds someone else.
        let (mut inbound_rx, _int_rx, _ops_rx) =
            run_reader_with_allowlist(vec![Message::Text(USER_MESSAGE.to_string())], ["USOMEONE"]);

        // The channel closes when the reader finishes the scripted frames, so a
        // `None` here means "no message was ever forwarded" rather than a stall.
        assert!(
            inbound_rx.recv().await.is_none(),
            "a non-allowlisted sender must not reach the Router"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_rejected_sender_is_still_acked() {
        // Slack redelivers an unacked envelope, so dropping the ack would make a
        // rejected message arrive again in a loop rather than go away.
        let (_inbound_rx, _int_rx, mut ops_rx) =
            run_reader_with_allowlist(vec![Message::Text(USER_MESSAGE.to_string())], ["USOMEONE"]);

        let op = tokio::time::timeout(Duration::from_secs(5), ops_rx.recv())
            .await
            .expect("the reader must ack even when it rejects the sender")
            .expect("an ack op must be sent");
        assert!(
            matches!(op, SentOp::Ack { .. }),
            "expected an ack for the rejected envelope, got {op:?}"
        );
    }

    /// The empty allowlist is the misconfigured-deploy case: it must deny
    /// everyone, not fall open to everyone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_empty_allowlist_denies_everyone() {
        let (mut inbound_rx, _int_rx, _ops_rx) = run_reader_with_allowlist(
            vec![Message::Text(USER_MESSAGE.to_string())],
            Vec::<String>::new(),
        );

        assert!(
            inbound_rx.recv().await.is_none(),
            "an empty allowlist must fail closed"
        );
    }
}

/// Graceful shutdown on the Slack path (#1060 scope bullet 4).
///
/// The Mock transport covers the router-side contract; these cover *this*
/// implementation of it, which is where the ordering subtlety lives: a shutdown
/// must not jump the queue ahead of messages Slack already delivered.
mod shutdown {
    use ff_transport::{MessageTransport, ShutdownHandle};
    use tokio::sync::mpsc;

    use crate::transport::SlackTransport;
    use ff_transport::{ChannelId, InboundMessage};

    fn msg(text: &str) -> InboundMessage {
        InboundMessage {
            channel: ChannelId::new("slack", "C1"),
            sender_id: "U1".into(),
            text: text.into(),
            timestamp: 0,
            reply_thread: None,
        }
    }

    /// A transport wired to an inbound channel, as `connect` leaves it.
    fn connected() -> (SlackTransport, mpsc::Sender<InboundMessage>, ShutdownHandle) {
        let (tx, rx) = mpsc::channel::<InboundMessage>(8);
        let mut transport = SlackTransport::new("xapp-t", "xoxb-t").with_inbound_for_test(rx);
        let handle = transport.shutdown_handle();
        (transport, tx, handle)
    }

    #[tokio::test]
    async fn a_shutdown_signal_ends_the_receive_loop() {
        let (mut transport, _tx, handle) = connected();

        handle.shutdown();

        assert!(
            transport.recv().await.is_none(),
            "after shutdown `recv` must report closed so `Router::run` returns"
        );
    }

    /// The `biased` select plus the leading `try_recv` exist for this case: a
    /// message Slack already acked must not be dropped because Ctrl-C arrived
    /// while it sat in the buffer. Losing it would mean the user's request was
    /// acknowledged and then silently discarded.
    #[tokio::test]
    async fn buffered_messages_are_delivered_before_the_shutdown_takes_effect() {
        let (mut transport, tx, handle) = connected();

        tx.send(msg("first")).await.unwrap();
        tx.send(msg("second")).await.unwrap();
        handle.shutdown();

        assert_eq!(
            transport.recv().await.map(|m| m.text),
            Some("first".to_string()),
            "a buffered message must win over a pending shutdown"
        );
        assert_eq!(
            transport.recv().await.map(|m| m.text),
            Some("second".to_string()),
            "the whole buffer must drain, not just the first entry"
        );
        assert!(
            transport.recv().await.is_none(),
            "once drained, the shutdown takes effect"
        );
    }

    /// Without a handle the transport must behave exactly as before, so a host
    /// that never asks for one is unaffected.
    #[tokio::test]
    async fn without_a_handle_recv_still_blocks_on_the_channel() {
        let (tx, rx) = mpsc::channel::<InboundMessage>(8);
        let mut transport = SlackTransport::new("xapp-t", "xoxb-t").with_inbound_for_test(rx);

        tx.send(msg("only")).await.unwrap();
        assert_eq!(
            transport.recv().await.map(|m| m.text),
            Some("only".to_string())
        );
    }
}
