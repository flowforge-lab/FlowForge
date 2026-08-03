use crate::approver::MessagingApprover;
use crate::channel_map::ChannelMap;
use crate::router::{Router, RouterConfig};
use crate::transport::MessageTransport;
use crate::types::{ChannelId, InboundMessage};
use ff_agent::{ApprovalOutcome, Approver};
use ff_core::{Egress, Mode};
use ff_llm::{ChatRequest, Chunk, ChunkStream, LlmError, Provider, ToolCallDelta};
use ff_session::SessionStore;
use ff_tools::{Safety, ToolRegistry};
use futures_util::StreamExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

// ── MessagingApprover ────────────────────────────────────────────────────────

#[tokio::test]
async fn act_mode_approves_write_and_sensitive() {
    let a = MessagingApprover::new(Mode::Act);
    let v = serde_json::json!({});
    assert!(matches!(
        a.approve("m", "c", "bash", Safety::Write, &v).await,
        ApprovalOutcome::Allowed
    ));
    assert!(matches!(
        a.approve("m", "c", "bash", Safety::Sensitive, &v).await,
        ApprovalOutcome::Allowed
    ));
    assert!(matches!(
        a.approve("m", "c", "bash", Safety::Dangerous, &v).await,
        ApprovalOutcome::Denied(_)
    ));
    // #1051: a messaging-triggered agent has no interactive surface to confirm
    // a remote publish, so Publish is blocked unattended — like Dangerous.
    assert!(matches!(
        a.approve("m", "c", "bash", Safety::Publish, &v).await,
        ApprovalOutcome::Denied(_)
    ));
}

#[tokio::test]
async fn plan_mode_denies_all() {
    let a = MessagingApprover::new(Mode::Plan);
    let v = serde_json::json!({});
    assert!(matches!(
        a.approve("m", "c", "bash", Safety::Write, &v).await,
        ApprovalOutcome::Denied(_)
    ));
    assert!(matches!(
        a.approve("m", "c", "bash", Safety::Sensitive, &v).await,
        ApprovalOutcome::Denied(_)
    ));
    assert!(matches!(
        a.approve("m", "c", "bash", Safety::Dangerous, &v).await,
        ApprovalOutcome::Denied(_)
    ));
}

// ── ChannelMap ───────────────────────────────────────────────────────────────

#[test]
fn channel_map_round_trip_persistence() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("transports/channel_map.json");

    let channel = ChannelId::new("slack", "C123");
    {
        let mut map = ChannelMap::open(&path);
        assert!(map.is_empty());
        map.insert(channel.clone(), "session-abc".into());
        assert_eq!(map.get(&channel), Some("session-abc"));
    }
    // Reload from disk.
    let map = ChannelMap::open(&path);
    assert_eq!(map.get(&channel), Some("session-abc"));
    assert_eq!(map.len(), 1);
}

#[test]
fn channel_map_in_memory() {
    let mut map = ChannelMap::new();
    let ch = ChannelId::new("test", "ch1");
    map.insert(ch.clone(), "s1".into());
    assert_eq!(map.get(&ch), Some("s1"));
}

// ── Router::run approver injection (#1056) ───────────────────────────────────

/// Emits one `python` (Dangerous) tool call on the first turn, then plain text
/// on the second so the loop terminates. The Dangerous call forces the loop to
/// consult the approver (read-only calls bypass it).
struct DangerousToolThenText {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Provider for DangerousToolThenText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunk = if n == 0 {
            Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call-1".into()),
                    name: Some("python".into()),
                    arguments: "{\"code\":\"print(1)\"}".into(),
                }],
                ..Default::default()
            }
        } else {
            Chunk {
                delta: "done".into(),
                ..Default::default()
            }
        };
        Ok(futures_util::stream::iter(vec![Ok(chunk)]).boxed())
    }
}

/// Records that it was consulted and returns a fixed decision, so we can prove
/// the loop honors the *injected* approver rather than a hardcoded one.
struct RecordingApprover {
    consulted: Arc<AtomicBool>,
    decision: bool,
}

