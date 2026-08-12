//! The client-side ACP caller: spawn an external ACP agent over stdio and drive
//! the client→agent half of the protocol (`initialize`, `session/new`,
//! `session/prompt`, `session/cancel`, `session/set_mode`).
//!
//! This is the structural sibling of `ff-mcp`'s [`McpClient`](ff_mcp::McpClient),
//! built against the official [`agent_client_protocol`] SDK the way `McpClient`
//! is built against `rmcp`. It owns **no JSON-RPC framing of its own** — the
//! SDK's [`AcpAgent`] provides the spawn + process-group reap-on-drop (so
//! `npx → node` / `uvx → python` wrapper orphans don't survive, the #1197
//! lesson) and a bounded protocol-vs-child-exit shutdown. What lives here is
//! the FlowForge-typed surface over that, and the [`content::inbound`] mapping
//! that turns the agent's `session/update` stream into [`AgentEvent`]s the host
//! already renders.
//!
//! # Actor shape
//!
//! The SDK's [`Client::connect_with`] owns the connection for its closure's
//! lifetime, so the client *is* an actor: [`AcpClient`] is a cheap
//! [`mpsc::Sender`] of [`Cmd`]s plus a [`JoinHandle`]; the connection task
//! drives commands inside the `connect_with` closure. A [`Cmd::Stop`] returns
//! the closure, `connect_with` resolves, and `AcpAgent`'s [`ChildGuard`] reaps
//! the process group on drop — bounded by [`SHUTDOWN_TIMEOUT`] so a wedged
//! agent cannot hang FlowForge's quit (AC 2/3, mirroring `ff-mcp`'s
//! `stop_all`/`SHUTDOWN_TIMEOUT`).
//!
//! [`ChildGuard`]: agent_client_protocol::acp_agent
//! [`Client::connect_with`]: agent_client_protocol::Client::connect_with

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::{
    self as acp, util::MatchDispatch, AcpAgent, ActiveSession, Agent, Client, ConnectionTo,
    Dispatch, SessionMessage,
};
use ff_agent::AgentEvent;
use ff_core::Mode;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::{content, mode, wire};

/// How long a clean shutdown may take before we abort the connection task and
/// let `AcpAgent`'s `ChildGuard` reap the process group. Bounds app-exit latency
/// so one wedged agent cannot hang the quit — mirrors `ff-mcp`'s 2s per-server
/// `SHUTDOWN_TIMEOUT`, with headroom over the SDK's internal 1s grace.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// Errors from the ACP client.
#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    /// A JSON-RPC / protocol error from the agent or the SDK transport.
    #[error(transparent)]
    Protocol(#[from] acp::Error),
    /// The connection task exited before answering a command (agent crashed or
    /// was shut down). The command's reply channel is dropped, surfacing this.
    #[error("agent connection task exited before responding")]
    ActorGone,
    /// [`AcpClient::shutdown`] exceeded [`SHUTDOWN_TIMEOUT`]; the task was
    /// aborted and the child left to `ChildGuard`'s drop-reap.
    #[error("agent shutdown timed out after {0:?}")]
    ShutdownTimeout(Duration),
}

/// A live connection to one external ACP agent.
///
/// Cheap to clone-share the command channel (it's an `mpsc::Sender`); the
/// connection task owns the child. Drop is *not* the teardown path — call
/// [`shutdown`](Self::shutdown) so the bound is observed; `Drop` here only
/// signals `Stop` and detaches (mirroring the lesson recorded on #1197: do
/// not rely on drop-reaping as the only teardown path).
pub struct AcpClient {
    cmd_tx: mpsc::Sender<Cmd>,
    join: Option<JoinHandle<Result<(), AcpError>>>,
}

