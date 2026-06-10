//! Thin Tauri shell. Per the SOP, this layer contains only command/event glue —
//! all business logic lives in the `ff-*` crates. Each handler deserializes,
//! calls into a crate, and returns. Streaming responses go out as Tauri events.

mod state;

use ff_agent::{run_turn, AgentEvent, ApprovalFn, CancelToken, ToolContext};
use ff_core::events::{
    IntentionSignal, TokenEvent, ToolCallEvent, ToolResultEvent, TurnDoneEvent, TurnErrorEvent,
};
use ff_core::{Message, Role, Session};
use state::AppState;
use std::sync::Arc;
use tauri::{Emitter, State};

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
        // TODO(M2 PR-B): route Write/Dangerous calls to a UI confirmation. Until the
        // approval surface lands, the shell auto-approves; read-only calls bypass
        // this regardless. The agent loop already enforces the gate.
        let approve: Box<ApprovalFn> = Box::new(|_name, _safety, _args| true);
        let tool_ctx = ToolContext {
            registry: &state.tools,
            root: &state.workspace_root,
            approve: approve.as_ref(),
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
