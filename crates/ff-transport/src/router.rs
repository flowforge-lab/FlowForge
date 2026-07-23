use std::path::PathBuf;
use std::sync::Arc;

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

            // Open a response stream.
            let stream = transport.begin_response(&channel);

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

            // Buffer token deltas in the sync callback, flush async after the
            // turn completes. This avoids `Handle::block_on` inside the closure
            // which deadlocks on current_thread runtimes and starves workers on
            // multi_thread runtimes (#1 fix).
            let mut token_buf = String::new();

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
                        token_buf.push_str(delta);
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

            // Flush buffered tokens to the response stream.
            if !token_buf.is_empty() {
                stream.chunk(&token_buf).await;
            }
            stream.finish().await;
            transport.notify(&channel, Notification::TurnFinished);

            match result {
                Ok(_msg) => {
                    debug!(session_id = %session_id, "turn completed");
                }
                Err(e) => {
                    warn!(session_id = %session_id, error = %e, "turn failed");
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
