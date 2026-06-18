//! Thin Tauri shell. Per the SOP, this layer contains only command/event glue —
//! all business logic lives in the `ff-*` crates. Each handler deserializes,
//! calls into a crate, and returns. Streaming responses go out as Tauri events.

mod optimize;
mod state;
mod tools;
mod web_search;

use async_trait::async_trait;
use ff_agent::{run_turn, AgentEvent, Approver, CancelToken, ToolContext};
use ff_core::events::{
    ApprovalSafety, EvolveCostEstimate, IntentionSignal, McpStatusChangedEvent, ReasoningEvent,
    SkillActivated, SkillCompleted, SkillEvolveApprovalRequestEvent,
    SkillInstallApprovalRequestEvent, SkillsChangedEvent, TokenEvent, ToolApprovalRequestEvent,
    ToolAskRequestEvent, ToolCallEvent, ToolResultEvent, TurnDoneEvent, TurnErrorEvent,
};
use ff_core::{
    McpServerConfig, McpServerStatus, MemoryFileInfo, MemoryFileKind, MemoryOverview, Message,
    Phenotype, ProviderConfig, ProviderConnection, ProviderKind, ProviderRegistry, Role,
    SearchConfig, Session, Skill, SkillInfo, SkillManifest,
};
use ff_signals::SkillAggregate;
use ff_tools::Safety;
use state::AppState;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};
use uuid::Uuid;

/// Per-turn telemetry accumulator (RFC 0001 §8), filled by the agent-event closure
/// and folded into per-skill aggregates when the turn ends. `message_ids` counts
/// distinct assistant messages — one per agent loop iteration, i.e. the turn count;
/// `chars` is the total streamed assistant text used as a coarse token-cost proxy.
#[derive(Default)]
struct TurnMetrics {
    chars: usize,
    message_ids: std::collections::HashSet<String>,
}

impl TurnMetrics {
    fn note_turn(&mut self, message_id: &str) {
        self.message_ids.insert(message_id.to_string());
    }