#[async_trait::async_trait]
impl Approver for RecordingApprover {
    async fn approve(
        &self,
        _message_id: &str,
        _call_id: &str,
        _tool: &str,
        _safety: Safety,
        _args: &serde_json::Value,
    ) -> ApprovalOutcome {
        self.consulted.store(true, Ordering::SeqCst);
        if self.decision {
            ApprovalOutcome::Allowed
        } else {
            ApprovalOutcome::Denied(ff_agent::DenyReason::User)
        }
    }
}

#[tokio::test]
async fn run_consults_the_injected_approver() {
    let dir = TempDir::new().unwrap();
    let config = RouterConfig {
        mode: Mode::Act,
        egress: Egress::default(),
        workspace: dir.path().to_path_buf(),
        ..Default::default()
    };
    let store = Arc::new(SessionStore::new());
    let registry = Arc::new(ToolRegistry::with_defaults());
    let provider = Arc::new(DangerousToolThenText {
        calls: AtomicUsize::new(0),
    });
    let mut router = Router::new(config, ChannelMap::new(), store, registry, provider);

    let mut transport = crate::MockTransport::new("mock");
    let tx = transport.sender();
    tx.send(InboundMessage {
        channel: ChannelId::new("mock", "ch1"),
        sender_id: "u1".into(),
        text: "do it".into(),
        timestamp: 0,
    })
    .unwrap();

    let consulted = Arc::new(AtomicBool::new(false));
    let approver = RecordingApprover {
        consulted: consulted.clone(),
        decision: false,
    };

    // MockTransport never closes its channel, so `run` idles after the message;
    // bound it with a timeout — the mock turn is I/O-free, so 2s is ample for the
    // Dangerous call to reach the approval gate.
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        router.run(&mut transport, &approver),
    )
    .await;

    assert!(
        consulted.load(Ordering::SeqCst),
        "Router::run must consult the approver passed to it, proving injection replaced the hardcoded MessagingApprover"
    );
}

// ── turn-failure logging (#1118 problem 3) ───────────────────────────────────

/// The contract that makes always-on logging safe: what goes into the `warn` line
/// must never carry the provider's response body.
///
/// `LlmError::Api` and `LlmError::RateLimited` hold up to 2 KB of provider prose
/// (`ff_llm::error_for_status_with_body`), and a provider may echo request
/// fragments back in a 400. Logging is on by default now, so rendering that at
/// `warn` would write slices of user conversations to disk unasked. The body stays
/// at `debug`, which is opt-in.
#[test]
fn turn_failure_kind_never_leaks_the_provider_body() {
    let secret = "user asked about ACME Corp merger, ssn 123-45-6789";
    let cases = vec![
        LlmError::Api {
            status: 400,
            message: secret.to_string(),
        },
        LlmError::RateLimited {
            retry_after: Some(std::time::Duration::from_secs(30)),
            message: secret.to_string(),
        },
        LlmError::RateLimited {
            retry_after: None,
            message: secret.to_string(),
        },
        LlmError::Transport(secret.to_string()),
        LlmError::Decode(secret.to_string()),
    ];

    for err in cases {
        let rendered = crate::router::turn_failure_kind(&ff_agent::AgentError::Llm(err));
        assert!(
            !rendered.contains("ACME") && !rendered.contains("123-45-6789"),
            "leaked provider body: {rendered:?}"
        );
        assert!(!rendered.is_empty(), "a failure must still be identifiable");
    }
}

/// Redacting everything would be safe and useless. The status code is what tells
/// you whether to back off, fix the payload, or retry — and it cannot contain user
/// data, so it must survive.
#[test]
fn turn_failure_kind_keeps_the_diagnostic_signal() {
    let kind = |e: LlmError| crate::router::turn_failure_kind(&ff_agent::AgentError::Llm(e));

    assert!(
        kind(LlmError::Api {
            status: 429,
            message: "slow down".into(),
        })
        .contains("429"),
        "the status code carries the diagnosis"
    );
    assert!(
        kind(LlmError::RateLimited {
            retry_after: Some(std::time::Duration::from_secs(30)),
            message: "x".into(),
        })
        .contains("30"),
        "retry-after is actionable and safe"
    );
    // Local failures are distinguishable from provider failures, which is the
    // first fork when reading a log.
    assert_ne!(
        kind(LlmError::Transport("x".into())),
        kind(LlmError::Decode("x".into())),
        "local failure modes must stay distinct"
    );
}

