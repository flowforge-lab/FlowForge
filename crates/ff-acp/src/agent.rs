//! FlowForge **as an ACP agent**: an [`AcpAgent`] serves the agent half of the
//! protocol over stdio so an ACP client (e.g. Zed) can drive it.
//!
//! This is the server-side mirror of [`crate::client`]. Where `client` spawns an
//! external agent and drives the client→agent requests, this module *answers*
//! those requests: `initialize`, `session/new`, `session/load`, `session/prompt`,
//! `session/set_mode`, `session/delete`, `authenticate`, and the `session/cancel`
//! notification.
//!
//! ## Layering
//!
//! The protocol wiring lives here; the *host* (LLM provider, tool registry,
//! session store, permission matrix, system-prompt inputs) is injected through
//! the [`AcpHost`] trait. FlowForge's real host assembly (`host::load_provider`,
//! `build_registry_with_mcp`, …) lives in `apps/cli`, which cannot be reached
//! from a library crate — so the CLI implements `AcpHost` and calls [`serve`].
//!
//! ## Concurrency
//!
//! `session/prompt` runs a full agent turn, which is long. The SDK dispatch loop
//! processes one inbound message at a time, so a turn that blocks the handler
//! would also block `session/cancel`. The prompt handler therefore spawns the
//! turn on the connection (`cx.spawn`) and moves the [`Responder`] into that
//! task, letting the dispatch loop keep servicing `session/cancel` while the turn
//! streams. Cancellation is delivered through a [`CancelToken`] registered in the
//! [`SessionRegistry`].

use std::sync::Arc;

use agent_client_protocol::schema::v1 as wire;
use agent_client_protocol::schema::v1::{
    DeleteSessionRequest, DeleteSessionResponse, SessionCapabilities, SessionDeleteCapabilities,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{Agent, Client, ConnectionTo, Responder, Result as AcpResult, Stdio};
use async_trait::async_trait;
use ff_agent::{
    AgentEvent, ApprovalOutcome, Approver, CancelToken, SystemPromptInputs, ToolContext,
};
use ff_core::{DenyReason, Mode, ReasoningVisibility, Role};
use ff_llm::Provider;
use ff_session::SessionStore;
use ff_tools::Safety;
use tokio::sync::mpsc;

use crate::content;
use crate::session::SessionRegistry;
use crate::{advertise, mode, permission};

/// The host resources the ACP server needs to answer a turn.
///
/// FlowForge assembles these in `apps/cli` (provider, registry, store, matrix,
/// system-prompt inputs) and implements this trait so the protocol layer stays
/// free of host wiring.
pub trait AcpHost: Send + Sync + 'static {
    /// The LLM provider driving the turn.
    fn provider(&self) -> &dyn Provider;
    /// The session store holding conversation history.
    fn store(&self) -> &SessionStore;
    /// The model id passed to the provider.
    fn model(&self) -> &str;
    /// Build a [`ToolContext`] for a turn under `mode`, wiring `approver` as the
    /// approval surface. Borrows must outlive the turn, so the host owns the
    /// backing registry/root/matrix and hands out borrows here.
    fn tool_context<'a>(&'a self, mode: Mode, approver: &'a dyn Approver) -> ToolContext<'a>;
    /// Build the [`SystemPromptInputs`] for `mode`, borrowing the host's owned
    /// skills / memory / user context.
    fn prompt_inputs(&self, mode: Mode) -> SystemPromptInputs<'_>;
}

/// Serve the ACP agent over stdio until the client disconnects.
///
/// A thin wrapper over [`connect`] that uses the real stdio transport. `host` supplies
/// every turn's resources.
pub async fn serve<H: AcpHost>(host: Arc<H>) -> AcpResult<()> {
    connect(host, Stdio::new()).await
}

