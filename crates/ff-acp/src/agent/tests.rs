//! Behavioural tests for the ACP agent server, driven over an in-memory
//! [`Channel`](agent_client_protocol::Channel) transport so a real client and
//! the FlowForge agent talk without spawning a child process.
//!
//! Coverage (ticket #1201 ACs):
//! - AC1: unknown client→agent methods return a JSON-RPC error, not a panic/hang.
//! - AC3: a tool in an `Ask` cell produces a `session/request_permission` round-trip.
//! - AC4: `session/cancel` cancels in-flight work — a wedged prompt cannot outlive it.
//!
//! (AC2 — a `Deny`-cell tool is absent from the advertised set — is pinned by the
//! unit tests in [`crate::advertise`], which assert the mapping directly.)

use super::*;
use crate::wire;
use agent_client_protocol::{Channel, ConnectionTo};
use async_trait::async_trait;
use ff_agent::{Approver, SystemPromptInputs, ToolContext, UserContext};
use ff_core::{PermissionMatrix, Safety};
use ff_llm::{Chunk, ChunkStream, LlmError, Provider, ToolCallDelta};
use ff_session::SessionStore;
use ff_skills::SkillRegistry;
use ff_tools::{ToolOutcome, ToolRegistry};
use futures_util::StreamExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

// ---- Test host ----

/// A [`Provider`] scripted per test: the first turn either emits a tool-call (to
/// exercise the approval round-trip) or blocks forever (to exercise cancel), and
/// any later turn returns a short final message.
struct ScriptedProvider {
    calls: AtomicUsize,
    behaviour: Behaviour,
}

#[derive(Clone, Copy)]
enum Behaviour {
    /// First turn calls the Dangerous `wedge` tool (Ask cell in Act) → approval.
    CallWedgeTool,
    /// Every turn streams chunks forever (a runaway generation) — the turn only
    /// ends when the cooperative cancel token is observed between chunks.
    RunawayStream,
    /// The provider fails with a non-transient LLM error — a real turn failure,
    /// not a cancellation. Exercises the `Err` arm of the turn-result mapping.
    LlmError,
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn chat_stream(&self, _req: ff_llm::ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        match self.behaviour {
            Behaviour::LlmError => {
                // A non-transient error surfaces immediately (no retry churn), so the
                // turn returns `Err(AgentError::Llm)`.
                Err(LlmError::Decode("scripted decode failure".into()))
            }
            Behaviour::RunawayStream => {
                // An endless stream that never completes and never yields real
                // content: the turn loop keeps awaiting chunks and only stops when
                // it observes the cancel token between them, leaving an empty final
                // message — the path the core turn maps to `StopReason::Cancelled`.
                // `yield_now` keeps the runtime cooperative so the cancel
                // notification can be delivered mid-stream.
                let stream = futures_util::stream::unfold((), |()| async {
                    tokio::task::yield_now().await;
                    Some((Ok(Chunk::default()), ()))
                });
                Ok(stream.boxed())
            }
            Behaviour::CallWedgeTool => {
                let chunks = if n == 0 {
                    vec![Ok(Chunk {
                        tool_calls: vec![ToolCallDelta {
                            index: 0,
                            id: Some("call_1".into()),
                            name: Some("wedge".into()),
                            arguments: "{}".into(),
                        }],
                        done: true,
                        ..Chunk::default()
                    })]
                } else {
                    vec![Ok(Chunk {
                        delta: "all done".into(),
                        done: true,
                        ..Chunk::default()
                    })]
                };
                Ok(futures_util::stream::iter(chunks).boxed())
            }
        }
    }
}

/// A minimal `Dangerous`-tier tool. In Act mode the (Act, Dangerous) matrix cell
/// is `Ask`, so dispatching it drives the approver → `session/request_permission`.
struct WedgeTool;

#[async_trait]
impl ff_tools::Tool for WedgeTool {
    fn name(&self) -> &str {
        "wedge"
    }
    fn description(&self) -> &str {
        "test-only tool whose safety ceiling forces an approval prompt"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    fn safety(&self, _args: &serde_json::Value) -> Safety {
        Safety::Dangerous
    }
    async fn run(&self, _args: serde_json::Value, _root: &Path) -> ToolOutcome {
        ToolOutcome::ok("wedge ran")
    }
}

/// Owns every resource a turn borrows, mirroring `apps/cli`'s `CliAcpHost` but
/// with a scripted provider and a test-only tool.
struct TestHost {
    provider: ScriptedProvider,
    store: SessionStore,
    registry: ToolRegistry,
    matrix: PermissionMatrix,
    skills: SkillRegistry,
    user: UserContext,
    workspace: std::path::PathBuf,
}

impl TestHost {
    fn new(behaviour: Behaviour) -> Self {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(WedgeTool));
        Self {
            provider: ScriptedProvider {
                calls: AtomicUsize::new(0),
                behaviour,
            },
            store: SessionStore::new(),
            registry,
            matrix: PermissionMatrix::default(),
            skills: SkillRegistry::new(),
            user: UserContext::now(),
            workspace: std::env::temp_dir(),
        }
    }
}

