//! Thin Tauri shell. Per the SOP, this layer contains only command/event glue —
//! all business logic lives in the `ff-*` crates. Each handler deserializes,
//! calls into a crate, and returns. Streaming responses go out as Tauri events.

mod state;
mod tools;

use async_trait::async_trait;
use ff_agent::{run_turn, AgentEvent, Approver, CancelToken, ToolContext};
use ff_core::events::{
    IntentionSignal, SkillInstallApprovalRequestEvent, TokenEvent, ToolApprovalRequestEvent,
    ToolCallEvent, ToolResultEvent, TurnDoneEvent, TurnErrorEvent,
};
use ff_core::{Message, ProviderConfig, ProviderKind, Role, Session, SkillManifest};
use ff_tools::Safety;
use state::AppState;
use std::sync::Arc;
use tauri::{Emitter, State};
use uuid::Uuid;

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

/// Set a session's display title (server-truth). Used by the sidebar rename and
/// to lift legacy localStorage titles to the backend.
#[tauri::command]
fn rename_session(state: State<'_, Arc<AppState>>, session_id: String, title: String) {
    state.store.set_title(&session_id, title);
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
    // Snapshot the provider from the current config for this turn; a settings
    // change between turns is picked up on the next `send_message`.
    let (provider, model) = state.build_provider();
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
        // Skills + ambient context for this turn (RFC 0001 §4, RFC 0002 phase 1).
        // Phenotype selection (persona + active skill bodies) lands in M3.4; for now
        // we inject installed-skill descriptions and the current local time only.
        let skills = state.skills_snapshot();
        let user_ctx = ff_agent::UserContext::now();
        let system_prompt = ff_agent::build_system_prompt(None, &skills, &[], &user_ctx);

        let result = run_turn(
            provider.as_ref(),
            &state.store,
            &tool_ctx,
            &sid,
            &model,
            Some(system_prompt.as_str()),
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

/// Current LLM provider settings for the settings panel.
#[tauri::command]
fn get_provider_config(state: State<'_, Arc<AppState>>) -> ProviderConfig {
    state.provider_config()
}

/// Persist new provider settings. Returns the stored config so the UI can confirm
/// the applied state (e.g. `has_key`, which the frontend never sets itself).
#[tauri::command]
fn set_provider_config(
    state: State<'_, Arc<AppState>>,
    kind: ProviderKind,
    base_url: Option<String>,
    model: String,
) -> ProviderConfig {
    let current = state.provider_config();
    let config = ProviderConfig {
        kind,
        // Treat an empty string from the UI the same as "use the default endpoint".
        base_url: base_url.filter(|u| !u.trim().is_empty()),
        model,
        // Secrets are a later phase; preserve whatever the backend already knows.
        has_key: current.has_key,
    };
    state.set_provider_config(config.clone());
    config
}

/// Best-effort model list for the configured provider's endpoint. Returns an empty
/// list (never an error) when the server is unreachable so the picker degrades to
/// free-text entry.
#[tauri::command]
async fn list_models(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<String>> {
    let (provider, _model) = state.build_provider();
    Ok(provider.list_models().await.unwrap_or_default())
}

/// Wake the configured model server so its GPU/compute pipelines are hot before
/// the user's first message. The composer fires this (debounced) when it gains
/// focus. Fully best-effort: any failure (server down, busy) is swallowed so
/// warmup never blocks the UI or surfaces an error.
#[tauri::command]
async fn warmup(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    let (provider, model) = state.build_provider();
    let _ = provider.warmup(&model).await;
    Ok(())
}

/// Install a skill from a path / git URL / raw-Markdown URL. The bundle is fetched
/// and validated first (a bad bundle returns an error and never prompts), then the
/// user approves the real declared manifest via the shared approval gate, and only
/// on approval is it moved into `~/.flowforge/skills/`. Returns the installed
/// manifest. Errors (validation, denial, IO) come back as `Err(String)`.
#[tauri::command]
async fn install_skill(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    source: String,
) -> CmdResult<SkillManifest> {
    // Fetch + validate off the async runtime; a bad bundle fails here, pre-approval.
    let prep_source = source.clone();
    let staged = tokio::task::spawn_blocking(move || ff_skills::prepare_install(&prep_source))
        .await
        .map_err(|e| format!("install task failed: {e}"))?
        .map_err(|e| e.to_string())?;

    let manifest = staged.manifest().clone();
    let request_id = Uuid::new_v4().to_string();

    // Present the validated manifest for approval, reusing the dangerous-tool gate.
    let rx = state.register_approval(&request_id, "");
    let _ = app.emit(
        "skill:install-approval-request",
        SkillInstallApprovalRequestEvent {
            request_id: request_id.clone(),
            source,
            manifest: manifest.clone(),
            warnings: Vec::new(),
        },
    );
    if !rx.await.unwrap_or(false) {
        return Err("install was not approved".to_string());
    }

    let skills_root = state.skills_root();
    tokio::task::spawn_blocking(move || ff_skills::commit_install(staged, &skills_root))
        .await
        .map_err(|e| format!("install task failed: {e}"))?
        .map_err(|e| e.to_string())?;

    state.reload_skills();
    Ok(manifest)
}

/// Uninstall a skill by manifest name. Local and reversible, so it is not gated.
#[tauri::command]
fn uninstall_skill(state: State<'_, Arc<AppState>>, name: String) -> CmdResult<()> {
    ff_skills::uninstall(&name, &state.skills_root()).map_err(|e| e.to_string())?;
    state.reload_skills();
    Ok(())
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
            rename_session,
            send_message,
            cancel_turn,
            respond_approval,
            get_provider_config,
            set_provider_config,
            list_models,
            warmup,
            install_skill,
            uninstall_skill,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
