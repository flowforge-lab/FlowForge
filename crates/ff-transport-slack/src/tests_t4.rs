//! T4 approver tests (#1059, RFC 0021 §5.2).
//!
//! The approver's contract has three tiers and they are tested separately:
//!
//! 1. **The hard override** — `Publish`/`Dangerous` are denied in every mode. This
//!    is the tier that justifies the type existing, so it is pinned against the
//!    *default matrix values it contradicts* (`Act/Publish = Allow`,
//!    `Act/Dangerous = Ask`), not against a hand-built matrix that would agree
//!    with it anyway.
//! 2. **The matrix passthrough** — `Allow`/`Deny` cells resolve with no prompt.
//!    Pinned by asserting zero HTTP calls, since "did not prompt" is otherwise
//!    indistinguishable from "prompted and got lucky".
//! 3. **The prompt round-trip** — buttons render, a click resolves, and anything
//!    that is *not* the awaited click is discarded rather than accepted.

use std::sync::Arc;
use std::time::Duration;

use ff_agent::Approver;
use ff_core::{Mode, PermissionCell, PermissionMatrix, Safety};
use ff_transport::ChannelId;
use tokio::sync::mpsc;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::api::SlackApi;
use crate::approver::{SlackApprover, ACTION_APPROVE, ACTION_DENY};
use crate::envelope::SlackInteraction;

fn channel() -> ChannelId {
    ChannelId {
        transport: "slack".into(),
        platform_id: "C9".into(),
    }
}

fn interaction(action_id: &str, value: Option<&str>) -> SlackInteraction {
    SlackInteraction {
        action_id: action_id.into(),
        value: value.map(Into::into),
        channel: channel(),
        user_id: "U1".into(),
        message_ts: Some("100.1".into()),
        response_url: None,
    }
}

/// A server that answers `chat.postMessage` and counts the calls.
async fn prompt_server(expect: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "ok": true, "ts": "900.1" })),
        )
        .expect(expect)
        .mount(&server)
        .await;
    server
}

fn approver(
    server: &MockServer,
    mode: Mode,
    matrix: PermissionMatrix,
) -> (SlackApprover, mpsc::Sender<SlackInteraction>) {
    let (tx, rx) = mpsc::channel(8);
    let api = SlackApi::new("xoxb-test").with_base(server.uri());
    let approver = SlackApprover::new(api, channel(), mode, matrix, rx)
        .with_timeout(Duration::from_millis(200));
    (approver, tx)
}

// ---------------------------------------------------------------------------
// Tier 1: the hard override
// ---------------------------------------------------------------------------

#[tokio::test]
async fn act_mode_publish_is_denied_even_though_the_matrix_allows_it() {
    // The premise, asserted rather than assumed: the default matrix would let
    // this through autonomously. If this assert ever fails the test below stops
    // proving anything, so it is checked in-test.
    let matrix = PermissionMatrix::default();
    assert_eq!(
        matrix.effective_cell("github", Mode::Act, Safety::Publish),
        PermissionCell::Allow,
        "premise: Act/Publish is Allow by default, so the override below is what denies it"
    );

    let server = prompt_server(0).await;
    let (approver, _tx) = approver(&server, Mode::Act, matrix);

    let decision = approver
        .approve(
            "m1",
            "c1",
            "github",
            Safety::Publish,
            &serde_json::json!({}),
        )
        .await;

    assert!(
        !decision,
        "a shared channel button must not authorize Publish"
    );
    // `.expect(0)` verifies on drop that we did not even ask.
}

#[tokio::test]
async fn act_mode_dangerous_is_denied_without_prompting() {
    let matrix = PermissionMatrix::default();
    assert_eq!(
        matrix.effective_cell("bash", Mode::Act, Safety::Dangerous),
        PermissionCell::Ask,
        "premise: Act/Dangerous prompts by default; the override must pre-empt that"
    );

    let server = prompt_server(0).await;
    let (approver, _tx) = approver(&server, Mode::Act, matrix);

    let decision = approver
        .approve(
            "m1",
            "c1",
            "bash",
            Safety::Dangerous,
            &serde_json::json!({}),
        )
        .await;

    assert!(!decision);
}