/// Build the [`Agent`] connection over an arbitrary transport and run until it closes.
///
/// Registers a handler per client→agent method, then hands the wired role to `transport`.
/// Production uses [`Stdio`] (see [`serve`]); tests inject an in-memory
/// [`Channel`](agent_client_protocol::Channel) so a client and agent can talk without a
/// child process.
pub async fn connect<H: AcpHost>(
    host: Arc<H>,
    transport: impl agent_client_protocol::ConnectTo<Agent>,
) -> AcpResult<()> {
    let sessions = Arc::new(SessionRegistry::new());

    Agent
        .builder()
        .name("flowforge")
        .on_receive_request(
            async move |_req: wire::InitializeRequest, responder: Responder<_>, _cx| {
                let resp = wire::InitializeResponse::new(ProtocolVersion::LATEST)
                    .agent_capabilities(wire::AgentCapabilities::new().session_capabilities(
                        SessionCapabilities::new().delete(SessionDeleteCapabilities::new()),
                    ));
                responder.respond(resp)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let host = Arc::clone(&host);
                let sessions = Arc::clone(&sessions);
                async move |_req: wire::NewSessionRequest, responder: Responder<_>, _cx| {
                    let session = host.store().create_session(None);
                    let id = wire::SessionId::new(session.id.as_str());
                    sessions.set_mode(&id, Mode::default());
                    let resp =
                        wire::NewSessionResponse::new(id).modes(mode::mode_state(Mode::default()));
                    responder.respond(resp)
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: wire::LoadSessionRequest, responder: Responder<_>, _cx| {
                responder.respond(wire::LoadSessionResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: wire::AuthenticateRequest, responder: Responder<_>, _cx| {
                responder.respond(wire::AuthenticateResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let sessions = Arc::clone(&sessions);
                async move |req: wire::SetSessionModeRequest, responder: Responder<_>, _cx| {
                    match mode::mode_from_id(&req.mode_id) {
                        Some(mode) => {
                            sessions.set_mode(&req.session_id, mode);
                            responder.respond(wire::SetSessionModeResponse::new())
                        }
                        None => responder
                            .respond_with_error(agent_client_protocol::Error::invalid_params()),
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let host = Arc::clone(&host);
                let sessions = Arc::clone(&sessions);
                async move |req: DeleteSessionRequest, responder: Responder<_>, _cx| {
                    sessions.delete_session(&req.session_id);
                    host.store().delete_session(req.session_id.0.as_ref());
                    responder.respond(DeleteSessionResponse::new())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let host = Arc::clone(&host);
                let sessions = Arc::clone(&sessions);
                async move |req: wire::PromptRequest,
                            responder: Responder<wire::PromptResponse>,
                            cx: ConnectionTo<Client>| {
                    let host = Arc::clone(&host);
                    let sessions = Arc::clone(&sessions);
                    cx.clone().spawn(async move {
                        let stop = run_prompt(host, sessions, cx, req).await;
                        responder.respond(wire::PromptResponse::new(stop))
                    })?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            {
                let sessions = Arc::clone(&sessions);
                async move |note: wire::CancelNotification, _cx| {
                    sessions.cancel(&note.session_id);
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(transport)
        .await
}

/// Run one `session/prompt` turn to completion, streaming `session/update`
/// notifications back to the client, and return the ACP stop reason.
async fn run_prompt<H: AcpHost>(
    host: Arc<H>,
    sessions: Arc<SessionRegistry>,
    cx: ConnectionTo<Client>,
    req: wire::PromptRequest,
) -> wire::StopReason {
    let session_id = req.session_id.clone();
    let mode = sessions.mode(&session_id);

    // Extract the user's prompt text and append it to the session history.
    let prompt_text = prompt_text(&req.prompt);
    host.store()
        .add_message(session_id.0.as_ref(), Role::User, prompt_text);

    // Register a cancel token so `session/cancel` can interrupt this turn.
    let cancel = CancelToken::new();
    sessions.register(&session_id, cancel.clone());

    // Pump `AgentEvent`s from the sync `on_event` callback to an async task that
    // sends `session/update` notifications on the connection.
    let (tx, mut rx) = mpsc::unbounded_channel::<wire::SessionUpdate>();
    let pump = {
        let cx = cx.clone();
        let session_id = session_id.clone();
        tokio::spawn(async move {
            while let Some(update) = rx.recv().await {
                let note = wire::SessionNotification::new(session_id.clone(), update);
                let _ = cx.send_notification(note);
            }
        })
    };

    let approver = AcpApprover::new(cx.clone(), session_id.clone());
    let mut tools = host.tool_context(mode, &approver);
    // ACP has no wire representation for a `Deny` cell, so the protocol layer — not the
    // host — is where that gap is closed: restrict the model's visible tool set to
    // exactly what crosses the ACP boundary in this mode. `advertised_for_acp` drops
    // every `Deny` tool in every mode, which `advertised_tools`' tier-only filter does
    // not (in Act it would otherwise show a per-tool-denied tool).
    tools.mode = mode;
    tools.allowed = Some(
        advertise::advertised_for_acp(tools.registry, mode, tools.matrix)
            .into_iter()
            .collect(),
    );
    let inputs = host.prompt_inputs(mode);

    let on_event = move |event: AgentEvent| {
        if let Some(update) = content::outbound(&event) {
            let _ = tx.send(update);
        }
    };

    let result = ff_agent::run_session_turn(
        host.provider(),
        host.store(),
        &tools,
        session_id.0.as_ref(),
        host.model(),
        &inputs,
        false,
        ReasoningVisibility::default(),
        cancel.clone(),
        on_event,
    )
    .await;

    sessions.remove(&session_id);
    pump.abort();

    match result {
        Ok(msg) => content::outbound_stop_reason(msg.stop_reason),
        // A turn error is `AgentError::Llm` — a transport/model failure, never a
        // cancellation (cancel returns `Ok` with an empty message that resolves to
        // `Cancelled`). Reporting it as `Cancelled` would mask a real defect as a
        // benign stop, so map it to `EndTurn`. ACP has no generic error stop reason;
        // the client still saw any error/retry chunks streamed via `on_event`.
        Err(_) => content::outbound_stop_reason(None),
    }
}

/// Concatenate the text of every [`wire::ContentBlock::Text`] in a prompt.
fn prompt_text(blocks: &[wire::ContentBlock]) -> String {
    let mut out = String::new();
    for block in blocks {
        if let wire::ContentBlock::Text(text) = block {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&text.text);
        }
    }
    out
}

/// Bridges FlowForge's [`Approver`] onto ACP's `session/request_permission`.
///
/// When a tool call needs approval, this sends a `session/request_permission`
/// request to the client and maps the selected option back to an
/// [`ApprovalOutcome`] via [`permission::outcome_to_approval`].
struct AcpApprover {
    cx: ConnectionTo<Client>,
    session_id: wire::SessionId,
}

impl AcpApprover {
    fn new(cx: ConnectionTo<Client>, session_id: wire::SessionId) -> Self {
        Self { cx, session_id }
    }
}

#[async_trait]
impl Approver for AcpApprover {
    async fn approve(
        &self,
        _message_id: &str,
        call_id: &str,
        name: &str,
        _safety: Safety,
        _args: &serde_json::Value,
    ) -> ApprovalOutcome {
        let fields = wire::ToolCallUpdateFields::new().title(name);
        let tool_call = wire::ToolCallUpdate::new(wire::ToolCallId::new(call_id), fields);
        let req = wire::RequestPermissionRequest::new(
            self.session_id.clone(),
            tool_call,
            permission::ask_options(),
        );
        match self.cx.send_request_to(Client, req).block_task().await {
            Ok(resp) => permission::outcome_to_approval(&resp.outcome),
            Err(_) => ApprovalOutcome::Denied(DenyReason::Cancelled),
        }
    }
}

#[cfg(test)]
mod tests;
