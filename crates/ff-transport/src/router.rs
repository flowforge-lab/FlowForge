use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ff_agent::{run_turn, AgentEvent, Approver, CancelToken, SystemPrompt, ToolContext};
use ff_core::ReasoningVisibility;
use ff_core::{Egress, Mode, PermissionMatrix, Role};
use ff_llm::Provider;
use ff_session::SessionStore;
use ff_tools::ToolRegistry;
use tracing::{debug, info, warn};

use crate::channel_map::ChannelMap;
use crate::transport::MessageTransport;
use crate::types::{ChannelId, Notification};

/// How often the per-turn flusher copies the accumulated token buffer to the
/// response stream while the model is still generating.
///
/// This is intentionally *coarser* than the transport's own throttle (Slack's
/// `EDIT_THROTTLE` is 500ms): the flusher only decides *when to offer* the
/// latest text, and the stream coalesces what lands inside its window and skips
/// unchanged bodies. Per-token delivery would be O(n²) in clones for a long
/// turn; one copy every cadence is the cheap middle ground.
const STREAM_FLUSH_INTERVAL: Duration = Duration::from_millis(250);

/// A coarse label for a failed turn, safe to record without being asked.
///
/// Delegates to [`ff_llm::LlmError::log_kind`], which owns the reasoning about
/// which fields of an `LlmError` may be persisted. The desktop app needs the same
/// guarantee at its own `tracing` sites (#1118), and keeping one implementation on
/// the error type means the two cannot drift into disagreeing about what is safe.
pub(crate) fn turn_failure_kind(error: &ff_agent::AgentError) -> String {
    let ff_agent::AgentError::Llm(llm) = error;
    llm.log_kind().into_owned()
}

/// Configuration for the headless router.
pub struct RouterConfig {
    /// Agent autonomy mode for messaging sessions.
    pub mode: Mode,
    /// Network egress policy.
    pub egress: Egress,
    /// Maximum tool iterations per turn.
    pub max_iterations: usize,
    /// Workspace root (tools are jailed here).
    pub workspace: PathBuf,
    /// Model identifier to use for turns. Callers should set this explicitly;
    /// the default is a reasonable fallback but may not match the configured
    /// provider (e.g. OpenRouter users need their own model string).
    pub model: String,
    /// Optional system prompt prepended to every turn.
    pub system_prompt: Option<String>,
    /// Enable extended thinking / reasoning. Defaults to `false` for headless
    /// transports since the router currently discards reasoning deltas (they are
    /// not streamed to the transport). Set to `true` if reasoning improves quality
    /// enough to justify the added latency.
    pub enable_reasoning: bool,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Act,
            egress: Egress::default(),
            max_iterations: 25,
            workspace: PathBuf::from("."),
            model: String::from("claude-sonnet-4-20250514"),
            system_prompt: None,
            enable_reasoning: false,
        }
    }
}

/// The headless session orchestrator. Maps inbound messages from a
/// [`MessageTransport`] to FlowForge sessions, drives agent turns, and streams
/// responses back through the transport.
pub struct Router {
    config: RouterConfig,
    channel_map: ChannelMap,
    store: Arc<SessionStore>,
    registry: Arc<ToolRegistry>,
    provider: Arc<dyn Provider>,
    matrix: PermissionMatrix,
}

impl Router {
    pub fn new(
        config: RouterConfig,
        channel_map: ChannelMap,
        store: Arc<SessionStore>,
        registry: Arc<ToolRegistry>,
        provider: Arc<dyn Provider>,
    ) -> Self {
        Self {
            config,
            channel_map,
            store,
            registry,
            provider,
            matrix: PermissionMatrix::default(),
        }
    }