#[tokio::test]
async fn the_override_holds_in_every_mode() {
    for mode in [Mode::Plan, Mode::Act, Mode::Auto] {
        for safety in [Safety::Publish, Safety::Dangerous] {
            let server = prompt_server(0).await;
            let (approver, _tx) = approver(&server, mode, PermissionMatrix::default());
            let decision = approver
                .approve("t", "c", "tool", safety, &serde_json::json!({}))
                .await;
            assert!(
                !decision,
                "{mode:?}/{safety:?} must be denied over a channel button"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tier 2: matrix passthrough, no prompt
// ---------------------------------------------------------------------------

#[tokio::test]
async fn readonly_is_allowed_without_prompting() {
    let server = prompt_server(0).await;
    let (approver, _tx) = approver(&server, Mode::Plan, PermissionMatrix::default());

    let decision = approver
        .approve("m1", "c1", "view", Safety::ReadOnly, &serde_json::json!({}))
        .await;

    assert!(decision);
}

#[tokio::test]
async fn plan_mode_write_is_denied_without_prompting() {
    let matrix = PermissionMatrix::default();
    assert_eq!(
        matrix.effective_cell("write", Mode::Plan, Safety::Write),
        PermissionCell::Deny,
        "premise: Plan denies Write outright, so no button should be posted"
    );

    let server = prompt_server(0).await;
    let (approver, _tx) = approver(&server, Mode::Plan, matrix);

    let decision = approver
        .approve("m1", "c1", "write", Safety::Write, &serde_json::json!({}))
        .await;

    assert!(!decision);
}

#[tokio::test]
async fn auto_mode_write_stays_autonomous() {
    // #1041: Auto must not start prompting for Write just because the surface is
    // remote. Only the Publish/Dangerous tier is clamped.
    let matrix = PermissionMatrix::default();
    assert_eq!(
        matrix.effective_cell("write", Mode::Auto, Safety::Write),
        PermissionCell::Allow
    );

    let server = prompt_server(0).await;
    let (approver, _tx) = approver(&server, Mode::Auto, matrix);

    let decision = approver
        .approve("m1", "c1", "write", Safety::Write, &serde_json::json!({}))
        .await;

    assert!(decision);
}

// ---------------------------------------------------------------------------
// Tier 3: the prompt round-trip
// ---------------------------------------------------------------------------

/// Drive `approve` to completion while a task answers the prompt.
async fn approve_with_reply(
    matrix: PermissionMatrix,
    mode: Mode,
    safety: Safety,
    reply: impl FnOnce(
        mpsc::Sender<SlackInteraction>,
        String,
    ) -> futures_util::future::BoxFuture<'static, ()>,
) -> bool {
    // Premise, asserted rather than remembered: this mode/safety pair must land on
    // an `Ask` cell, otherwise the prompt path is never taken and every assertion
    // below passes for the wrong reason. `Act/Sensitive` is `Allow`, not `Ask` —
    // exactly the mistake this guard catches.
    assert_eq!(
        matrix.effective_cell("tool", mode, safety),
        PermissionCell::Ask,
        "{mode:?}/{safety:?} must prompt for this test to mean anything"
    );

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .and(body_string_contains(ACTION_APPROVE))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "ok": true, "ts": "900.1" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (tx, rx) = mpsc::channel(8);
    let api = SlackApi::new("xoxb-test").with_base(server.uri());
    let approver = Arc::new(
        SlackApprover::new(api, channel(), mode, matrix, rx)
            .with_timeout(Duration::from_millis(500)),
    );

    // The correlation token is `call_id#seq`, so the first prompt of a fresh
    // approver is `c1#0`. A test that guessed wrong would look like a timeout, so
    // the sender asserts on the exact token.
    let replier = tokio::spawn(reply(tx, "c1#0".to_string()));

    let decision = approver
        .approve("m1", "c1", "tool", safety, &serde_json::json!({}))
        .await;
    replier.await.expect("replier");
    decision
}

#[tokio::test]
async fn prompt_renders_buttons_and_an_approve_click_resolves_true() {
    let decision = approve_with_reply(
        PermissionMatrix::default(),
        Mode::Auto,
        Safety::Sensitive,
        |tx, token| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                tx.send(interaction(ACTION_APPROVE, Some(&token)))
                    .await
                    .expect("send");
            })
        },
    )
    .await;
    assert!(decision);
}

#[tokio::test]
async fn a_deny_click_resolves_false() {
    let decision = approve_with_reply(
        PermissionMatrix::default(),
        Mode::Auto,
        Safety::Sensitive,
        |tx, token| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                tx.send(interaction(ACTION_DENY, Some(&token)))
                    .await
                    .expect("send");
            })
        },
    )
    .await;
    assert!(!decision);
}