    /// `(streamed assistant chars, distinct turn count)`.
    fn snapshot(&self) -> (usize, usize) {
        (self.chars, self.message_ids.len())
    }
}

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
        let safety = match safety {
            Safety::Write => ApprovalSafety::Write,
            Safety::Dangerous => ApprovalSafety::Dangerous,
            // The agent loop short-circuits ReadOnly before calling the approver,
            // so it never reaches the approval contract.
            // The agent loop short-circuits ReadOnly before approval; deny
            // defensively rather than panic if a future caller violates that.
            Safety::ReadOnly => return false,
        };
        let rx = self.state.register_approval(&self.session_id, call_id);
        let _ = self.app.emit(
            "tool:approval-request",
            ToolApprovalRequestEvent {
                session_id: self.session_id.clone(),
                message_id: message_id.to_string(),
                call_id: call_id.to_string(),
                tool: name.to_string(),
                args: args.clone(),
                safety,
            },
        );
        // Sender dropped (cancel) -> RecvError -> deny.
        rx.await.unwrap_or(false)
    }

    async fn ask(
        &self,
        message_id: &str,
        call_id: &str,
        args: &serde_json::Value,
    ) -> Option<String> {
        // The loop forwards the tool args; the host reads the `question` field.
        let question = args
            .get("question")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let rx = self.state.register_ask(&self.session_id, call_id);
        let _ = self.app.emit(
            "tool:ask-request",
            ToolAskRequestEvent {
                session_id: self.session_id.clone(),
                message_id: message_id.to_string(),
                call_id: call_id.to_string(),
                question,
            },
        );
        // Sender dropped (cancel/teardown) -> RecvError -> dismissed (None).
        rx.await.ok()
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

/// Permanently remove a session and its transcript. Cancels any in-flight turn and
/// pending approvals first, so a stream cannot write back to a deleted session.
#[tauri::command]
fn delete_session(state: State<'_, Arc<AppState>>, session_id: String) {
    if let Some(token) = state.take_cancel(&session_id) {
        token.cancel();
    }
    state.cancel_pending_approvals(&session_id);
    state.store.delete_session(&session_id);
}

/// Clone a session and its transcript into a fresh session (server-truth).
/// Backs the sidebar/split Duplicate action.
#[tauri::command]
fn fork_session(state: State<'_, Arc<AppState>>, session_id: String) -> Result<Session, String> {
    state
        .store
        .fork_session(&session_id)
        .ok_or_else(|| format!("unknown session: {session_id}"))
}

/// Map an internal [`ff_memory::MemoryFile`] to the IPC [`MemoryFileInfo`] contract.
fn to_file_info(f: ff_memory::MemoryFile) -> MemoryFileInfo {
    MemoryFileInfo {
        name: f.name,
        rel_path: f.rel_path,
        kind: match f.kind {
            ff_memory::MemoryFileKind::Curated => MemoryFileKind::Curated,
            ff_memory::MemoryFileKind::Daily => MemoryFileKind::Daily,
        },
        size_bytes: f.size_bytes as i64,
        modified_ms: f.modified_ms,
    }
}

/// List the curated + daily memory files for the Settings memory pane (Issue #131).
/// Read-only: curated first, then daily newest-first.
#[tauri::command]
fn list_memory_files(state: State<'_, Arc<AppState>>) -> Vec<MemoryFileInfo> {
    state
        .memory()
        .list_files()
        .into_iter()
        .map(to_file_info)
        .collect()
}

/// Read one memory file's body by its root-relative path (from `list_memory_files`).
/// Errors if the path escapes the memory root.
#[tauri::command]
fn read_memory_file(state: State<'_, Arc<AppState>>, rel_path: String) -> Result<String, String> {
    state
        .memory()
        .read_file(&rel_path)
        .ok_or_else(|| "invalid memory path".to_string())
}

/// Summarize the memory store (file/byte counts, root, enabled flag) for the
/// Settings pane header. Read-only — the enable toggle lands with Issue #166.
#[tauri::command]
fn memory_overview(state: State<'_, Arc<AppState>>) -> MemoryOverview {
    let mem = state.memory();
    let files = mem.list_files();
    let total_bytes: i64 = files.iter().map(|f| f.size_bytes as i64).sum();
    MemoryOverview {
        enabled: mem.is_enabled(),
        file_count: files.len() as i64,
        total_bytes,
        root_path: mem.root().display().to_string(),
    }
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
fn respond_approval(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    call_id: String,
    approved: bool,
) {
    state.resolve_approval(&session_id, &call_id, approved);
}

/// Frontend response to a [`ToolAskRequestEvent`] (#44): the user's answer to an
/// `ask_user` question. Wakes the awaiting tool call, which resumes the turn.
#[tauri::command]
fn respond_ask(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    call_id: String,
    answer: String,
) {
    state.resolve_ask(&session_id, &call_id, answer);
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
    // A clone kept by the host so the post-turn telemetry can tell a clean finish
    // from a user cancel (the original is moved into `run_turn`).
    let cancel_probe = cancel.clone();

    let state = state.inner().clone();
    // Snapshot the provider from the current config for this turn; a settings
    // change between turns is picked up on the next `send_message`.
    let (provider, default_model) = state.build_provider();
    // The active phenotype may override the model and prepend a persona (RFC 0001 §7).
    let model = state.active_model_override().unwrap_or(default_model);
    let persona = state.active_persona();
    tauri::async_runtime::spawn(async move {
        let sid = session_id.clone();
        let approver = UiApprover {
            app: app.clone(),
            state: state.clone(),
            session_id: sid.clone(),
        };
        // Snapshot built-in + MCP-bridged tools for this turn (RFC 0003 §6).
        let registry = state.build_tool_registry();
        let tool_ctx = ToolContext {
            registry: &registry,
            root: &state.workspace_root,
            approve: &approver,
            max_iterations: 8,
        };
        // Skills + ambient context for this turn (RFC 0001 §4, RFC 0002 phase 1):
        // the active phenotype's persona, installed-skill descriptions, the bodies of
        // active skills, and the current local time.
        let skills = state.skills_snapshot();
        let user_ctx = ff_agent::UserContext::now();
        let active = state.active_skills();
        let memory = state.memory().ambient_block();
        let system_prompt = ff_agent::build_system_prompt(
            persona.as_deref(),
            &skills,
            &active,
            &user_ctx,
            memory.as_deref(),
        );

        // Telemetry (RFC 0001 §8): one SkillActivated per active skill, plus a
        // wall-clock start and a per-turn metrics accumulator the event closure
        // folds into. `turns` = distinct assistant message ids (one per loop
        // iteration); `tokens` = a coarse char/4 proxy over streamed assistant text.
        for skill in &active {
            state.record_skill_activated(skill);
            let _ = app.emit(
                "skill:activated",
                SkillActivated {
                    skill: skill.clone(),
                    session_id: sid.clone(),
                },
            );
        }
        let turn_start = std::time::Instant::now();
        let metrics = std::sync::Arc::new(std::sync::Mutex::new(TurnMetrics::default()));
        let metrics_for_events = metrics.clone();

        let thinking = state.provider_config().thinking;
        let result = run_turn(
            provider.as_ref(),
            &state.store,
            &tool_ctx,
            &sid,
            &model,
            Some(system_prompt.as_str()),
            thinking,
            cancel,
            |event| match event {
                AgentEvent::Token { message_id, delta } => {
                    if let Ok(mut m) = metrics_for_events.lock() {
                        m.note_turn(&message_id);
                        m.chars += delta.chars().count();
                    }
                    let _ = app.emit(
                        "turn:token",
                        TokenEvent {
                            session_id: sid.clone(),
                            message_id,
                            delta,
                        },
                    );
                }
                AgentEvent::Reasoning { message_id, delta } => {
                    if let Ok(mut m) = metrics_for_events.lock() {
                        m.note_turn(&message_id);
                    }
                    let _ = app.emit(
                        "turn:reasoning",
                        ReasoningEvent {
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
                    if let Ok(mut m) = metrics_for_events.lock() {
                        m.note_turn(&message_id);
                    }
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
                AgentEvent::Done { message_id, .. } => {
                    if let Ok(mut m) = metrics_for_events.lock() {
                        m.note_turn(&message_id);
                    }
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

        if let Err(ref e) = result {
            let _ = app.emit(
                "turn:error",
                TurnErrorEvent {
                    session_id: session_id.clone(),
                    message: e.to_string(),
                },
            );
        }

        // Telemetry (RFC 0001 §8): fold this turn's metrics into each active skill's
        // aggregate and emit a SkillCompleted per skill. Success = a clean finish
        // (run_turn returned Ok and the turn was not cancelled).
        let m = metrics.lock().map(|m| m.snapshot()).unwrap_or_default();
        let success = result.is_ok() && !cancel_probe.is_cancelled();
        let latency_ms = u32::try_from(turn_start.elapsed().as_millis()).unwrap_or(u32::MAX);
        let tokens = u32::try_from(m.0 / 4).unwrap_or(u32::MAX);
        let turns = u32::try_from(m.1).unwrap_or(u32::MAX);
        for skill in &active {
            let ev = SkillCompleted {
                skill: skill.clone(),
                session_id: sid.clone(),
                tokens,
                latency_ms,
                turns,
                success,
            };
            state.record_skill_completed(&ev);
            let _ = app.emit("skill:completed", ev);
        }
        // Persist the turn's telemetry once, lock-free (addresses #77 nit 1).
        state.persist_signals();

        // Drop the session's cancel token *before* the flush. The token is keyed
        // by session_id alone, so leaving it registered across the (potentially
        // multi-second, multi-round-trip) silent flush lets the next turn's
        // register_cancel overwrite it — then this task's take_cancel would remove
        // the *new* turn's token, silently disabling its Stop button and
        // auto-denying all of its tool approvals. The flush runs on cancel_probe,
        // the task-local clone it already owns, so it stays bounded and silent.
        state.take_cancel(&session_id);

        // Pre-compaction memory flush (RFC 0006 §7.2): once the visible turn has
        // finished cleanly, persist any durable facts before context pressure forces
        // a summarization that would drop them. Silent — never adds to the transcript.
        if success {
            state
                .maybe_flush_memory(provider.as_ref(), &registry, &sid, &model, cancel_probe)
                .await;
        }
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
    thinking: bool,
) -> ProviderConfig {
    let current = state.provider_config();
    let config = ProviderConfig {
        kind,
        // Treat an empty string from the UI the same as "use the default endpoint".
        base_url: base_url.filter(|u| !u.trim().is_empty()),
        model,
        // Secrets are a later phase; preserve whatever the backend already knows.
        has_key: current.has_key,
        thinking,
    };
    state.set_provider_config(config.clone());
    config
}

/// The full provider connection registry for the settings panel (RFC 0005 Phase A).
#[tauri::command]
fn get_provider_registry(state: State<'_, Arc<AppState>>) -> ProviderRegistry {
    state.provider_registry()
}

/// Select the active connection by id. Errors on an unknown id.
#[tauri::command]
fn set_active_connection(state: State<'_, Arc<AppState>>, id: String) -> CmdResult<()> {
    state.set_active_connection(&id)
}

/// Add or update a connection (keyed by `id`, derived from vendor/name when blank).
/// Returns the stored connection so the UI sees the resolved id.
#[tauri::command]
fn upsert_connection(
    state: State<'_, Arc<AppState>>,
    conn: ProviderConnection,
) -> CmdResult<ProviderConnection> {
    Ok(state.upsert_connection(conn))
}

/// Remove a connection by id. Errors when removing the last one.
#[tauri::command]
fn remove_connection(state: State<'_, Arc<AppState>>, id: String) -> CmdResult<()> {
    state.remove_connection(&id)
}

/// Current persisted web-search settings.
#[tauri::command]
fn get_search_config(state: State<'_, Arc<AppState>>) -> SearchConfig {
    state.search_config()
}

/// Persist new web-search settings. Returns the stored config so the UI can confirm
/// the applied state (e.g. `hasKey`, which the frontend never sets itself).
#[tauri::command]
fn set_search_config(
    state: State<'_, Arc<AppState>>,
    backend: ff_core::SearchBackend,
    base_url: Option<String>,
) -> SearchConfig {
    let current = state.search_config();
    let config = SearchConfig {
        backend,
        // Treat an empty string from the UI the same as "no endpoint configured".
        base_url: base_url.filter(|u| !u.trim().is_empty()),
        // Secrets are a later phase; preserve whatever the backend already knows.
        has_key: current.has_key,
    };
    state.set_search_config(config.clone());
    config
}

/// Best-effort model list for a connection's endpoint (`id` defaults to the active
/// connection). Returns an empty list (never an error) when the server is
/// unreachable so the picker degrades to free-text entry.
#[tauri::command]
async fn list_models(
    state: State<'_, Arc<AppState>>,
    id: Option<String>,
) -> CmdResult<Vec<String>> {
    let (provider, _model) = state.build_provider_for(id.as_deref());
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
    // An install is a live, standalone approval (no turn), so register a liveness
    // token under `request_id` to satisfy `register_approval`'s TOCTOU guard, and
    // key the prompt (request_id, request_id). The token is released after the
    // await — and lets a shutdown/`cancel_pending_approvals` deny a stuck install.
    state.register_cancel(&request_id, CancelToken::new());
    let rx = state.register_approval(&request_id, &request_id);
    let _ = app.emit(
        "skill:install-approval-request",
        SkillInstallApprovalRequestEvent {
            request_id: request_id.clone(),
            source,
            manifest: manifest.clone(),
            warnings: Vec::new(),
        },
    );
    let approved = rx.await.unwrap_or(false);
    state.take_cancel(&request_id);
    if !approved {
        return Err("install was not approved".to_string());
    }

    let skills_root = state.skills_root();
    tokio::task::spawn_blocking(move || ff_skills::commit_install(staged, &skills_root))
        .await
        .map_err(|e| format!("install task failed: {e}"))?
        .map_err(|e| e.to_string())?;

    state.reload_skills();
    emit_skills_changed(&app, &state);
    Ok(manifest)
}

/// Build a frontend [`SkillInfo`] from a loaded skill, folding in active state and
/// a search score (`0` for the unranked list).
fn to_skill_info(skill: &Skill, active: &[String], score: u32) -> SkillInfo {
    SkillInfo {
        name: skill.manifest.name.clone(),
        description: skill.manifest.description.clone(),
        version: skill.manifest.version.clone(),
        keywords: skill.manifest.keywords.clone(),
        active: active.contains(&skill.manifest.name),
        score,
    }
}

/// Emit the current active set so the frontend can reconcile its state.
fn emit_skills_changed(app: &tauri::AppHandle, state: &AppState) {
    let _ = app.emit(
        "skills:changed",
        SkillsChangedEvent {
            active: state.active_skills(),
        },
    );
}

/// All installed skills, name-sorted, each flagged with its active state. `score`
/// is always `0` here — ranking is `search_skills`' job. Backs the command palette
/// skill source (#11/#16).
#[tauri::command]
fn list_skills(state: State<'_, Arc<AppState>>) -> Vec<SkillInfo> {
    let active = state.active_skills();
    let reg = state.skills_snapshot();
    let mut skills: Vec<&Skill> = reg.list().collect();
    skills.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    skills
        .into_iter()
        .map(|s| to_skill_info(s, &active, 0))
        .collect()
}

/// Rank installed skills for `query`, sharing `ff_skills::search_skills` with the
/// agent tool of the same name. An empty query lists all skills (name-sorted).
#[tauri::command]
fn search_skills(state: State<'_, Arc<AppState>>, query: String) -> Vec<SkillInfo> {
    let active = state.active_skills();
    let reg = state.skills_snapshot();
    ff_skills::search_skills(&reg, &query)
        .into_iter()
        .map(|hit| SkillInfo {
            name: hit.manifest.name.clone(),
            description: hit.manifest.description.clone(),
            version: hit.manifest.version.clone(),
            keywords: hit.manifest.keywords.clone(),
            active: active.contains(&hit.manifest.name),
            score: hit.score,
        })
        .collect()
}

/// Per-skill telemetry aggregate (RFC 0001 §8): activation/completion counts, mean
/// token cost, mean turns, mean latency, and success rate. `None` when the skill has
/// no recorded signals yet. Backs the optimize flow's before/after cost estimate.
#[tauri::command]
fn get_skill_telemetry(state: State<'_, Arc<AppState>>, skill: String) -> Option<SkillAggregate> {
    state.skill_telemetry(&skill)
}

/// A compact recent-transcript sample for an optimize prompt: the last `n` messages
/// of the session, each as `role: content` with the body truncated. Current-session
/// only for M3 (durable cross-session transcripts are deferred to M5).
fn recent_transcript(state: &AppState, session_id: &str, n: usize) -> Vec<String> {
    let messages = state.store.get_messages(session_id);
    let start = messages.len().saturating_sub(n);
    messages[start..]
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
                Role::Tool => "tool",
            };
            let mut body = m.content.replace('\n', " ");
            if body.chars().count() > 200 {
                body = body.chars().take(200).collect::<String>() + "…";
            }
            format!("{role}: {body}")
        })
        .collect()
}

/// Manual skill optimize/evolve (M3.5, RFC 0001 §8). Gathers the skill body, its
/// telemetry aggregate, and a recent-transcript sample; asks the model for a
/// streamlined rewrite; then presents a before->after proposal with a cost estimate
/// for approval. On approval the skill is version-bumped (previous version retained
/// for rollback); on rejection nothing changes. Reuses the standalone-approval gate
/// (keyed by `request_id`, answered via `respond_approval`), exactly like install.
#[tauri::command]
async fn optimize_skill(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    session_id: String,
    skill: String,
) -> CmdResult<String> {
    // Snapshot the current body + version (reject an unknown skill before any model
    // call), plus telemetry and a short transcript sample for context.
    let (before_body, current_version) = {
        let reg = state.skills_snapshot();
        let s = reg
            .get(&skill)
            .ok_or_else(|| format!("unknown skill: {skill}"))?;
        (s.body.clone(), s.manifest.version.clone())
    };
    let aggregate = state.skill_telemetry(&skill);
    let transcript = recent_transcript(&state, &session_id, 6);

    let (provider, default_model) = state.build_provider();
    let model = state.active_model_override().unwrap_or(default_model);
    let after_body = optimize::propose_rewrite(
        provider.as_ref(),
        &model,
        &skill,
        &before_body,
        aggregate.as_ref(),
        &transcript,
    )
    .await?;

    let new_version = ff_skills::bump_patch(&current_version);
    let (current_mean_tokens, estimated_mean_tokens) =
        optimize::estimate_cost(aggregate.as_ref(), &before_body, &after_body);

    // Standalone approval, same pattern as install_skill.
    let request_id = Uuid::new_v4().to_string();
    state.register_cancel(&request_id, CancelToken::new());
    let rx = state.register_approval(&request_id, &request_id);
    let _ = app.emit(
        "skill:evolve-approval-request",
        SkillEvolveApprovalRequestEvent {
            request_id: request_id.clone(),
            skill: skill.clone(),
            current_version,
            new_version,
            before_body,
            after_body: after_body.clone(),
            cost_estimate: EvolveCostEstimate {
                current_mean_tokens,
                estimated_mean_tokens,
            },
        },
    );
    let approved = rx.await.unwrap_or(false);
    state.take_cancel(&request_id);
    if !approved {
        return Err("optimize was not approved".to_string());
    }

    let skills_root = state.skills_root();
    let history_root = state.skill_history_root();
    let skill_name = skill.clone();
    let applied = tokio::task::spawn_blocking(move || {
        ff_skills::bump_skill(&skills_root, &history_root, &skill_name, &after_body)
    })
    .await
    .map_err(|e| format!("optimize task failed: {e}"))?
    .map_err(|e| e.to_string())?;

    state.reload_skills();
    emit_skills_changed(&app, &state);
    Ok(applied)
}

/// Restore a retained previous version of a skill as the live version (RFC 0001 §8).
/// The current live version is archived first, so a rollback is itself reversible.
/// Emits `skills:changed`.
#[tauri::command]
async fn rollback_skill(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    skill: String,
    version: String,
) -> CmdResult<()> {
    let skills_root = state.skills_root();
    let history_root = state.skill_history_root();
    tokio::task::spawn_blocking(move || {
        ff_skills::rollback_skill(&skills_root, &history_root, &skill, &version)
    })
    .await
    .map_err(|e| format!("rollback task failed: {e}"))?
    .map_err(|e| e.to_string())?;
    state.reload_skills();
    emit_skills_changed(&app, &state);
    Ok(())
}

/// Retained version names for a skill, newest-looking first. Empty when the skill
/// has never been optimized (RFC 0001 §8). Backs the rollback picker.
#[tauri::command]
fn list_skill_versions(state: State<'_, Arc<AppState>>, skill: String) -> CmdResult<Vec<String>> {
    ff_skills::list_skill_versions(&state.skill_history_root(), &skill).map_err(|e| e.to_string())
}

/// Add a skill to the global active set (its body is injected next turn). Errors on
/// an unknown name. Emits `skills:changed`.
#[tauri::command]
fn activate_skill(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    name: String,
) -> CmdResult<()> {
    state.activate_skill(&name)?;
    emit_skills_changed(&app, &state);
    Ok(())
}

/// Remove a skill from the active set (idempotent). Emits `skills:changed`.
#[tauri::command]
fn deactivate_skill(state: State<'_, Arc<AppState>>, app: tauri::AppHandle, name: String) {
    state.deactivate_skill(&name);
    emit_skills_changed(&app, &state);
}

/// Uninstall a skill by manifest name. Local and reversible, so it is not gated.
#[tauri::command]
fn uninstall_skill(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    name: String,
) -> CmdResult<()> {
    ff_skills::uninstall(&name, &state.skills_root()).map_err(|e| e.to_string())?;
    state.reload_skills();
    // An uninstalled skill must not linger in the active set.
    state.prune_active_skills();
    emit_skills_changed(&app, &state);
    Ok(())
}

/// All selectable phenotypes (built-in `default` + `~/.flowforge/phenotypes/`),
/// name-sorted. Backs the `pheno` command palette.
#[tauri::command]
fn list_phenotypes(state: State<'_, Arc<AppState>>) -> Vec<Phenotype> {
    state.list_phenotypes()
}

/// The active phenotype.
#[tauri::command]
fn get_phenotype(state: State<'_, Arc<AppState>>) -> Phenotype {
    state.active_phenotype()
}

/// Switch the active phenotype: replaces the active-skill set with the phenotype's
/// skills and persists the choice across restarts (RFC 0001 §7). Errors on an unknown
/// name. Emits `skills:changed` so the FE active set updates. Returns the phenotype now
/// active.
#[tauri::command]
fn switch_phenotype(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    name: String,
) -> CmdResult<Phenotype> {
    let pheno = state.switch_phenotype(&name)?;
    emit_skills_changed(&app, &state);
    Ok(pheno)
}

// ---- MCP server control (M4.4, RFC 0003 §3,5) ----
//
// Enable/disable/add/remove write `mcp.json` via `ff_mcp::config`; the existing config
// watcher then reloads and reconciles the supervisor, so these commands deliberately
// do NOT touch the supervisor directly. Only `restart_mcp_server` drives the actor (an
// immediate restart has no config delta for the watcher to observe). Status changes are
// pushed to the FE via the `mcp:status-changed` forwarder wired in `run`.

/// Current status snapshot of every configured MCP server. Empty (not an error) when
/// the MCP host never started (no `mcp.json`, no home dir).
#[tauri::command]
fn list_mcp_servers(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<McpServerStatus>> {
    Ok(state
        .mcp_handle()
        .map(|h| h.status_snapshot())
        .unwrap_or_default())
}

/// Drive an immediate restart of one server, bypassing backoff and reviving a `Failed`
/// server. Unknown ids are a no-op inside the supervisor.
#[tauri::command]
async fn restart_mcp_server(state: State<'_, Arc<AppState>>, id: String) -> CmdResult<()> {
    let handle = state
        .mcp_handle()
        .ok_or_else(|| "mcp host is not running".to_string())?;
    handle.restart(&id).await;
    Ok(())
}

/// Enable or disable one server by flipping `disabled` in `mcp.json`. The config watcher
/// reconciles the supervisor; the resulting status change arrives via `mcp:status-changed`.
#[tauri::command]
fn set_mcp_server_enabled(
    state: State<'_, Arc<AppState>>,
    id: String,
    enabled: bool,
) -> CmdResult<()> {
    let path = state
        .mcp_config_path()
        .ok_or_else(|| "mcp host is not running".to_string())?;
    ff_mcp::set_disabled(&path, &id, !enabled).map_err(|e| e.to_string())
}

/// Add (or replace) a server definition in `mcp.json`. `def` is **raw user input** from
/// the add form, never a `load()`-resolved config, so its `env` holds literal strings or
/// `${env:}` templates — mapping it to the un-resolved `McpServerInput` keeps a resolved
/// secret from ever being written back (enforced by the distinct type).
#[tauri::command]
fn add_mcp_server(state: State<'_, Arc<AppState>>, def: McpServerConfig) -> CmdResult<()> {
    let path = state
        .mcp_config_path()
        .ok_or_else(|| "mcp host is not running".to_string())?;
    let input = ff_mcp::McpServerInput {
        id: def.id,
        command: def.command,
        args: def.args,
        env: def.env,
        disabled: def.disabled,
    };
    ff_mcp::upsert(&path, &input).map_err(|e| e.to_string())
}

/// Remove a server definition from `mcp.json`. Idempotent: absent id is a no-op.
#[tauri::command]
fn remove_mcp_server(state: State<'_, Arc<AppState>>, id: String) -> CmdResult<()> {
    let path = state
        .mcp_config_path()
        .ok_or_else(|| "mcp host is not running".to_string())?;
    ff_mcp::remove(&path, &id).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let state = Arc::new(AppState::new());
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(state.clone())
        .setup(move |app| {
            // `init_mcp` enters the shared Tokio runtime itself, so it's safe to
            // call here even though Tauri's `setup` runs on the main thread outside
            // an entered reactor on macOS (issue #117).
            state.init_mcp();
            // Forward supervisor status changes to the FE as `mcp:status-changed`, so
            // the servers panel reflects start/stop/restart/health without polling. The
            // watch tick coalesces; we re-snapshot on each wake. Runs on Tauri's managed
            // runtime, so it needs no entered reactor here.
            if let Some(handle) = state.mcp_handle() {
                let app_handle = app.handle().clone();
                let mut rx = handle.status_changed_rx();
                tauri::async_runtime::spawn(async move {
                    while rx.changed().await.is_ok() {
                        let servers = handle.status_snapshot();
                        let _ = app_handle
                            .emit("mcp:status-changed", McpStatusChangedEvent { servers });
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            create_session,
            list_sessions,
            get_messages,
            rename_session,
            delete_session,
            fork_session,
            list_memory_files,
            read_memory_file,
            memory_overview,
            send_message,
            cancel_turn,
            respond_approval,
            respond_ask,
            get_provider_config,
            set_provider_config,
            get_provider_registry,
            set_active_connection,
            upsert_connection,
            remove_connection,
            get_search_config,
            set_search_config,
            list_models,
            warmup,
            install_skill,
            uninstall_skill,
            list_skills,
            search_skills,
            get_skill_telemetry,
            optimize_skill,
            rollback_skill,
            list_skill_versions,
            activate_skill,
            deactivate_skill,
            list_phenotypes,
            get_phenotype,
            switch_phenotype,
            list_mcp_servers,
            restart_mcp_server,
            set_mcp_server_enabled,
            add_mcp_server,
            remove_mcp_server,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");
    app.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event {
            // Stop every MCP child cleanly while the runtime is still alive — drop
            // alone reaps the child via `process_wrap`, but its background task needs
            // a live Tokio runtime, which exits with the app (RFC 0003 §5).
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                if let Some(handle) = state.mcp_handle() {
                    tauri::async_runtime::block_on(handle.stop_all());
                }
            }
        }
    });
}