#[async_trait]
impl AcpHost for TestHost {
    fn provider(&self) -> &dyn Provider {
        &self.provider
    }
    fn store(&self) -> &SessionStore {
        &self.store
    }
    fn model(&self) -> &str {
        "test-model"
    }
    fn tool_context<'a>(&'a self, _mode: Mode, approver: &'a dyn Approver) -> ToolContext<'a> {
        ToolContext::new(
            &self.registry,
            &self.workspace,
            approver,
            ff_agent::DEFAULT_MAX_ITERATIONS,
            &self.matrix,
        )
    }
    fn prompt_inputs(&self, mode: Mode) -> SystemPromptInputs<'_> {
        SystemPromptInputs::new(&self.skills, &[], &self.user, mode)
    }
}

// ---- Test client ----
//
// The client is built inline per test (see `run_client`) with closure handlers,
// mirroring the SDK's `yolo_one_shot_client` example: a `SessionNotification`
// handler forwards `session/update`s to a channel, and a `RequestPermissionRequest`
// handler answers with a fixed outcome and records that it fired.

// ---- Harness ----

/// Spawn the FlowForge agent on one end of an in-memory duplex channel and return
/// the other end for a client to connect to.
fn spawn_agent(host: Arc<TestHost>) -> (Channel, tokio::task::JoinHandle<AcpResult<()>>) {
    let (agent_chan, client_chan) = Channel::duplex();
    let handle = tokio::spawn(async move { connect(host, agent_chan).await });
    (client_chan, handle)
}

/// Drive `initialize` → `session/new` on `connection`, returning the new session id.
async fn init_and_new_session(
    connection: &ConnectionTo<Agent>,
) -> Result<wire::SessionId, agent_client_protocol::Error> {
    connection
        .send_request(wire::InitializeRequest::new(ProtocolVersion::V1))
        .block_task()
        .await?;
    let resp = connection
        .send_request(wire::NewSessionRequest::new(std::env::temp_dir()))
        .block_task()
        .await?;
    Ok(resp.session_id)
}

// ---- AC1: unknown method → JSON-RPC error, no panic/hang ----

#[tokio::test]
async fn unknown_method_returns_jsonrpc_error_not_panic() {
    let host = Arc::new(TestHost::new(Behaviour::CallWedgeTool));
    let (client_chan, agent) = spawn_agent(host);

    agent_client_protocol::Client
        .builder()
        .connect_with(client_chan, |connection: ConnectionTo<Agent>| async move {
            init_and_new_session(&connection).await?;
            // `session/list` is a valid schema request the agent never registers a
            // handler for; the SDK must auto-respond `method_not_found`.
            let result = connection
                .send_request(wire::ListSessionsRequest::new())
                .block_task()
                .await;
            assert!(
                result.is_err(),
                "an unregistered method must return an error, not a response"
            );
            Ok(())
        })
        .await
        .expect("client connection");

    agent.abort();
}

// ---- AC3: an Ask cell produces a session/request_permission round-trip ----

