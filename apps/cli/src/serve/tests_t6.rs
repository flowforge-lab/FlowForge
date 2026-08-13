//! T6 booted round-trip tests (#1061, RFC 0021 §5.1/§5.2): `flowforge serve`
//! boots against a mock Slack (Socket Mode WS + Web API) and the four #912
//! acceptance criteria are exercised end-to-end:
//!
//! 1. `serve` boots and connects.
//! 2. A user message round-trips (inbound → agent turn → streamed reply visible).
//! 3. Button approval flow: a `Sensitive` tool call posts buttons; clicking
//!    Approve proceeds, Deny blocks.
//! 4. Streaming edits are visible (throttle + chunking observable in the mock's
//!    received frames).
//!
//! Plus the extra acceptance from #1061: `Publish`/`Dangerous` denial is asserted
//! end-to-end (a push-shaped call is denied, no button offered).
//!
//! # Thread-local config dir
//!
//! `TestEnv` redirects the config dir via a thread-local (`test_support.rs:42`).
//! Every test therefore runs on the **current-thread** tokio runtime so the
//! spawned serve task stays on the same thread and inherits the override.
//! `multi_thread` would launch the task on a worker thread and the override
//! would be lost, causing `serve` to read the real `~/.config/flowforge/`.

#![allow(clippy::await_holding_lock)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ff_llm::{ChatRequest, Chunk, ChunkStream, LlmError, Provider, ToolCallDelta};
use ff_tools::ToolRegistry;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::ServeArgs;
use crate::test_support::TestEnv;
use crate::ModeArg;

// ── Serialisation ────────────────────────────────────────────────────────────

/// Serializes the booted tests.
///
/// The `serve` seams (`crate::serve::test_seams`) are process-global `static`s:
/// `API_BASE` and `HOST`. Each test sets them for its own mock, so under a
/// runner that keeps the tests in one process and interleaves their threads —
/// plain `cargo test`, unlike nextest's process-per-test — one test's
/// `set_api_base(mock_url)` clobbers a sibling's, and the second `serve` dials
/// the wrong mock and times out at the WS handshake (#1240 review).
///
/// Same shape as `test_support::MEM_STORE_LOCK`: a `Mutex<()>` guard taken by
/// every test that shares process-global state. The `unwrap_or_else` recovers
/// from a poisoned lock (a panic in one test must not fail its siblings).
static T6_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn t6_lock() -> std::sync::MutexGuard<'static, ()> {
    T6_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ── Slack envelope builders ──────────────────────────────────────────────────

fn hello_frame() -> String {
    r#"{"type":"hello"}"#.into()
}

fn user_message_frame(
    envelope_id: &str,
    channel: &str,
    user: &str,
    text: &str,
    ts: &str,
) -> String {
    serde_json::json!({
        "envelope_id": envelope_id,
        "type": "events_api",
        "payload": {
            "type": "event_callback",
            "event": {
                "type": "message",
                "channel": channel,
                "user": user,
                "text": text,
                "ts": ts
            }
        }
    })
    .to_string()
}

fn interaction_frame(
    envelope_id: &str,
    channel: &str,
    user: &str,
    action_id: &str,
    value: &str,
) -> String {
    serde_json::json!({
        "envelope_id": envelope_id,
        "type": "interactive",
        "payload": {
            "type": "block_actions",
            "user": { "id": user, "username": "tony" },
            "channel": { "id": channel, "name": "dev" },
            "message": { "ts": "1548261231.000200" },
            "actions": [
                { "type": "button", "action_id": action_id, "block_id": "gate", "value": value }
            ]
        }
    })
    .to_string()
}

fn disconnect_frame() -> String {
    serde_json::json!({ "type": "disconnect", "reason": "test done" }).to_string()
}

// ── Mock Slack ───────────────────────────────────────────────────────────────

