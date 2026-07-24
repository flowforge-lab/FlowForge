//! T3 integration tests (#912 T3, RFC 0021 §5.1): the streaming response
//! stream (chunking + throttle) and the Slack Web API round-trip.
//!
//! The parser fixtures live in `tests.rs`; this module exercises the live-path
//! pieces T3 adds. Chunk/throttle are tested against the raw `OutboundOp` queue
//! (no writer task, no network, `tokio::time` paused) so they are deterministic;
//! the Web API post/edit is tested against a `wiremock` server.

use std::time::Duration;

use ff_transport::ResponseStream;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::api::SlackApi;
use crate::response::{SlackResponseStream, SLACK_TEXT_LIMIT};
use crate::writer::{OutboundOp, WriterHandle};

/// Drain every op currently queued without blocking.
fn drain(rx: &mut tokio::sync::mpsc::Receiver<OutboundOp>) -> Vec<OutboundOp> {
    let mut out = Vec::new();
    while let Ok(op) = rx.try_recv() {
        out.push(op);
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn short_reply_posts_one_message() {
    let (writer, mut rx) = WriterHandle::channel_for_test();
    let stream = SlackResponseStream::new("C123", writer);

    stream.chunk("hello world").await;
    stream.finish().await;

    let ops = drain(&mut rx);
    // One part → one Post, no overflow.
    assert_eq!(ops.len(), 1, "expected a single post, got {ops:?}");
    match &ops[0] {
        OutboundOp::Post {
            channel,
            text,
            part_index,
        } => {
            assert_eq!(channel, "C123");
            assert_eq!(text, "hello world");
            assert_eq!(*part_index, 0);
        }
        other => panic!("expected Post, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reply_over_limit_splits_into_parts() {
    let (writer, mut rx) = WriterHandle::channel_for_test();
    let stream = SlackResponseStream::new("C1", writer);

    // 3000 + 500 chars → two parts (first exactly at the limit, second the rest).
    let body = "x".repeat(SLACK_TEXT_LIMIT + 500);
    stream.chunk(&body).await;
    stream.finish().await;

    let ops = drain(&mut rx);
    let posts: Vec<&OutboundOp> = ops
        .iter()
        .filter(|o| matches!(o, OutboundOp::Post { .. }))
        .collect();
    assert_eq!(posts.len(), 2, "expected 2 continuation parts, got {ops:?}");
    for op in &posts {
        if let OutboundOp::Post { text, .. } = op {
            assert!(
                text.len() <= SLACK_TEXT_LIMIT,
                "each part must respect the 3000-char limit, got {}",
                text.len()
            );
        }
    }
    // Parts are ordered 0,1.
    let indices: Vec<usize> = posts
        .iter()
        .filter_map(|o| match o {
            OutboundOp::Post { part_index, .. } => Some(*part_index),
            _ => None,
        })
        .collect();
    assert_eq!(indices, vec![0, 1]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multibyte_split_never_breaks_a_char() {
    let (writer, mut rx) = WriterHandle::channel_for_test();
    let stream = SlackResponseStream::new("C1", writer);

    // '€' is 3 bytes; a body of them forces a split that must land on a char
    // boundary or `String` construction would panic.
    let body = "€".repeat(SLACK_TEXT_LIMIT); // ~3x over the byte limit
    stream.chunk(&body).await;
    stream.finish().await;

    let ops = drain(&mut rx);
    let total: String = ops
        .iter()
        .filter_map(|o| match o {
            OutboundOp::Post { text, .. } => Some(text.clone()),
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
    let (writer, mut rx) = WriterHandle::channel_for_test();
    let stream = SlackResponseStream::new("C1", writer);

    // First chunk flushes immediately (no prior flush) → one Post(part 0).
    stream.chunk("v1").await;
    // Simulate the writer assigning a ts so later flushes edit in place.
    stream.record_ts(0, "111.000".to_string());

    // These arrive within EDIT_THROTTLE (time is paused, 0 elapsed) → coalesced,
    // no new ops emitted.
    stream.chunk("v1 v2").await;
    stream.chunk("v1 v2 v3").await;

    let after_rapid = drain(&mut rx);
    assert_eq!(
        after_rapid.len(),
        1,
        "first chunk posts once; the two rapid follow-ups coalesce: {after_rapid:?}"
    );
    assert!(matches!(after_rapid[0], OutboundOp::Post { .. }));

    // finish() bypasses the throttle and flushes the latest body as an edit.
    stream.finish().await;
    let after_finish = drain(&mut rx);
    assert_eq!(
        after_finish.len(),
        1,
        "finish flushes once: {after_finish:?}"
    );
    match &after_finish[0] {
        OutboundOp::Update { ts, text, .. } => {
            assert_eq!(ts, "111.000", "edit targets the posted message");
            assert_eq!(text, "v1 v2 v3", "final coalesced body wins");
        }
        other => panic!("expected an Update on finish, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn edit_after_throttle_window_flushes() {
    let (writer, mut rx) = WriterHandle::channel_for_test();
    let stream = SlackResponseStream::new("C1", writer);

    stream.chunk("first").await; // immediate post
    stream.record_ts(0, "222.000".to_string());
    let _ = drain(&mut rx);

    // Advance past the throttle window; the next chunk should flush as an edit.
    tokio::time::advance(Duration::from_millis(600)).await;
    stream.chunk("first second").await;

    let ops = drain(&mut rx);
    assert_eq!(ops.len(), 1, "a chunk past the window flushes: {ops:?}");
    match &ops[0] {
        OutboundOp::Update { ts, text, .. } => {
            assert_eq!(ts, "222.000");
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
        .post_message("C9", "streamed reply")
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
    let err = api.post_message("CBAD", "hi").await.unwrap_err();
    assert!(
        err.to_string().contains("channel_not_found"),
        "error should surface Slack's reason: {err}"
    );
}