enum Cmd {
    NewSession {
        cwd: PathBuf,
        reply: oneshot::Sender<Result<wire::SessionId, AcpError>>,
    },
    Prompt {
        session_id: wire::SessionId,
        prompt: String,
        updates: mpsc::UnboundedSender<content::Inbound>,
    },
    Cancel {
        session_id: wire::SessionId,
    },
    SetMode {
        session_id: wire::SessionId,
        mode: Mode,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    Stop,
}

impl AcpClient {
    /// Spawn the agent described by `config` and complete the `initialize`
    /// handshake, returning a handle that drives the rest of the protocol.
    ///
    /// Awaits the `initialize` handshake before returning so the caller knows
    /// immediately whether the agent connected and the protocol version agreed.
    /// If the handshake fails, the spawned task is aborted and the child is
    /// left to `ChildGuard`'s drop-reap.
    ///
    /// # Env isolation (gap, recorded)
    ///
    /// `AcpAgent` inherits the host environment and applies `config.env` on
    /// top, unlike `ff-mcp`'s `env_clear()` + allowlist (RFC 0003 §9.2). Env
    /// isolation is **not** an #1202 acceptance criterion; the first cut uses
    /// `AcpAgent` as-is. A fast-follow either uses `AcpAgent::spawn_process()`
    /// with an env-cleared `Command` + the SDK's `Lines` transport + a
    /// replicated `ChildGuard`, or upstreams an `env_clear` option.
    pub async fn connect(config: acp::AcpAgentConfig) -> Result<Self, AcpError> {
        let agent = AcpAgent::new(config);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);
        let (handshake_tx, handshake_rx) = oneshot::channel::<()>();
        let join = tokio::spawn(async move {
            Client
                .builder()
                .name("flowforge")
                .connect_with(agent, async move |cx| {
                    // The SDK does not auto-initialize; the closure owns the
                    // handshake (see the crate's quick-start example).
                    cx.send_request_to(
                        Agent,
                        wire::InitializeRequest::new(acp::schema::ProtocolVersion::V1),
                    )
                    .block_task()
                    .await?;
                    let _ = handshake_tx.send(());
                    drive(cx, cmd_rx).await
                })
                .await
                .map_err(AcpError::from)
        });
        // Wait for the initialize handshake to complete before returning.
        // If the handshake never arrives (join panicked, child failed to
        // spawn, transport errored), the oneshot error surfaces as ActorGone.
        handshake_rx.await.map_err(|_| AcpError::ActorGone)?;
        Ok(Self {
            cmd_tx,
            join: Some(join),
        })
    }

    /// Create a new session rooted at `cwd`. Returns the session id to use with
    /// [`prompt`](Self::prompt)/[`cancel`](Self::cancel)/[`set_mode`](Self::set_mode).
    pub async fn session_new(&self, cwd: PathBuf) -> Result<wire::SessionId, AcpError> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::NewSession { cwd, reply }).await?;
        rx.await.map_err(|_| AcpError::ActorGone)?
    }

    /// Send a prompt and return a stream of [`content::Inbound`] updates ending
    /// in a terminal `AgentEvent::Done` carrying the mapped `stopReason`. The
    /// stream closes when the turn ends, the caller drops it, or the agent dies.
    pub async fn prompt(
        &self,
        session_id: wire::SessionId,
        prompt: String,
    ) -> Result<mpsc::UnboundedReceiver<content::Inbound>, AcpError> {
        let (updates_tx, updates_rx) = mpsc::unbounded_channel();
        self.send(Cmd::Prompt {
            session_id,
            prompt,
            updates: updates_tx,
        })
        .await?;
        Ok(updates_rx)
    }

    /// Send a `session/cancel` notification. The in-flight prompt's response
    /// arrives with `stopReason = cancelled`, ending the stream (AC 4).
    pub async fn cancel(&self, session_id: wire::SessionId) -> Result<(), AcpError> {
        self.send(Cmd::Cancel { session_id }).await
    }

    /// Send a `session/set_mode` request. Best-effort: a failure is returned
    /// to the caller but does not tear down the connection.
    pub async fn set_mode(&self, session_id: wire::SessionId, mode: Mode) -> Result<(), AcpError> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::SetMode {
            session_id,
            mode,
            reply,
        })
        .await?;
        rx.await.map_err(|_| AcpError::ActorGone)?
    }
    /// Stop the agent and await its exit, bounded by [`SHUTDOWN_TIMEOUT`]. On
    /// timeout the task is aborted and the child is left to `ChildGuard`'s
    /// drop-reap (which kills the whole process group synchronously, so it
    /// survives a Tokio runtime that has already wound down — better than
    /// `rmcp`'s `tokio::spawn`-based cleanup).
    pub async fn shutdown(mut self) -> Result<(), AcpError> {
        let _ = self.cmd_tx.send(Cmd::Stop).await;
        let Some(mut join) = self.join.take() else {
            return Ok(());
        };
        tokio::select! {
            // Prefer a clean join to the timeout.
            biased;
            r = &mut join => match r {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(_panic) => Err(AcpError::ActorGone),
            },
            _ = tokio::time::sleep(SHUTDOWN_TIMEOUT) => {
                // Aborting the task drops the `connect_with` future, whose
                // teardown drops `AcpAgent` and its `ChildGuard` — which kills
                // the whole process group synchronously, surviving a runtime
                // that is already winding down (AC 3).
                join.abort();
                Err(AcpError::ShutdownTimeout(SHUTDOWN_TIMEOUT))
            }
        }
    }

    async fn send(&self, cmd: Cmd) -> Result<(), AcpError> {
        self.cmd_tx.send(cmd).await.map_err(|_| AcpError::ActorGone)
    }
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        // Drop is not the teardown path (see #1197): fire-and-forget `Stop`
        // and detach the task so `ChildGuard` reaps on the connection's own
        // drop. Callers that need a bound must use `shutdown`.
        let _ = self.cmd_tx.try_send(Cmd::Stop);
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