/// A simulated Slack side: a wiremock HTTP server for `apps.connections.open`
/// and the Web API, plus a WebSocket server that speaks Socket Mode.
struct MockSlack {
    http: MockServer,
    conn_rx: oneshot::Receiver<()>,
    ws_tx: mpsc::Sender<String>,
    inbound: Arc<Mutex<Vec<serde_json::Value>>>,
    posts: Arc<Mutex<Vec<serde_json::Value>>>,
    updates: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl MockSlack {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind WS listener");
        let ws_addr = listener.local_addr().expect("WS addr");
        let http = MockServer::start().await;

        // 1. apps.connections.open → returns the mock's own WS URL.
        let ws_url = format!("ws://{ws_addr}");
        Mock::given(method("POST"))
            .and(path("/apps.connections.open"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "url": ws_url
            })))
            .mount(&http)
            .await;

        // 2. Web API captures.
        let posts: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let updates: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let next_ts = Arc::new(AtomicUsize::new(1000));

        let posts_mock = Arc::clone(&posts);
        let ts = Arc::clone(&next_ts);
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .respond_with(move |req: &wiremock::Request| {
                if let Ok(body) = serde_json::from_slice::<serde_json::Value>(&req.body) {
                    posts_mock.lock().unwrap().push(body);
                }
                let n = ts.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "ok": true,
                    "ts": format!("{n}.000")
                }))
            })
            .mount(&http)
            .await;

        let updates_mock = Arc::clone(&updates);
        Mock::given(method("POST"))
            .and(path("/chat.update"))
            .respond_with(move |req: &wiremock::Request| {
                if let Ok(body) = serde_json::from_slice::<serde_json::Value>(&req.body) {
                    updates_mock.lock().unwrap().push(body);
                }
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true }))
            })
            .mount(&http)
            .await;

        // 3. WS accept task.
        let (conn_tx, conn_rx) = oneshot::channel();
        let (ws_tx, mut ws_rx) = mpsc::channel::<String>(64);
        let inbound = Arc::new(Mutex::new(Vec::new()));
        let inbound_task = Arc::clone(&inbound);
        tokio::spawn(async move {
            let (stream, _) = listener
                .accept()
                .await
                .expect("serve must connect to the mock WS");
            let (mut sink, mut stream) = tokio_tungstenite::accept_async(stream)
                .await
                .expect("WS handshake")
                .split();
            // Slack greets the app with `hello` right after the handshake.
            let _ = sink.send(Message::Text(hello_frame())).await;
            let _ = conn_tx.send(());
            loop {
                tokio::select! {
                    frame = stream.next() => match frame {
                        Some(Ok(Message::Text(t))) => {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                                inbound_task.lock().unwrap().push(v);
                            }
                        }
                        _ => break,
                    },
                    outgoing = ws_rx.recv() => match outgoing {
                        Some(t) => {
                            if sink.send(Message::Text(t)).await.is_err() { break; }
                        }
                        None => break,
                    },
                }
            }
        });

        Self {
            http,
            conn_rx,
            ws_tx,
            inbound,
            posts,
            updates,
        }
    }

    /// Wait until the transport has completed the WebSocket handshake and the
    /// mock has sent `hello`.
    async fn wait_connected(&mut self) {
        tokio::time::timeout(Duration::from_secs(10), &mut self.conn_rx)
            .await
            .expect("serve must connect within 10s")
            .expect("connection signal");
    }

    /// Push a frame down the socket to the transport.
    async fn send_frame(&self, frame: String) {
        self.ws_tx.send(frame).await.expect("WS task must be alive");
    }

    /// Close the socket gracefully: `disconnect` makes the transport's reader
    /// stop, which ends `recv` → `Router::run` → `serve` returns.
    async fn shut_down(&self) {
        self.send_frame(disconnect_frame()).await;
    }

    fn captured_posts(&self) -> Vec<serde_json::Value> {
        self.posts.lock().unwrap().clone()
    }

    fn captured_updates(&self) -> Vec<serde_json::Value> {
        self.updates.lock().unwrap().clone()
    }

    fn acks(&self) -> Vec<serde_json::Value> {
        self.inbound.lock().unwrap().clone()
    }

    /// Wait until a `chat.postMessage` body satisfies `pred`.
    async fn wait_for_post(&self, pred: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
        wait_for(&self.posts, Duration::from_secs(15), pred).await
    }

    /// Wait until a `chat.update` body satisfies `pred`.
    async fn wait_for_update(
        &self,
        pred: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        wait_for(&self.updates, Duration::from_secs(15), pred).await
    }

    /// Wait until the transport has acked the given envelope id.
    async fn wait_for_ack(&self, envelope_id: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let hits = self.acks();
            if hits
                .iter()
                .any(|v| v["envelope_id"].as_str() == Some(envelope_id))
            {
                return;
            }
            assert!(Instant::now() < deadline, "no ack for {envelope_id}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// Spin-poll `sink` until a value matching `pred` appears.
async fn wait_for(
    sink: &Mutex<Vec<serde_json::Value>>,
    timeout: Duration,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = sink.lock().unwrap().iter().find(|v| pred(v)).cloned() {
            return v;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for a matching HTTP capture"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ── Stub providers ───────────────────────────────────────────────────────────

/// A provider that returns a single text chunk.
struct TextProvider {
    reply: String,
}

impl TextProvider {
    fn new(reply: impl Into<String>) -> Self {
        Self {
            reply: reply.into(),
        }
    }
}

#[async_trait]
impl Provider for TextProvider {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let chunk = Chunk {
            delta: self.reply.clone(),
            ..Default::default()
        };
        Ok(futures_util::stream::iter(vec![Ok(chunk)]).boxed())
    }
}

/// A provider that emits a paced stream of deltas, one every `PACED_STEP`, so
/// the Router's per-turn flusher (250ms cadence) delivers intermediate edits.
struct StreamingProvider {
    deltas: Vec<String>,
}

impl StreamingProvider {
    fn new(deltas: Vec<String>) -> Self {
        Self { deltas }
    }
}

const PACED_STEP: Duration = Duration::from_millis(700);

#[async_trait]
impl Provider for StreamingProvider {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let deltas = Arc::new(self.deltas.clone());
        Ok(futures_util::stream::unfold(0usize, move |i| {
            let deltas = Arc::clone(&deltas);
            async move {
                if i >= deltas.len() {
                    return None;
                }
                tokio::time::sleep(PACED_STEP).await;
                let chunk = Chunk {
                    delta: deltas[i].clone(),
                    ..Default::default()
                };
                Some((Ok(chunk), i + 1))
            }
        })
        .boxed())
    }
}

/// A provider that emits a tool call on the first invocation, then text on
/// subsequent calls. Used for the approval-flow tests.
struct ToolThenText {
    tool_name: &'static str,
    tool_args: &'static str,
    text: String,
    calls: AtomicUsize,
}

impl ToolThenText {
    fn new(tool_name: &'static str, tool_args: &'static str, text: String) -> Self {
        Self {
            tool_name,
            tool_args,
            text,
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl Provider for ToolThenText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunk = if n == 0 {
            Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call-1".into()),
                    name: Some(self.tool_name.into()),
                    arguments: self.tool_args.into(),
                }],
                ..Default::default()
            }
        } else {
            Chunk {
                delta: self.text.clone(),
                ..Default::default()
            }
        };
        Ok(futures_util::stream::iter(vec![Ok(chunk)]).boxed())
    }
}