#[tokio::test]
async fn a_stale_click_is_discarded_and_does_not_answer_the_current_prompt() {
    // The safety property: a click on an *earlier* prompt (or another session's)
    // must not authorize the call in flight. The stale click says Approve; the
    // real one says Deny. If staleness were ignored the result would be `true`.
    let decision = approve_with_reply(
        PermissionMatrix::default(),
        Mode::Auto,
        Safety::Sensitive,
        |tx, token| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                tx.send(interaction(ACTION_APPROVE, Some("c0#99")))
                    .await
                    .expect("stale send");
                tx.send(interaction(ACTION_APPROVE, None))
                    .await
                    .expect("valueless send");
                tx.send(interaction("some_other_button", Some(&token)))
                    .await
                    .expect("unknown action send");
                tx.send(interaction(ACTION_DENY, Some(&token)))
                    .await
                    .expect("real send");
            })
        },
    )
    .await;
    assert!(
        !decision,
        "the awaited prompt said Deny; a stale Approve must not have answered for it"
    );
}

#[tokio::test]
async fn no_click_within_the_timeout_denies() {
    // Fail-closed. The prompt is posted, nobody clicks, and the call is denied
    // rather than left hanging or optimistically allowed.
    let server = prompt_server(1).await;
    let (approver, _tx) = approver(&server, Mode::Auto, PermissionMatrix::default());

    let decision = approver
        .approve(
            "m1",
            "c1",
            "tool",
            Safety::Sensitive,
            &serde_json::json!({}),
        )
        .await;

    assert!(!decision, "an unanswered prompt must fail closed");
}

#[tokio::test]
async fn a_closed_interaction_channel_denies() {
    // Transport disconnect while awaiting: `recv()` yields `None`. Must deny, and
    // must not spin waiting for the full timeout.
    let server = prompt_server(1).await;
    let (approver, tx) = approver(&server, Mode::Auto, PermissionMatrix::default());
    drop(tx);

    let started = tokio::time::Instant::now();
    let decision = approver
        .approve(
            "m1",
            "c1",
            "tool",
            Safety::Sensitive,
            &serde_json::json!({}),
        )
        .await;

    assert!(!decision);
    assert!(
        started.elapsed() < Duration::from_millis(190),
        "a closed channel should deny immediately, not wait out the timeout"
    );
}

#[tokio::test]
async fn a_failed_prompt_post_denies() {
    // Slack returns an error (bad token, channel gone). Nothing to click, so deny.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "ok": false, "error": "channel_not_found" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (approver, _tx) = approver(&server, Mode::Auto, PermissionMatrix::default());
    let decision = approver
        .approve(
            "m1",
            "c1",
            "tool",
            Safety::Sensitive,
            &serde_json::json!({}),
        )
        .await;

    assert!(
        !decision,
        "a prompt that never reached Slack must not allow"
    );
}

#[tokio::test]
async fn each_prompt_gets_a_distinct_token_so_a_retry_cannot_be_answered_by_an_old_click() {
    // Two prompts for the *same* call_id: the model retried. The second must not
    // be satisfiable by a click carrying the first prompt's token.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "ok": true, "ts": "900.1" })),
        )
        .expect(2)
        .mount(&server)
        .await;

    let (tx, rx) = mpsc::channel(8);
    let api = SlackApi::new("xoxb-test").with_base(server.uri());
    let approver = SlackApprover::new(api, channel(), Mode::Auto, PermissionMatrix::default(), rx)
        .with_timeout(Duration::from_millis(150));

    let t1 = tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let _ = t1.send(interaction(ACTION_APPROVE, Some("c1#0"))).await;
    });
    assert!(
        approver
            .approve(
                "m1",
                "c1",
                "tool",
                Safety::Sensitive,
                &serde_json::json!({})
            )
            .await,
        "first prompt is answered by its own token"
    );

    // Replay the *same* token against the retry.
    let t2 = tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let _ = t2.send(interaction(ACTION_APPROVE, Some("c1#0"))).await;
    });
    assert!(
        !approver
            .approve(
                "m1",
                "c1",
                "tool",
                Safety::Sensitive,
                &serde_json::json!({})
            )
            .await,
        "the retry must not be authorized by a replay of the first prompt's token"
    );
}