/// The connection task's command loop. Runs inside the `connect_with` closure,
/// so every error here is a connection-level error (the closure returning
/// propagates to `connect_with`'s result and reaps the child).
///
/// Command dispatch uses `tokio::select!` so that `Cmd::Cancel` is serviced
/// **during** a prompt rather than queueing until the prompt finishes — the
/// review finding for AC 4 (#1234). The prompt loop runs in a spawned task
/// and sends the `ActiveSession` back when it completes; the main loop reaps
/// it via `reaped_rx`.
async fn drive(cx: ConnectionTo<Agent>, mut cmd_rx: mpsc::Receiver<Cmd>) -> Result<(), acp::Error> {
    let mut sessions: HashMap<Arc<str>, ActiveSessionHandle> = HashMap::new();
    // Per-session cancel signals for in-flight prompts. The `Cmd::Cancel`
    // handler sends the `session/cancel` notification fires the oneshot
    // sender; the prompt task races the oneshot receiver against
    // `session.read_update()` and ends the stream early.
    let mut prompt_cancels: HashMap<Arc<str>, oneshot::Sender<()>> = HashMap::new();
    let (reaped_tx, mut reaped_rx) = tokio::sync::mpsc::unbounded_channel::<ReapedSession>();

    loop {
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    Cmd::NewSession { cwd, reply } => {
                        let result = cx.build_session(&cwd).block_task().start_session().await;
                        match result {
                            Ok(session) => {
                                let id = session.session_id().clone();
                                sessions.insert(Arc::clone(&id.0), ActiveSessionHandle(session));
                                let _ = reply.send(Ok(id));
                            }
                            Err(e) => {
                                let _ = reply.send(Err(AcpError::from(e)));
                            }
                        }
                    }
                    Cmd::Prompt {
                        session_id,
                        prompt,
                        updates,
                    } => {
                        if let Some(handle) = sessions.remove(&session_id.0) {
                            let (cancel_tx, cancel_rx) = oneshot::channel();
                            prompt_cancels.insert(session_id.0.clone(), cancel_tx);
                            let reaped_tx = reaped_tx.clone();
                            let sid = session_id.0.clone();
                            let prompt = prompt.clone();
                            tokio::spawn(async move {
                                let session = drive_prompt(
                                    handle.0, &prompt, updates, cancel_rx,
                                ).await;
                                let _ = reaped_tx.send(ReapedSession {
                                    session_id: sid,
                                    handle: ActiveSessionHandle(session),
                                });
                            });
                        }
                        // Unknown session: `updates` is dropped, so the caller's
                        // stream closes empty — a loud failure rather than a hang.
                    }
                    Cmd::Cancel { session_id } => {
                        // Send the protocol notification to the agent immediately.
                        // This works even during a prompt because `cx` is a
                        // separate clone of the connection (BLOCKER 1 fix).
                        cx.send_notification_to(
                            Agent,
                            wire::CancelNotification::new(session_id.clone()),
                        )?;
                        // Signal the in-flight prompt task to end the host's
                        // stream with a Cancelled Done immediately, without
                        // waiting for the agent's acknowledgement.
                        if let Some(cancel_tx) = prompt_cancels.remove(&session_id.0) {
                            let _ = cancel_tx.send(());
                        }
                    }
                    Cmd::SetMode {
                        session_id,
                        mode,
                        reply,
                    } => {
                        let req = wire::SetSessionModeRequest::new(
                            session_id,
                            wire::SessionModeId::new(mode::mode_id(mode)),
                        );
                        let outcome = cx
                            .send_request_to(Agent, req)
                            .block_task()
                            .await
                            .map(|_| ())
                            .map_err(AcpError::from);
                        let _ = reply.send(outcome);
                    }
                    Cmd::Stop => return Ok(()),
                }
            }
            // Reap completed prompt tasks and return the session to the map.
            Some(reaped) = reaped_rx.recv() => {
                prompt_cancels.remove(&reaped.session_id);
                sessions.insert(reaped.session_id, reaped.handle);
            }
            else => break,
        }
    }
    Ok(())
}

/// A session returned by a completed prompt task, ready to be re-inserted
/// into the session map so the next prompt on the same session reuses it.
struct ReapedSession {
    session_id: Arc<str>,
    handle: ActiveSessionHandle,
}

