use crate::approver::MessagingApprover;
use crate::channel_map::ChannelMap;
use crate::router::{Router, RouterConfig};
use crate::types::{ChannelId, InboundMessage};
use ff_agent::Approver;
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
    assert!(a.approve("m", "c", "bash", Safety::Write, &v).await);
    assert!(a.approve("m", "c", "bash", Safety::Sensitive, &v).await);
    assert!(!a.approve("m", "c", "bash", Safety::Dangerous, &v).await);
    // #1051: a messaging-triggered agent has no interactive surface to confirm
    // a remote publish, so Publish is blocked unattended — like Dangerous.
    assert!(!a.approve("m", "c", "bash", Safety::Publish, &v).await);
}

#[tokio::test]
async fn plan_mode_denies_all() {
    let a = MessagingApprover::new(Mode::Plan);
    let v = serde_json::json!({});
    assert!(!a.approve("m", "c", "bash", Safety::Write, &v).await);
    assert!(!a.approve("m", "c", "bash", Safety::Sensitive, &v).await);
    assert!(!a.approve("m", "c", "bash", Safety::Dangerous, &v).await);
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

/// Emits one `python` (Write) tool call on the first turn, then plain text on
/// the second so the loop terminates. The Write call forces the loop to consult
/// the approver (read-only calls bypass it).
struct WriteToolThenText {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Provider for WriteToolThenText {
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
    ) -> bool {
        self.consulted.store(true, Ordering::SeqCst);
        self.decision
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
    let provider = Arc::new(WriteToolThenText {
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
    // Write call to reach the approval gate.
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