// ── Config ───────────────────────────────────────────────────────────────────

const SLACK_TOML: &str = r#"
[slack]
app_token = "xapp-test"
bot_token = "xoxb-test"
allowed_users = ["U1"]
"#;

// ── Boot helper ──────────────────────────────────────────────────────────────

/// Boot `serve` against `mock` with a scripted provider + the default toolset.
/// Returns the serve task handle; the test ends it by closing the socket.
fn spawn_serve(
    mock: &MockSlack,
    provider: Arc<dyn Provider>,
) -> tokio::task::JoinHandle<Result<(), String>> {
    crate::serve::test_seams::set_api_base(mock.http.uri());
    crate::serve::test_seams::set_host(
        provider,
        "t6-test-model",
        Arc::new(ToolRegistry::with_defaults()),
    );
    tokio::spawn(async move {
        super::serve(ServeArgs {
            channel: "C9".into(),
            mode: ModeArg::Auto,
            allow_user: Vec::new(),
        })
        .await
    })
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Extract the `value` of the first button element with `action_id` from a
/// posted approval prompt's blocks.
fn prompt_button_value(prompt: &serde_json::Value, action_id: &str) -> Option<String> {
    prompt["blocks"]
        .as_array()?
        .iter()
        .filter_map(|b| b["elements"].as_array())
        .flatten()
        .find(|el| el["action_id"].as_str() == Some(action_id))
        .and_then(|el| el["value"].as_str().map(str::to_owned))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn serve_boots_against_mock_and_shuts_down_cleanly() {
    let _lock = t6_lock();
    let mut mock = MockSlack::start().await;
    let _env = TestEnv::new();
    _env.write_transports(SLACK_TOML);

    let handle = spawn_serve(&mock, Arc::new(TextProvider::new("hi")));
    mock.wait_connected().await;

    mock.shut_down().await;

    let result = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("serve must stop after disconnect")
        .expect("join");
    assert!(result.is_ok(), "clean shutdown, got {result:?}");
}

#[tokio::test]
async fn user_message_round_trips_inbound_to_streamed_reply() {
    let _lock = t6_lock();
    let mut mock = MockSlack::start().await;
    let _env = TestEnv::new();
    _env.write_transports(SLACK_TOML);

    let handle = spawn_serve(&mock, Arc::new(TextProvider::new("the reply")));
    mock.wait_connected().await;

    mock.send_frame(user_message_frame(
        "env-r1",
        "C9",
        "U1",
        "hello",
        "1548261231.000200",
    ))
    .await;

    // The message is acked on the socket.
    mock.wait_for_ack("env-r1").await;

    // The turn's reply is posted with the thread anchor.
    let post = mock
        .wait_for_post(|b| b["text"].as_str() == Some("the reply"))
        .await;
    assert_eq!(post["channel"].as_str(), Some("C9"));
    assert_eq!(
        post["thread_ts"].as_str(),
        Some("1548261231.000200"),
        "reply must anchor to the triggering message's ts"
    );

    mock.shut_down().await;
    let result = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("serve must stop")
        .expect("join");
    assert!(result.is_ok());
}

#[tokio::test]
async fn sensitive_call_posts_buttons_and_approve_proceeds() {
    let _lock = t6_lock();
    let mut mock = MockSlack::start().await;
    let _env = TestEnv::new();
    _env.write_transports(SLACK_TOML);

    let provider = Arc::new(ToolThenText::new(
        "web_fetch",
        r#"{"url":"http://127.0.0.1:1/"}"#,
        "approved and done".into(),
    ));
    let handle = spawn_serve(&mock, provider);
    mock.wait_connected().await;

    mock.send_frame(user_message_frame(
        "env-a1",
        "C9",
        "U1",
        "deploy",
        "1548261231.000300",
    ))
    .await;

    // A Sensitive call (web_fetch) in Auto lands on Ask: buttons must be posted.
    let prompt = mock
        .wait_for_post(|b| {
            b["text"]
                .as_str()
                .is_some_and(|t| t.contains("Approval needed"))
        })
        .await;
    let token = prompt_button_value(&prompt, "ff_approve")
        .expect("approve button must have a value (correlation token)");

    // Click Approve.
    mock.send_frame(interaction_frame(
        "env-a2",
        "C9",
        "U1",
        "ff_approve",
        &token,
    ))
    .await;

    // The settled prompt is retired with an attribution epilogue.
    mock.wait_for_update(|b| {
        b["text"]
            .as_str()
            .is_some_and(|t| t.contains("approved by"))
    })
    .await;

    // The turn completed and posted its reply.
    mock.wait_for_post(|b| b["text"].as_str() == Some("approved and done"))
        .await;

    mock.shut_down().await;
    let result = tokio::time::timeout(Duration::from_secs(15), handle)
        .await
        .expect("serve must stop")
        .expect("join");
    assert!(result.is_ok());
}

#[tokio::test]
async fn sensitive_call_posts_buttons_and_deny_blocks() {
    let _lock = t6_lock();
    let mut mock = MockSlack::start().await;
    let _env = TestEnv::new();
    _env.write_transports(SLACK_TOML);

    let provider = Arc::new(ToolThenText::new(
        "web_fetch",
        r#"{"url":"http://127.0.0.1:1/"}"#,
        "denied and done".into(),
    ));
    let handle = spawn_serve(&mock, provider);
    mock.wait_connected().await;

    mock.send_frame(user_message_frame(
        "env-d1",
        "C9",
        "U1",
        "do it",
        "1548261231.000350",
    ))
    .await;

    let prompt = mock
        .wait_for_post(|b| {
            b["text"]
                .as_str()
                .is_some_and(|t| t.contains("Approval needed"))
        })
        .await;
    let token = prompt_button_value(&prompt, "ff_deny")
        .expect("deny button must have a value (correlation token)");

    // Click Deny.
    mock.send_frame(interaction_frame("env-d2", "C9", "U1", "ff_deny", &token))
        .await;

    // The prompt is retired with a "denied by" epilogue — this proves the
    // approver was consulted and returned Denied. Whether the *tool* actually
    // ran is a unit-level assertion: `tests_t4::a_deny_click_resolves_false`
    // proves the approver returns `Denied` on a deny click, and
    // `ff_agent::run_turn` treats a `Denied` approval as a tool error without
    // dispatching the tool. At the transport layer the distinction is invisible
    // (the scripted provider emits the same reply either way), so the two
    // layers together cover the end-to-end behaviour.
    mock.wait_for_update(|b| b["text"].as_str().is_some_and(|t| t.contains("denied by")))
        .await;

    // The turn completed.
    mock.wait_for_post(|b| b["text"].as_str() == Some("denied and done"))
        .await;

    mock.shut_down().await;
    let result = tokio::time::timeout(Duration::from_secs(15), handle)
        .await
        .expect("serve must stop")
        .expect("join");
    assert!(result.is_ok());
}

#[tokio::test]
async fn streaming_edits_are_visible_in_the_mock() {
    let _lock = t6_lock();
    let mut mock = MockSlack::start().await;
    let _env = TestEnv::new();
    _env.write_transports(SLACK_TOML);

    let provider = Arc::new(StreamingProvider::new(vec![
        "alpha ".into(),
        "beta ".into(),
        "gamma".into(),
    ]));
    let handle = spawn_serve(&mock, provider);
    mock.wait_connected().await;

    mock.send_frame(user_message_frame(
        "env-s1",
        "C9",
        "U1",
        "stream",
        "1548261231.000400",
    ))
    .await;

    // The first chunk is posted, then the accumulated text is edited in place.
    mock.wait_for_post(|b| b["text"].as_str() == Some("alpha "))
        .await;

    // The intermediate edit ("alpha beta ") must also be visible — proving the
    // throttle allowed the window between deltas.
    let full = "alpha beta gamma";
    mock.wait_for_update(|b| b["text"].as_str() == Some("alpha beta "))
        .await;
    mock.wait_for_update(|b| b["text"].as_str() == Some(full))
        .await;

    // Sanity: the body that was actually posted was the first delta, not the
    // full text in one shot (which would mean no streaming happened).
    let posts = mock.captured_posts();
    assert_eq!(
        posts.len(),
        1,
        "there must be exactly one post; the rest are edits, got {posts:?}"
    );
    assert_eq!(posts[0]["text"].as_str(), Some("alpha "));

    mock.shut_down().await;
    let result = tokio::time::timeout(Duration::from_secs(20), handle)
        .await
        .expect("serve must stop")
        .expect("join");
    assert!(result.is_ok());
}

#[tokio::test]
async fn publish_shaped_call_is_denied_without_buttons() {
    let _lock = t6_lock();
    let mut mock = MockSlack::start().await;
    let _env = TestEnv::new();
    _env.write_transports(SLACK_TOML);

    let provider = Arc::new(ToolThenText::new(
        "github",
        r#"{"action":"push"}"#,
        "push refused".into(),
    ));
    let handle = spawn_serve(&mock, provider);
    mock.wait_connected().await;

    mock.send_frame(user_message_frame(
        "env-p1",
        "C9",
        "U1",
        "push it",
        "1548261231.000500",
    ))
    .await;

    // The turn resolves…
    mock.wait_for_post(|b| b["text"].as_str() == Some("push refused"))
        .await;

    // …without ever offering an approval button: a shared channel button may not
    // authorise Publish (the `SlackApprover` clamps `Publish` → Deny before any
    // prompt is posted).
    let bodies = mock.captured_posts();
    assert!(
        bodies.iter().all(|b| !b["text"]
            .as_str()
            .is_some_and(|t| t.contains("Approval needed"))),
        "a Publish call must not post an approval prompt, got {bodies:?}"
    );
    assert_eq!(
        mock.captured_updates().len(),
        0,
        "no epilogue: nothing was prompted"
    );

    mock.shut_down().await;
    let result = tokio::time::timeout(Duration::from_secs(15), handle)
        .await
        .expect("serve must stop")
        .expect("join");
    assert!(result.is_ok());
}