/// A `session/update` stream's [`AgentEvent`]s flow through this wrapper so
/// the `Link` type parameter stays in one place.
struct ActiveSessionHandle(ActiveSession<'static, Agent>);

/// Drive one prompt to completion: send it, then drain `session/update`
/// notifications through [`content::inbound`] and respond to agent→client
/// requests (fs/*, terminal/*, request_permission) with stub errors so the
/// agent's turn completes instead of hanging.
///
/// Accepts a `cancel_rx` [`oneshot::Receiver`] that the main loop fires when
/// `Cmd::Cancel` arrives, so the host stream ends with a `Cancelled` Done
/// without blocking on the agent (BLOCKER 1 fix).
async fn drive_prompt(
    session: ActiveSession<'static, Agent>,
    prompt: &str,
    updates: mpsc::UnboundedSender<content::Inbound>,
    mut cancel_rx: oneshot::Receiver<()>,
) -> ActiveSession<'static, Agent> {
    let mut session = session;
    if session.send_prompt(prompt).is_err() {
        return session; // caller's stream closes (sender dropped)
    }
    // ACP leaves `messageId` optional on a chunk and absent on a `ToolCall`;
    // `AgentEvent` keys every event on one. Use the session id as the stable
    // fallback so every surfaced event is correlatable to its session.
    let fallback = session.session_id().0.to_string();
    loop {
        tokio::select! {
            biased;
            // When cancel fires, end the host stream with a Cancelled Done
            // without waiting for the agent's StopReason. The protocol
            // notification was already sent by the `Cmd::Cancel` handler.
            _ = &mut cancel_rx => {
                let _ = updates.send(content::Inbound::Agent(done_event(
                    fallback,
                    Some(ff_core::StopReason::Cancelled),
                )));
                return session;
            }
            msg = session.read_update() => {
                match msg {
                    Ok(SessionMessage::SessionMessage(dispatch)) => {
                        handle_session_message(dispatch, &fallback, &updates).await;
                    }
                    Ok(SessionMessage::StopReason(stop)) => {
                        let internal = content::inbound_stop_reason(stop);
                        let _ = updates.send(content::Inbound::Agent(done_event(
                            fallback,
                            internal,
                        )));
                        return session;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "agent stream read failed; ending prompt");
                        return session;
                    }
                    // `SessionMessage` is `#[non_exhaustive]`; a future variant
                    // has no FlowForge surface yet. End the stream rather than
                    // spin on it — the host sees a closed channel instead of a
                    // silent hang.
                    Ok(_) => {
                        tracing::debug!("ignoring unknown SessionMessage variant; ending prompt");
                        return session;
                    }
                }
            }
        }
    }
}

/// Handle one inbound `Dispatch` from the agent during a prompt turn:
///
/// - `session/update` notifications → extract the update and forward to the stream.
/// - `session/request_permission` → respond `cancelled` so the agent resumes.
/// - Any other request → respond with a JSON-RPC error (`method not implemented`).
/// - Unknown notifications → ignored (no response needed).
/// - Responses → ignored (they belong to our requests, not the agent's).
async fn handle_session_message(
    dispatch: Dispatch,
    fallback: &str,
    updates: &mpsc::UnboundedSender<content::Inbound>,
) {
    let _ =
        MatchDispatch::new(dispatch)
            // session/update notification → extract and forward.
            .if_notification(async |notif: wire::SessionNotification| {
                let inbound = content::inbound(&notif.update, fallback);
                let _ = updates.send(inbound);
                Ok(())
            })
            .await
            // session/request_permission → respond cancelled.
            .if_request(
                async |_req: wire::RequestPermissionRequest,
                       responder: agent_client_protocol::Responder<
                    wire::RequestPermissionResponse,
                >| {
                    responder.respond(wire::RequestPermissionResponse::new(
                        wire::RequestPermissionOutcome::Cancelled,
                    ))?;
                    Ok(())
                },
            )
            .await
            // Everything else: respond with error so the agent doesn't hang.
            .otherwise(|dispatch: Dispatch| async move {
                match dispatch {
                    Dispatch::Request(untyped, responder) => {
                        let method = untyped.method().to_string();
                        let err = agent_client_protocol::util::internal_error(format!(
                            "{method} not implemented by this host"
                        ));
                        let _ = responder.respond_with_error(err);
                    }
                    Dispatch::Notification(_) => {
                        // Unknown notification — ignore.
                    }
                    Dispatch::Response(_, _) => {
                        // Response to one of our requests — ignore.
                    }
                }
                Ok(())
            })
            .await;
}

/// Minimal `AgentEvent::Done` — only `message_id` and `stop_reason` are
/// meaningful from an external agent's turn; the perf/counters are absent.
fn done_event(message_id: String, stop_reason: Option<ff_core::StopReason>) -> AgentEvent {
    AgentEvent::Done {
        message_id,
        final_message: None,
        stop_reason,
        turns: None,
        token_count: None,
        prefill_estimates: None,
        prompt_latency_ms: None,
        tier2_ms: None,
        tier1_fires: None,
        tier2_fires: None,
        retrieve_calls: None,
        cache_hit_tokens: None,
        cache_miss_tokens: None,
        breakdown: None,
        usage: None,
        budget_tokens: None,
    }
}
