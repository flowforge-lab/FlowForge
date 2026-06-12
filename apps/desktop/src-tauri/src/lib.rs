//! Thin Tauri shell. Per the SOP, this layer contains only command/event glue —
//! all business logic lives in the `ff-*` crates. Each handler deserializes,
//! calls into a crate, and returns. Streaming responses go out as Tauri events.

mod state;

use async_trait::async_trait;
use ff_agent::{run_turn, AgentEvent, Approver, CancelToken, ToolContext};
use ff_core::events::{
    IntentionSignal, TokenEvent, ToolApprovalRequestEvent, ToolCallEvent, ToolResultEvent,
    TurnDoneEvent, TurnErrorEvent,
};
use ff_core::{Message, Role, Session};
use ff_tools::Safety;
use state::AppState;
use std::sync::Arc;
use tauri::{Emitter, State};

/// Routes write/dangerous tool calls through a UI confirmation. Read-only calls
/// never reach this approver — the agent loop short-circuits them.
struct UiApprover {
    app: tauri::AppHandle,
    state: Arc<AppState>,
    session_id: String,
}

#[async_trait]
impl Approver for UiApprover {
    async fn approve(
        &self,
        message_id: &str,
        call_id: &str,
        name: &str,
        safety: Safety,
        args: &serde_json::Value,
    ) -> bool {
        let safety_str = match safety {
            // Unreachable in practice: the agent loop short-circuits ReadOnly
            // before calling the approver. Kept for the exhaustive match.
            Safety::ReadOnly => "readOnly",
            Safety::Write => "write",
            Safety::Dangerous => "dangerous",
        };
        let rx = self.state.register_approval(call_id, &self.session_id);
        let _ = self.app.emit(
            "tool:approval-request",
            ToolApprovalRequestEvent {
                session_id: self.session_id.clone(),
                message_id: message_id.to_string(),
                call_id: call_id.to_string(),
                tool: name.to_string(),
                args: args.clone(),
                safety: safety_str.to_string(),
            },
        );
        // Sender dropped (cancel) -> RecvError -> deny.
        rx.await.unwrap_or(false)
    }
}

type CmdResult<T> = Result<T, String>;

#[tauri::command]
fn create_session(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    goal: Option<String>,
) -> Session {
    let session = state.store.create_session(goal.clone());
    if let Some(goal) = goal {
        let _ = app.emit(
            "signal:intention",
            IntentionSignal {
                session_id: session.id.clone(),
                goal,
            },
        );
    }
    session
}

#[tauri::command]
fn list_sessions(state: State<'_, Arc<AppState>>) -> Vec<Session> {
    state.store.list_sessions()
}

#[tauri::command]
fn get_messages(state: State<'_, Arc<AppState>>, session_id: String) -> Vec<Message> {
    state.store.get_messages(&session_id)
}

#[tauri::command]
fn cancel_turn(state: State<'_, Arc<AppState>>, session_id: String) {
    if let Some(token) = state.take_cancel(&session_id) {
        token.cancel();
    }
    // Pending approvals for this session would block the turn forever otherwise.
    state.cancel_pending_approvals(&session_id);
}

/// Frontend response to a [`ToolApprovalRequestEvent`]. Wakes the awaiting approver.
#[tauri::command]
fn respond_approval(state: State<'_, Arc<AppState>>, call_id: String, approved: bool) {
    state.resolve_approval(&call_id, approved);
}

/// Persists the user message, then spawns the assistant turn. Tokens stream back
/// over `turn:token`; completion via `turn:done`; failures via `turn:error`.
/// Returns the user message id immediately.
#[tauri::command]
fn send_message(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    session_id: String,
    content: String,
) -> CmdResult<String> {
    let user_msg = state.store.add_message(&session_id, Role::User, content);

    let cancel = CancelToken::new();
    state.register_cancel(&session_id, cancel.clone());

    let state = state.inner().clone();
    let model = state.model.clone();
    tauri::async_runtime::spawn(async move {
        let sid = session_id.clone();
        let approver = UiApprover {
            app: app.clone(),
            state: state.clone(),
            session_id: sid.clone(),
        };
        let tool_ctx = ToolContext {
            registry: &state.tools,
            root: &state.workspace_root,
            approve: &approver,
            max_iterations: 8,
        };
        let result = run_turn(
            state.provider.as_ref(),
            &state.store,
            &tool_ctx,
            &sid,
            &model,
            cancel,
            |event| match event {
                AgentEvent::Token { message_id, delta } => {
                    let _ = app.emit(
                        "turn:token",
                        TokenEvent {
                            session_id: sid.clone(),
                            message_id,
                            delta,
                        },
                    );
                }
                AgentEvent::ToolCallStarted {
                    message_id,
                    call_id,
                    name,
                    args,
                } => {
                    let _ = app.emit(
                        "tool:call",
                        ToolCallEvent {
                            session_id: sid.clone(),
                            message_id,
                            call_id,
                            tool: name,
                            args,
                        },
                    );
                }
                AgentEvent::ToolCallFinished {
                    message_id,
                    call_id,
                    success,
                    result,
                } => {
                    let _ = app.emit(
                        "tool:result",
                        ToolResultEvent {
                            session_id: sid.clone(),
                            message_id,
                            call_id,
                            success,
                            result,
                        },
                    );
                }
                AgentEvent::Done { message_id } => {
                    let _ = app.emit(
                        "turn:done",
                        TurnDoneEvent {
                            session_id: sid.clone(),
                            message_id,
                        },
                    );
                }
                AgentEvent::Error { message } => {
                    let _ = app.emit(
                        "turn:error",
                        TurnErrorEvent {
                            session_id: sid.clone(),
                            message,
                        },
                    );
                }
            },
        )
        .await;

        if let Err(e) = result {
            let _ = app.emit(
                "turn:error",
                TurnErrorEvent {
                    session_id: session_id.clone(),
                    message: e.to_string(),
                },
            );
        }
        state.take_cancel(&session_id);
    });

    Ok(user_msg.id)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(Arc::new(AppState::new()))
        .invoke_handler(tauri::generate_handler![
            create_session,
            list_sessions,
            get_messages,
            send_message,
            cancel_turn,
            respond_approval,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