#[tokio::test]
async fn ask_cell_produces_request_permission_roundtrip() {
    let host = Arc::new(TestHost::new(Behaviour::CallWedgeTool));
    let (client_chan, agent) = spawn_agent(host);

    let permission_hits = Arc::new(AtomicUsize::new(0));
    let hits = Arc::clone(&permission_hits);

    agent_client_protocol::Client
        .builder()
        .on_receive_request(
            {
                let hits = Arc::clone(&hits);
                async move |request: wire::RequestPermissionRequest, responder, _cx| {
                    hits.fetch_add(1, Ordering::SeqCst);
                    // Approve by selecting the first offered option.
                    let id = request
                        .options
                        .first()
                        .map(|opt| opt.option_id.clone())
                        .expect("permission request offers at least one option");
                    responder.respond(wire::RequestPermissionResponse::new(
                        wire::RequestPermissionOutcome::Selected(
                            wire::SelectedPermissionOutcome::new(id),
                        ),
                    ))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(client_chan, |connection: ConnectionTo<Agent>| async move {
            let session_id = init_and_new_session(&connection).await?;
            connection
                .send_request(wire::PromptRequest::new(
                    session_id,
                    vec![wire::ContentBlock::Text(wire::TextContent::new(
                        "use the tool".to_string(),
                    ))],
                ))
                .block_task()
                .await?;
            Ok(())
        })
        .await
        .expect("client connection");

    agent.abort();
    assert_eq!(
        permission_hits.load(Ordering::SeqCst),
        1,
        "the Ask-cell tool must trigger exactly one session/request_permission"
    );
}

// ---- AC4: session/cancel cancels in-flight work ----

#[tokio::test]
async fn cancel_stops_a_wedged_prompt() {
    let host = Arc::new(TestHost::new(Behaviour::RunawayStream));
    let (client_chan, agent) = spawn_agent(host);

    agent_client_protocol::Client
        .builder()
        .connect_with(client_chan, |connection: ConnectionTo<Agent>| async move {
            let session_id = init_and_new_session(&connection).await?;
            // Fire the prompt without awaiting: the provider hangs forever, so the
            // turn cannot complete on its own.
            let prompt = connection.send_request(wire::PromptRequest::new(
                session_id.clone(),
                vec![wire::ContentBlock::Text(wire::TextContent::new(
                    "hang".to_string(),
                ))],
            ));
            let prompt_fut = prompt.block_task();
            tokio::pin!(prompt_fut);

            // Let the turn get in-flight, then cancel it.
            tokio::time::sleep(Duration::from_millis(50)).await;
            connection.send_notification(wire::CancelNotification::new(session_id))?;

            // The prompt must now resolve (cancelled) rather than hang forever.
            let resp = tokio::time::timeout(Duration::from_secs(5), prompt_fut)
                .await
                .expect("a cancelled prompt must resolve, not outlive cancellation")?;
            assert_eq!(
                resp.stop_reason,
                wire::StopReason::Cancelled,
                "cancel must yield StopReason::Cancelled"
            );
            Ok(())
        })
        .await
        .expect("client connection");

    agent.abort();
}

// ---- A turn failure must not masquerade as a cancellation ----

#[tokio::test]
async fn llm_error_reports_end_turn_not_cancelled() {
    // A non-transient provider error makes the turn return `Err(AgentError::Llm)`.
    // That is a real failure, not a user cancellation, so the ACP `stop_reason`
    // must be `EndTurn` — reporting `Cancelled` would mask a defect as a benign
    // stop (regression guard for the turn-result `Err` arm).
    let host = Arc::new(TestHost::new(Behaviour::LlmError));
    let (client_chan, agent) = spawn_agent(host);

    agent_client_protocol::Client
        .builder()
        .connect_with(client_chan, |connection: ConnectionTo<Agent>| async move {
            let session_id = init_and_new_session(&connection).await?;
            let resp = connection
                .send_request(wire::PromptRequest::new(
                    session_id,
                    vec![wire::ContentBlock::Text(wire::TextContent::new(
                        "go".to_string(),
                    ))],
                ))
                .block_task()
                .await?;
            assert_eq!(
                resp.stop_reason,
                wire::StopReason::EndTurn,
                "an LLM error must report EndTurn, never Cancelled"
            );
            Ok(())
        })
        .await
        .expect("client connection");

    agent.abort();
}

// ---- session/delete cleans up the session registry and store ----

#[tokio::test]
async fn delete_session_removes_session_from_store() {
    let host = Arc::new(TestHost::new(Behaviour::CallWedgeTool));
    let host_for_agent = Arc::clone(&host);
    let (client_chan, agent) = spawn_agent(host_for_agent);

    agent_client_protocol::Client
        .builder()
        .connect_with(client_chan, |connection: ConnectionTo<Agent>| async move {
            let session_id = init_and_new_session(&connection).await?;
            let id_str = session_id.0.as_ref().to_string();

            assert!(
                host.store().get_session(&id_str).is_some(),
                "session must exist in the store after session/new"
            );

            // Set a non-default mode so the test exercises the mode path.
            connection
                .send_request(wire::SetSessionModeRequest::new(
                    session_id.clone(),
                    wire::SessionModeId::new("act"),
                ))
                .block_task()
                .await?;

            // Send session/delete.
            connection
                .send_request(wire::DeleteSessionRequest::new(session_id.clone()))
                .block_task()
                .await?;

            // The session is removed from the store.
            assert!(
                host.store().get_session(&id_str).is_none(),
                "session must be removed from the store after session/delete"
            );

            Ok(())
        })
        .await
        .expect("client connection");

    agent.abort();
}