// ── graceful shutdown (#1060 scope bullet 4, acceptance 3) ───────────────────

/// `Router::run` returns when the transport is shut down, rather than having to
/// be aborted.
///
/// This is the lever `flowforge serve` needs for Ctrl-C: `recv()` already
/// documents `None` as "closed — a clean stop" (`transport.rs:32`), but nothing
/// could *cause* it from outside, so the only way to stop a running host was to
/// drop the future mid-turn. `shutdown()` closes the inbound side, so the
/// in-flight turn finishes and the loop then exits on its own.
#[tokio::test]
async fn shutdown_makes_run_return_without_being_aborted() {
    let dir = TempDir::new().unwrap();
    let config = RouterConfig {
        mode: Mode::Act,
        egress: Egress::default(),
        workspace: dir.path().to_path_buf(),
        ..Default::default()
    };
    let store = Arc::new(SessionStore::new());
    let registry = Arc::new(ToolRegistry::with_defaults());
    let provider = Arc::new(DangerousToolThenText {
        calls: AtomicUsize::new(0),
    });
    let mut router = Router::new(config, ChannelMap::new(), store, registry, provider);

    let mut transport = crate::MockTransport::new("mock");
    let handle = transport.shutdown_handle();

    let approver = RecordingApprover {
        consulted: Arc::new(AtomicBool::new(false)),
        decision: false,
    };

    handle.shutdown();

    // No `tokio::time::timeout` wrapper: a timeout that *passes* on expiry would
    // make a broken shutdown look green. Letting it hang is the louder failure,
    // and it is bounded — `.config/nextest.toml` terminates a stalled test after
    // two 60s slow-timeout strikes (#1072), the same mechanism the intentionally
    // stalling `ff-llm` tests rely on.
    router.run(&mut transport, &approver).await;
}

/// Shutting down must not discard a turn that is already in flight — that is the
/// difference between "graceful" and `select!`-style abort, which would cut a
/// reply off halfway and leave the user staring at a partial message.
#[tokio::test]
async fn shutdown_lets_an_in_flight_message_finish() {
    let dir = TempDir::new().unwrap();
    let config = RouterConfig {
        mode: Mode::Act,
        egress: Egress::default(),
        workspace: dir.path().to_path_buf(),
        ..Default::default()
    };
    let store = Arc::new(SessionStore::new());
    let registry = Arc::new(ToolRegistry::with_defaults());
    let provider = Arc::new(DangerousToolThenText {
        calls: AtomicUsize::new(0),
    });
    let mut router = Router::new(config, ChannelMap::new(), store, registry, provider);

    let mut transport = crate::MockTransport::new("mock");
    let tx = transport.sender();
    let handle = transport.shutdown_handle();

    tx.send(InboundMessage {
        channel: ChannelId::new("mock", "ch1"),
        sender_id: "u1".into(),
        text: "do it".into(),
        timestamp: 0,
    })
    .unwrap();
    // Release the test's own sender: the channel closes only when *every*
    // sender is gone, so holding this clone would keep `recv()` waiting
    // forever and the assertion below would never be reached.
    drop(tx);

    let consulted = Arc::new(AtomicBool::new(false));
    let approver = RecordingApprover {
        consulted: consulted.clone(),
        decision: false,
    };

    // Queued before the loop starts, so the message is already buffered when the
    // channel closes: a receiver drains what was sent before it sees the close.
    handle.shutdown();
    router.run(&mut transport, &approver).await;

    assert!(
        consulted.load(Ordering::SeqCst),
        "the buffered message must still be processed; shutdown closes the inbound \
         side, it does not discard what was already accepted"
    );
}