    /// Run the router loop: receive messages from the transport, route them to
    /// sessions, run turns, and stream responses back. Runs until the transport
    /// returns `None` (closed).
    ///
    /// The `approver` is injected rather than built here so an interactive
    /// approver (e.g. Slack buttons, #912 T4) can share the transport's
    /// connection: both are constructed per-connection by the host and passed
    /// in together. Headless callers pass a `MessagingApprover`.
    pub async fn run(&mut self, transport: &mut dyn MessageTransport, approver: &dyn Approver) {
        info!(transport = transport.name(), "router started");

        while let Some(msg) = transport.recv().await {
            let channel = msg.channel.clone();
            debug!(
                transport = transport.name(),
                channel = %channel.platform_id,
                sender = %msg.sender_id,
                "inbound message"
            );

            // Resolve or create session for this channel.
            let session_id = self.resolve_session(&channel);

            // Persist the user message.
            self.store
                .add_message(&session_id, Role::User, msg.text.clone());

            // Notify transport that we're starting.
            transport.notify(&channel, Notification::TurnStarted);

            // Open a response stream, anchored to the triggering message's thread
            // so the reply lands where it was asked (#1098); `resolve_session` keys
            // on `channel` alone, so this steers delivery only, not session identity.
            let stream = transport.begin_response(&channel, msg.reply_thread.as_deref());

            // Build tool context with mode + egress from config (#2 fix).
            // The approver is injected via `run` (#1056) so interactive
            // approvers can share the transport connection.
            let mut tools = ToolContext::new(
                &self.registry,
                &self.config.workspace,
                approver,
                self.config.max_iterations,
                &self.matrix,
            );
            tools.mode = self.config.mode;
            tools.egress = self.config.egress;

            // The sync callback cannot await (deadlock starves the provider
            // stream), so token deltas accumulate in a shared buffer while a
            // background flusher task copies it to the response stream on a
            // cadence — RFC 0021 §5.1 streaming edits, not a single end-of-turn
            // post. The flusher owns the stream, so it also owns the guaranteed
            // final flush + `finish`, and is joined before the loop continues.
            let token_buf = Arc::new(Mutex::new(String::new()));
            let turn_done = Arc::new(AtomicBool::new(false));
            let flusher_buf = Arc::clone(&token_buf);
            let flusher_done = Arc::clone(&turn_done);
            let flusher = tokio::spawn(async move {
                let mut interval = tokio::time::interval(STREAM_FLUSH_INTERVAL);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    interval.tick().await;
                    let text = flusher_buf.lock().unwrap().clone();
                    if !text.is_empty() {
                        stream.chunk(&text).await;
                    }
                    if flusher_done.load(Ordering::SeqCst) {
                        break;
                    }
                }
                // One final copy of the complete buffer, then finish. Without
                // it, the last tokens (appended after the flusher's final tick)
                // would be dropped: `finish` only flushes whatever the stream
                // last saw.
                let text = flusher_buf.lock().unwrap().clone();
                if !text.is_empty() {
                    stream.chunk(&text).await;
                }
                stream.finish().await;
            });

            // If *this* turn's future is dropped instead of run to completion
            // (a test timing out mid-turn, a host aborting), the flusher task
            // above would otherwise leak and spin on its interval forever.
            // Bind its lifetime to this scope, and disarm the abort before the
            // cooperative join below takes over.
            struct AbortOnDrop {
                handle: tokio::task::AbortHandle,
            }
            impl Drop for AbortOnDrop {
                fn drop(&mut self) {
                    self.handle.abort();
                }
            }
            let _flusher_guard = AbortOnDrop {
                handle: flusher.abort_handle(),
            };

            let cancel = CancelToken::new();
            // NOTE: volatile tail (cwd, time, ambient context) is not populated
            // here — matches CLI one-shot behavior. Long-lived messaging sessions
            // won't see environment-aware prompts until a UserContext parameter is
            // threaded through Router::run.
            let system_prompt = self.config.system_prompt.as_ref().map(|s| SystemPrompt {
                stable: s.clone(),
                volatile: String::new(),
            });
            let system_prompt_ref = system_prompt.as_ref();

            // Buffer token deltas in the sync callback; the flusher task above is
            // what actually streams them to the transport.
            let token_cb = Arc::clone(&token_buf);

            // Run the agent turn.
            let result = run_turn(
                self.provider.as_ref(),
                &self.store,
                &tools,
                &session_id,
                &self.config.model,
                system_prompt_ref,
                self.config.enable_reasoning,
                ReasoningVisibility::default(),
                cancel,
                |event| match &event {
                    AgentEvent::Token { delta, .. } => {
                        token_cb.lock().unwrap().push_str(delta);
                    }
                    AgentEvent::ToolCallStarted { name, .. } => {
                        transport.notify(&channel, Notification::ToolCall { name: name.clone() });
                    }
                    // AgentEvent::Error is a tool-level or loop-level error that the
                    // model may recover from; only surface fatal errors via the outer
                    // Err path (#6 fix — avoid duplicate Error notifications).
                    _ => {}
                },
            )
            .await;

            // Stop the flusher and wait for it to drain + finish the stream.
            turn_done.store(true, Ordering::SeqCst);
            // Disarm: `forget`, not `drop` — dropping the guard would abort the
            // flusher, which is exactly what the normal join path must not do.
            std::mem::forget(_flusher_guard);
            flusher.await.expect("stream flusher task panicked");
            transport.notify(&channel, Notification::TurnFinished);

            match result {
                Ok(_msg) => {
                    debug!(session_id = %session_id, "turn completed");
                }
                Err(e) => {
                    // Classified summary at `warn`, full error only at `debug`.
                    // See `turn_failure_kind` for why the two are split.
                    warn!(
                        session_id = %session_id,
                        kind = turn_failure_kind(&e),
                        "turn failed"
                    );
                    debug!(session_id = %session_id, error = %e, "turn failure detail");
                    transport.notify(&channel, Notification::Error(e.to_string()));
                }
            }
        }

        info!(
            transport = transport.name(),
            "router stopped (transport closed)"
        );
    }

    fn resolve_session(&mut self, channel: &ChannelId) -> String {
        if let Some(id) = self.channel_map.get(channel) {
            id.to_string()
        } else {
            let session = self.store.create_session(None);
            let id = session.id.clone();
            self.channel_map.insert(channel.clone(), id.clone());
            info!(
                channel = %channel.platform_id,
                session_id = %id,
                "created new session for channel"
            );
            id
        }
    }
}
