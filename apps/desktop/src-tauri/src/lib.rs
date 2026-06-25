//! Thin Tauri shell. Per the SOP, this layer contains only command/event glue —
//! all business logic lives in the `ff-*` crates. Each handler deserializes,
//! calls into a crate, and returns. Streaming responses go out as Tauri events.

mod optimize;
mod secrets;
mod state;
mod tools;

use async_trait::async_trait;
use ff_agent::{run_turn, AgentEvent, Approver, CancelToken, ToolContext};
use ff_core::events::{
    ApprovalSafety, EvolveCostEstimate, IntentionSignal, McpStatusChangedEvent, MemoryFlushedEvent,
    PhenotypeMcpUnavailableEvent, ReasoningEvent, SkillActivated, SkillCompleted,
    SkillEvolveApprovalRequestEvent, SkillInstallApprovalRequestEvent, SkillsChangedEvent,
    TokenEvent, ToolApprovalRequestEvent, ToolAskRequestEvent, ToolCallEvent, ToolResultEvent,
    TurnDoneEvent, TurnErrorEvent, TurnStatsEvent,
};
use ff_core::{
    Attachment, BedrockAuth, Format, McpServerConfig, McpServerStatus, MemoryFileInfo,
    MemoryFileKind, MemoryOverview, Message, Mode, Phenotype, ProviderConfig, ProviderConnection,
    ProviderKind, ProviderRegistry, Role, SearchConfig, SecretKind, Session, SessionWorkspace,
    Skill, SkillInfo, SkillManifest,
};
use ff_signals::SkillAggregate;
use ff_tools::Safety;
use state::AppState;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};
use tauri_plugin_shell::ShellExt;
use uuid::Uuid;

/// Per-turn telemetry accumulator (RFC 0001 §8), filled by the agent-event closure
/// and folded into per-skill aggregates when the turn ends. `message_ids` counts
/// distinct assistant messages — one per agent loop iteration, i.e. the turn count;
/// `chars` is the total streamed assistant text used as a coarse token-cost proxy.
#[derive(Default)]
struct TurnMetrics {
    chars: usize,
    message_ids: std::collections::HashSet<String>,
    /// First-seen instant of each distinct assistant message, in arrival order.
    /// One per agent loop iteration (provider round-trip); consecutive deltas
    /// give the per-iteration wall-clock baseline (F1, #427).
    iter_marks: Vec<std::time::Instant>,
    /// Silent mid-turn memory flushes, each an extra provider round-trip (#427).
    flushes: u32,
    /// F1b (#441): captured from the turn's `Done` event -- the per-round-trip
    /// prefill estimate and how often each compaction tier engaged this turn.
    prefill_estimates: Vec<u32>,
    tier1_fires: u32,
    tier2_fires: u32,
}

impl TurnMetrics {
    fn note_turn(&mut self, message_id: &str) {
        if self.message_ids.insert(message_id.to_string()) {
            self.iter_marks.push(std::time::Instant::now());
        }
    }

    fn note_flush(&mut self) {
        self.flushes += 1;
    }

    /// Fold the agent-side F1b signal carried by the turn's `Done` event (#441).
    /// Fires once per turn, so a plain assign is correct.
    fn note_done(&mut self, prefill_estimates: &[u32], tier1_fires: u32, tier2_fires: u32) {
        self.prefill_estimates = prefill_estimates.to_vec();
        self.tier1_fires = tier1_fires;
        self.tier2_fires = tier2_fires;
    }

    /// `(streamed assistant chars, distinct turn count)`.
    fn snapshot(&self) -> (usize, usize) {
        (self.chars, self.message_ids.len())
    }

    /// Per-turn timing breakdown for the #427 baseline: `(round_trips, per-iteration
    /// ms in arrival order, flushes)`. `turn_end` closes the final iteration.
    fn timing(&self, turn_end: std::time::Instant) -> (u32, Vec<u32>, u32) {
        let iter_ms = self
            .iter_marks
            .iter()
            .enumerate()
            .map(|(i, start)| {
                let end = self.iter_marks.get(i + 1).copied().unwrap_or(turn_end);
                u32::try_from(end.saturating_duration_since(*start).as_millis()).unwrap_or(u32::MAX)
            })
            .collect();
        let round_trips = u32::try_from(self.iter_marks.len()).unwrap_or(u32::MAX);
        (round_trips, iter_ms, self.flushes)
    }
}

/// Routes write/dangerous tool calls through a UI confirmation. Read-only calls
/// never reach this approver — the agent loop short-circuits them.
/// Whether the active autonomy mode auto-approves a call of this safety without a
/// prompt. Only `Auto` + `Write` qualifies; `Dangerous` always prompts regardless
/// of mode, preserving the #232 invariant that arbitrary code (python, `rm`) needs
/// a deliberate yes. ReadOnly never reaches the approver.
fn mode_auto_approves(mode: Mode, safety: Safety) -> bool {
    mode == Mode::Auto && safety == Safety::Write
}

struct UiApprover {
    app: tauri::AppHandle,
    state: Arc<AppState>,
    session_id: String,
    /// The session's resolved autonomy mode for this turn (#265). In [`Mode::Auto`]
    /// a `Write` call is auto-approved; `Dangerous` always prompts regardless of mode.
    mode: Mode,
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
        // Short-circuit on the #229 allowlist (never covers Dangerous — see
        // `AppState::allowlist_covers`).
        if self.state.allowlist_covers(&self.session_id, name, safety) {
            return true;
        }

        // Auto mode (#265) auto-approves Write without a prompt; Dangerous always
        // falls through to the prompt below, so arbitrary code (python, `rm`) is
        // never silently run. ReadOnly never reaches here (the loop short-circuits).
        if mode_auto_approves(self.mode, safety) {
            return true;
        }

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

/// Export a session and its transcript as Markdown or JSON (#278). Returns the
/// rendered string for the frontend to save via a file dialog. Errors when the
/// session id is unknown so the UI can surface it rather than write an empty file.
#[tauri::command]
fn export_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    format: Format,
) -> Result<String, String> {
    state
        .store
        .export_session(&session_id, format)
        .ok_or_else(|| format!("session not found: {session_id}"))
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
    state.clear_session_approvals(&session_id);
    state.store.delete_session(&session_id);
    state.reap_session_processes(&session_id);
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

/// The working directory a session's tools run in (slice 3b, issue #200).
/// Returns the session's chosen workspace (or the global default when unset)
/// together with its current git branch when the cwd is a repo (#211).
#[tauri::command]
fn get_session_workspace(state: State<'_, Arc<AppState>>, session_id: String) -> SessionWorkspace {
    let root = state.session_root(&session_id);
    SessionWorkspace {
        git_branch: git_branch(&root),
        path: root.display().to_string(),
    }
}

/// The current git branch of `dir`, or `None` when it is not a git working tree
/// (no `.git/HEAD`) or is in detached HEAD. Reads `.git/HEAD` directly -- cheaper
/// than spawning `git` and dependency-free; `ref: refs/heads/<name>` yields the
/// branch, a bare commit SHA (detached) yields `None`. Extracted as a free fn so
/// it is unit-testable without a Tauri `State`.
fn git_branch(dir: &std::path::Path) -> Option<String> {
    let head = std::fs::read_to_string(dir.join(".git").join("HEAD")).ok()?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
}

/// Set a session's working directory. Validates that `path` exists and is a
/// directory (canonicalized so the stored root is absolute and symlink-resolved),
/// then returns the canonical path the UI should display. The chosen directory
/// becomes the `root` for that session's tools -- file tools are jailed to it and
/// `bash` runs in it.
#[tauri::command]
fn set_session_workspace(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    path: String,
) -> Result<String, String> {
    let canonical = resolve_workspace_dir(&path)?;
    let display = canonical.display().to_string();
    state.set_session_cwd(&session_id, canonical);
    Ok(display)
}

/// Canonicalize `path` and confirm it is an existing directory. Returns the
/// absolute, symlink-resolved path on success. Extracted from
/// [`set_session_workspace`] so the validation is unit-testable without a
/// Tauri `State`.
fn resolve_workspace_dir(path: &str) -> Result<std::path::PathBuf, String> {
    let canonical =
        std::fs::canonicalize(path).map_err(|e| format!("cannot resolve directory: {e}"))?;
    if !canonical.is_dir() {
        return Err(format!("not a directory: {}", canonical.display()));
    }
    Ok(canonical)
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

// --- Four-option tool approval commands (#229) ---

/// Approve a tool for this session only (in-memory).
#[tauri::command]
fn set_session_approve(state: State<'_, Arc<AppState>>, session_id: String, tool: String) {
    state.set_session_approve(&session_id, &tool);
}

/// Add a tool to the persistent always-approved set.
#[tauri::command]
fn set_always_approve(state: State<'_, Arc<AppState>>, tool: String) {
    state.set_always_approve(&tool);
}

/// Remove a tool from the persistent always-approved set.
#[tauri::command]
fn remove_always_approve(state: State<'_, Arc<AppState>>, tool: String) {
    state.remove_always_approve(&tool);
}

/// List all persistently always-approved tools (sorted).
#[tauri::command]
fn list_always_approved(state: State<'_, Arc<AppState>>) -> Vec<String> {
    state.list_always_approved()
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
    // Sent by the composer (#399); persisted on the user message so `run_turn`
    // forwards them to vision-capable providers. `None` for a plain text turn.
    attachments: Option<Vec<Attachment>>,
) -> CmdResult<String> {
    let user_msg =
        match attachments {
            Some(attachments) if !attachments.is_empty() => state
                .store
                .add_message_with_attachments(&session_id, Role::User, content, attachments),
            _ => state.store.add_message(&session_id, Role::User, content),
        };

    spawn_assistant_turn(state.inner().clone(), app, session_id);

    Ok(user_msg.id)
}

/// Set up and spawn the assistant turn for `session_id`: snapshots the provider,
/// resolves the session's phenotype/mode, builds the tool registry + system
/// prompt, runs the turn (streaming over `turn:*` / `tool:*`), and folds the
/// per-turn telemetry. Shared by `send_message` (after persisting the user turn)
/// and `edit_message` (after editing + truncating), so both paths run identical
/// turn semantics.
fn spawn_assistant_turn(state: Arc<AppState>, app: tauri::AppHandle, session_id: String) {
    let cancel = CancelToken::new();
    state.register_cancel(&session_id, cancel.clone());
    // A clone kept by the host so the post-turn telemetry can tell a clean finish
    // from a user cancel (the original is moved into `run_turn`).
    let cancel_probe = cancel.clone();

    // Snapshot the provider from the current config for this turn; a settings
    // change between turns is picked up on the next `send_message`.
    // Resolve THIS session's phenotype (#246): an explicit per-pane binding, else
    // the global active one. It supplies the persona, active skills, and the loop
    // iteration cap for the turn (RFC 0001 §7) — so two panes can run different Phenos.
    let pheno = state.session_phenotype(&session_id);
    // Resolve the (connection, model) for the turn via the three-tier precedence
    // session > phenotype > global (RFC 0005 §11.2) and build the provider for the
    // RESOLVED connection. This routes a phenotype model override through its intended
    // endpoint rather than the global active one (fixes RFC 0005 §11.1).
    let selection = state.resolve_model_selection(&session_id);
    let (provider, _) = state.build_provider_for(Some(&selection.connection));
    let model = selection.model;
    let persona = pheno.persona.clone();
    // Per-pane iteration cap (#244-R3 x #246): the resolved phenotype carries
    // `max_iterations`, so the bound Pheno governs the loop cap with no extra plumbing.
    let max_iterations = pheno
        .max_iterations
        .unwrap_or(ff_agent::DEFAULT_MAX_ITERATIONS);
    // Resolve THIS session's autonomy mode (#265): an explicit per-pane binding, else
    // the global default. Governs tool capability (Plan), the prompt steer, and the
    // approval policy for the turn.
    let mode = state.session_mode(&session_id);
    tauri::async_runtime::spawn(async move {
        let sid = session_id.clone();
        let approver = UiApprover {
            app: app.clone(),
            state: state.clone(),
            session_id: sid.clone(),
            mode,
        };
        // Snapshot built-in + MCP-bridged tools for this turn (RFC 0003 §6).
        let registry = state.build_tool_registry();
        let session_root = state.session_root(&sid);
        let mut tool_ctx = ToolContext::new(&registry, &session_root, &approver, max_iterations);
        tool_ctx.mode = mode;
        tool_ctx.abstractive = crate::state::abstractive_config_from_env();
        // Skills + ambient context for this turn (RFC 0001 §4, RFC 0002 phase 1):
        // the resolved persona, installed-skill descriptions, the bodies of the
        // active skills, and the current local time.
        let skills = state.skills_snapshot();
        let user_ctx = ff_agent::UserContext::now();
        // Active-skill source (#246): an explicit per-pane binding uses the
        // phenotype's declared skills; an unbound session keeps the global active
        // set so the command palette still affects turns. See `turn_active_skills`.
        let active: Vec<String> = state.turn_active_skills(&sid);
        let (memory, ambient_keys) = state
            .memory()
            .ambient_block_filtered_keyed(state.index().as_ref());
        let system_prompt = ff_agent::build_system_prompt(
            persona.as_deref(),
            &skills,
            &active,
            &user_ctx,
            memory.as_deref(),
            mode,
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
            state.store.as_ref(),
            &tool_ctx,
            &sid,
            &model,
            Some(system_prompt.as_str()),
            thinking,
            cancel,
            |event| {
                // Telemetry (RFC 0001 §8): fold per-turn metrics for events
                // that carry a message_id. Token also counts streamed chars as
                // a coarse token-cost proxy. In-process only — the sidecar path
                // has no local accumulator and goes straight to emit_agent_event.
                if let Ok(mut m) = metrics_for_events.lock() {
                    match &event {
                        AgentEvent::Token { message_id, delta } => {
                            m.note_turn(message_id);
                            m.chars += delta.chars().count();
                        }
                        AgentEvent::Reasoning { message_id, .. }
                        | AgentEvent::ToolCallStarted { message_id, .. } => {
                            m.note_turn(message_id);
                        }
                        AgentEvent::Done {
                            message_id,
                            prefill_estimates,
                            tier1_fires,
                            tier2_fires,
                            ..
                        } => {
                            m.note_turn(message_id);
                            m.note_done(
                                prefill_estimates.as_deref().unwrap_or(&[]),
                                tier1_fires.unwrap_or(0),
                                tier2_fires.unwrap_or(0),
                            );
                        }
                        AgentEvent::MemoryFlushed { message_id, .. } => {
                            m.note_turn(message_id);
                            m.note_flush();
                        }
                        _ => {}
                    }
                }
                // Wire mapping is shared with the sidecar path via
                // emit_agent_event, so the two cannot drift — the whole point
                // of the sidecar parity test (RFC 0004 §5).
                emit_agent_event(&app, &sid, event);
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
        let turn_end = std::time::Instant::now();
        let (
            chars,
            turn_count,
            round_trips,
            iter_ms,
            flushes,
            prefill_estimates,
            tier1_fires,
            tier2_fires,
        ) = metrics
            .lock()
            .map(|m| {
                let (c, t) = m.snapshot();
                let (rt, ims, fl) = m.timing(turn_end);
                (
                    c,
                    t,
                    rt,
                    ims,
                    fl,
                    m.prefill_estimates.clone(),
                    m.tier1_fires,
                    m.tier2_fires,
                )
            })
            .unwrap_or_default();
        let success = result.is_ok() && !cancel_probe.is_cancelled();
        let latency_ms = u32::try_from(turn_end.saturating_duration_since(turn_start).as_millis())
            .unwrap_or(u32::MAX);
        let tokens = u32::try_from(chars / 4).unwrap_or(u32::MAX);
        let turns = u32::try_from(turn_count).unwrap_or(u32::MAX);

        // F1 (#427): emit the per-turn timing baseline the performance epic (#426)
        // measures every later change against. Additive telemetry -- never alters
        // turn behavior. `tracing` mirrors it to the dev log for a `tauri dev` run.
        let stats = TurnStatsEvent {
            session_id: sid.clone(),
            round_trips,
            total_ms: latency_ms,
            iter_ms,
            flushes,
            chars: u32::try_from(chars).unwrap_or(u32::MAX),
            prefill_estimates,
            tier1_fires,
            tier2_fires,
        };
        tracing::info!(
            target: "turn_metrics",
            session_id = %sid,
            round_trips,
            total_ms = latency_ms,
            flushes,
            chars = stats.chars,
            iter_ms = ?stats.iter_ms,
            tier1_fires = stats.tier1_fires,
            tier2_fires = stats.tier2_fires,
            prefill_estimates = ?stats.prefill_estimates,
            "turn metrics (F1 baseline)"
        );
        let _ = app.emit("turn:stats", stats);

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

        // Drop the session's cancel token *before* the flush — but only if it is
        // still THIS turn's token. The map is keyed by session_id alone, so a
        // successor turn (e.g. the re-run that `edit_message` spawns after
        // cancelling this one) may have already replaced it via register_cancel.
        // Removing unconditionally would strip the live successor's token, killing
        // its Stop button and auto-denying its tool approvals. The identity check
        // on cancel_probe (the task-local clone this turn owns) leaves a
        // successor's token intact and also subsumes the single-turn case where
        // the next turn registers during this turn's multi-second silent flush.
        state.take_cancel_if(&session_id, &cancel_probe);

        // Pre-compaction memory flush (RFC 0006 §7.2): once the visible turn has
        // finished cleanly, persist any durable facts before context pressure forces
        // a summarization that would drop them. Silent — never adds to the transcript.
        if success {
            // Weak ambient reinforcement (RFC 0007 §10.1): the turn replied, so
            // refresh the curated chunks that were ambient-injected. No-op unless
            // `decay.ambient_gain > 0`.
            let _ = state.index().reinforce_ambient(&ambient_keys);
            state
                .maybe_flush_memory(provider.as_ref(), &registry, &sid, &model, cancel_probe)
                .await;
        }
    });
}

/// Edit a prior user message in place, truncate the transcript after it, and
/// re-run the turn from the edited prompt (#464, backend for #463). Cancels any
/// in-flight turn + pending approvals for the session first so a running turn
/// cannot append after the truncation, validates the edit in the store (rejecting
/// an unknown id, a wrong-session id, or a non-user message), then spawns a fresh
/// assistant turn over the existing `turn:*` / `tool:*` events. Returns the edited
/// message's id.
#[tauri::command]
fn edit_message(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    session_id: String,
    message_id: String,
    content: String,
    attachments: Option<Vec<Attachment>>,
) -> CmdResult<String> {
    // Stop any turn already streaming into this session before we mutate its
    // transcript (mirrors `cancel_turn`): otherwise the running turn would keep
    // appending messages past the cut we are about to make.
    if let Some(token) = state.take_cancel(&session_id) {
        token.cancel();
    }
    state.cancel_pending_approvals(&session_id);

    let edited_id = state
        .store
        .edit_user_message(&session_id, &message_id, content, attachments)
        .map_err(|e| e.to_string())?;

    spawn_assistant_turn(state.inner().clone(), app, session_id);

    Ok(edited_id)
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
        // This legacy shim has no effort control; preserve the persisted dial.
        reasoning_effort: current.reasoning_effort,
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

/// Store a provider secret for a connection in the OS keychain and flip its hasKey
/// flag. The secret value is never returned to the frontend.
#[tauri::command]
fn set_provider_secret(
    state: State<'_, Arc<AppState>>,
    connection_id: String,
    kind: SecretKind,
    value: String,
) -> CmdResult<()> {
    state.set_connection_secret(&connection_id, kind, &value)
}

/// Clear a provider secret for a connection and recompute its hasKey flag from the
/// remaining stored secrets.
#[tauri::command]
fn clear_provider_secret(
    state: State<'_, Arc<AppState>>,
    connection_id: String,
    kind: SecretKind,
) -> CmdResult<()> {
    state.clear_connection_secret(&connection_id, kind)
}

/// Which secret kinds are stored for a connection (#320), so each Bedrock secret
/// field shows its own Stored/Clear state. Presence only — no value is returned.
#[tauri::command]
fn provider_secret_presence(
    state: State<'_, Arc<AppState>>,
    connection_id: String,
) -> CmdResult<Vec<SecretKind>> {
    state.connection_secret_presence(&connection_id)
}

/// The Bedrock auth a connection resolves to right now (#320) — the explicit mode,
/// or the `Auto` precedence winner (API key > profile > IAM keys). `None` for
/// non-Bedrock or unknown connections; lets the UI badge the active credential.
#[tauri::command]
fn resolved_bedrock_auth(
    state: State<'_, Arc<AppState>>,
    connection_id: String,
) -> Option<BedrockAuth> {
    state.resolved_bedrock_auth(&connection_id)
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

/// Probe a connection for the settings "Test Connection" button. `id` defaults to
/// the active connection. Returns `Ok(())` on a successful round-trip, or an
/// `Err(String)` message the UI can show. Unlike `list_models`, the error is
/// surfaced (the button reports failure) rather than swallowed.
#[tauri::command]
async fn test_connection(state: State<'_, Arc<AppState>>, id: Option<String>) -> CmdResult<()> {
    let (provider, model) = state.build_provider_for(id.as_deref());
    provider
        .test_connection(&model)
        .await
        .map_err(|e| e.to_string())
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

/// Emit `phenotype:mcp-unavailable` when `phenotype`'s skills declare an MCP server
/// that is absent or not running (#301), so the frontend can surface a non-blocking
/// toast over the warn-only backend signal. Fires only when the list is non-empty,
/// matching the frontend listener contract; best-effort (a dropped emit is harmless).
fn emit_pheno_mcp_unavailable(app: &tauri::AppHandle, state: &AppState, phenotype: &str) {
    let servers = state.unavailable_skill_mcp_servers(phenotype);
    if !servers.is_empty() {
        let _ = app.emit(
            "phenotype:mcp-unavailable",
            PhenotypeMcpUnavailableEvent {
                phenotype: phenotype.to_string(),
                servers,
            },
        );
    }
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

/// All selectable phenotypes (built-in `default` + `~/.flowforge/phenos/`),
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
    emit_pheno_mcp_unavailable(&app, &state, &pheno.name);
    Ok(pheno)
}

/// Bind a single session to a phenotype, or clear the binding (`name: None`) so it
/// inherits the global active one (#246). Unlike `switch_phenotype`, this changes
/// only the named session's Pheno — other panes are untouched — and persists
/// nothing globally. Errors on an unknown phenotype name. The FE pane selector
/// (#245) calls this to make a pane's Pheno truly per-session.
#[tauri::command]
fn set_session_phenotype(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    session_id: String,
    name: Option<String>,
) -> CmdResult<()> {
    state.set_session_phenotype(&session_id, name.clone())?;
    if let Some(n) = name {
        emit_pheno_mcp_unavailable(&app, &state, &n);
    }
    Ok(())
}

/// Bind a single session's autonomy mode, or clear it (`mode: None`) so it inherits
/// the global default (#265). Per-pane, like `set_session_phenotype`.
#[tauri::command]
fn set_session_mode(state: State<'_, Arc<AppState>>, session_id: String, mode: Option<Mode>) {
    state.set_session_mode(&session_id, mode);
}

/// The global default autonomy mode (#265), inherited by sessions with no explicit
/// binding. Factory value `Auto`.
#[tauri::command]
fn get_default_mode(state: State<'_, Arc<AppState>>) -> Mode {
    state.default_mode()
}

/// Set and persist the global default autonomy mode (#265).
#[tauri::command]
fn set_default_mode(state: State<'_, Arc<AppState>>, mode: Mode) {
    state.set_default_mode(mode);
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

/// Outcome of an update check, serialized to the FE-owned `UpdateStatus` contract
/// in `lib/about.ts` (`{ kind: "upToDate", version }` | `{ kind: "available",
/// version, notes }`). The FE owns the toast copy; the backend reports only the
/// structured outcome (#159, RFC 0014).
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum UpdateStatus {
    UpToDate {
        version: String,
    },
    Available {
        version: String,
        notes: Option<String>,
    },
}

/// Build the updater. In prod it reads `plugins.updater` from `tauri.conf.json`
/// (the GitHub `latest.json` feed). When `FF_UPDATER_ENDPOINT` is set (the RFC 0014
/// D1 dev/dogfood channel) it points at that feed instead and accepts any version
/// that differs from the running one, so a locally-built artifact installs without a
/// version bump. Inert in prod (env unset).
fn updater(app: &tauri::AppHandle) -> CmdResult<tauri_plugin_updater::Updater> {
    use tauri_plugin_updater::UpdaterExt;
    if let Ok(endpoint) = std::env::var("FF_UPDATER_ENDPOINT") {
        let endpoint = url::Url::parse(&endpoint).map_err(|e| e.to_string())?;
        app.updater_builder()
            .endpoints(vec![endpoint])
            .map_err(|e| e.to_string())?
            .version_comparator(|current, update| update.version != current)
            .build()
            .map_err(|e| e.to_string())
    } else {
        app.updater().map_err(|e| e.to_string())
    }
}

/// Check the configured update feed. Returns the structured `UpdateStatus` so the UI
/// can branch (offer "Update now" on `available`). Errors (offline, malformed
/// manifest) surface as `Err(String)` for the FE to toast.
#[tauri::command]
async fn check_for_updates(app: tauri::AppHandle) -> CmdResult<UpdateStatus> {
    let current = app.package_info().version.to_string();
    match updater(&app)?.check().await.map_err(|e| e.to_string())? {
        Some(update) => Ok(UpdateStatus::Available {
            version: update.version.clone(),
            notes: update.body.clone(),
        }),
        None => Ok(UpdateStatus::UpToDate { version: current }),
    }
}

/// Download and install the available update, then relaunch. Re-checks rather than
/// caching the `Update` handle across IPC calls. A no-op if nothing is available.
#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> CmdResult<()> {
    let Some(update) = updater(&app)?.check().await.map_err(|e| e.to_string())? else {
        return Ok(());
    };
    update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    app.restart();
}

/// Map an [`AgentEvent`] to its frontend Tauri event and emit it.
///
/// This is the single source of truth for the `AgentEvent → app.emit(…)` wire
/// mapping shared by both turn paths — the in-process `run_turn` closure (in
/// `send_message`) and the CLI sidecar loop below (`run_sidecar_turn`).
/// Keeping both paths on one helper is exactly what the sidecar parity
/// smoke-test (RFC 0004 §5) guards against drift: if the mapping changes, it
/// changes in one place.
fn emit_agent_event(app: &tauri::AppHandle, session_id: &str, event: AgentEvent) {
    match event {
        AgentEvent::Token { message_id, delta } => {
            let _ = app.emit(
                "turn:token",
                TokenEvent {
                    session_id: session_id.to_string(),
                    message_id,
                    delta,
                },
            );
        }
        AgentEvent::Reasoning { message_id, delta } => {
            let _ = app.emit(
                "turn:reasoning",
                ReasoningEvent {
                    session_id: session_id.to_string(),
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
                    session_id: session_id.to_string(),
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
                    session_id: session_id.to_string(),
                    message_id,
                    call_id,
                    success,
                    result,
                },
            );
        }
        AgentEvent::Done {
            message_id,
            token_count,
            ..
        } => {
            let _ = app.emit(
                "turn:done",
                TurnDoneEvent {
                    session_id: session_id.to_string(),
                    message_id,
                    token_count,
                },
            );
        }
        AgentEvent::MemoryFlushed { message_id, writes } => {
            let _ = app.emit(
                "memory:flushed",
                MemoryFlushedEvent {
                    session_id: session_id.to_string(),
                    message_id,
                    writes,
                },
            );
        }
        AgentEvent::AttachmentsDropped { .. } => {
            // User-facing notice deferred to PR-2 / #342 (transcript render).
        }
        AgentEvent::Error { message } => {
            let _ = app.emit(
                "turn:error",
                TurnErrorEvent {
                    session_id: session_id.to_string(),
                    message,
                },
            );
        }
    }
}

/// CLI.7 — Sidecar parity smoke-test (RFC 0004 §5).
///
/// Spawns the bundled `flowforge` CLI as a Tauri sidecar (`externalBin`),
/// invokes `flowforge run "<prompt>" --json`, and re-emits every parsed
/// `AgentEvent` as the same Tauri events the in-process `run_turn` path
/// emits (`turn:token`, `tool:call`, `turn:done`, …).  This lets the
/// frontend verify that the sidecar produces an event stream equivalent
/// to the in-process path.
///
/// **PATH caveat (RFC 0004 §5):** the bundled sidecar binary lives inside
/// the app bundle and is NOT on the user's PATH.  Users who want
/// `flowforge` on the command line must install it separately or symlink
/// it manually — see the caveat doc in `docs/rfcs/0004-cli.md`.
#[tauri::command]
async fn run_sidecar_turn(
    app: tauri::AppHandle,
    prompt: String,
) -> Result<serde_json::Value, String> {
    let session_id = Uuid::new_v4().to_string();

    let command = app
        .shell()
        .sidecar("flowforge")
        .map_err(|e| format!("failed to resolve sidecar: {e}"))?
        .args(["run", prompt.as_str(), "--json"]);

    let (mut rx, _child) = command
        .spawn()
        .map_err(|e| format!("failed to spawn sidecar: {e}"))?;

    let mut event_count = 0usize;

    while let Some(event) = rx.recv().await {
        match event {
            tauri_plugin_shell::process::CommandEvent::Stdout(line) => {
                let line = String::from_utf8_lossy(&line);
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let agent_event: AgentEvent = match serde_json::from_str(line) {
                    Ok(e) => e,
                    // The CLI emits only `AgentEvent` lines under `--json`, so a parse
                    // failure normally means a schema drift between the two binaries.
                    // Break loudly rather than swallowing it without a trace.
                    Err(e) => {
                        eprintln!(
                            "[sidecar] failed to parse stdout line as AgentEvent ({e}); \
                             this usually signals a schema drift between the desktop and \
                             CLI binaries. line: {line}"
                        );
                        continue;
                    }
                };
                event_count += 1;
                emit_agent_event(&app, &session_id, agent_event);
            }
            tauri_plugin_shell::process::CommandEvent::Stderr(bytes) => {
                eprintln!("[sidecar stderr] {}", String::from_utf8_lossy(&bytes));
            }
            tauri_plugin_shell::process::CommandEvent::Terminated(payload) => {
                if payload.code != Some(0) {
                    return Err(format!("sidecar exited with code {:?}", payload.code));
                }
                break;
            }
            tauri_plugin_shell::process::CommandEvent::Error(err) => {
                return Err(format!("sidecar error: {err}"));
            }
            _ => {}
        }
    }

    Ok(serde_json::json!({
        "session_id": session_id,
        "events": event_count,
    }))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let state = Arc::new(AppState::new());
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        .manage(state.clone())
        .setup(move |app| {
            // `init_mcp` enters the shared Tokio runtime itself, so it's safe to
            // call here even though Tauri's `setup` runs on the main thread outside
            // an entered reactor on macOS (issue #117).
            state.init_mcp();
            // Drive the periodic idle-process reaper (`ProcessSupervisor::reap_idle`)
            // from the same setup path: it enters the runtime itself, so it's safe
            // here too. Finished processes and ones the agent abandoned (started but
            // never polled again) are cleaned up on a timer.
            state.start_process_reaper();
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
            export_session,
            rename_session,
            delete_session,
            fork_session,
            get_session_workspace,
            set_session_workspace,
            list_memory_files,
            read_memory_file,
            memory_overview,
            send_message,
            edit_message,
            run_sidecar_turn,
            cancel_turn,
            respond_approval,
            respond_ask,
            set_session_approve,
            set_always_approve,
            remove_always_approve,
            list_always_approved,
            get_provider_config,
            set_provider_config,
            get_provider_registry,
            set_active_connection,
            upsert_connection,
            remove_connection,
            set_provider_secret,
            clear_provider_secret,
            provider_secret_presence,
            resolved_bedrock_auth,
            get_search_config,
            set_search_config,
            list_models,
            test_connection,
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
            set_session_phenotype,
            set_session_mode,
            get_default_mode,
            set_default_mode,
            list_mcp_servers,
            restart_mcp_server,
            set_mcp_server_enabled,
            add_mcp_server,
            remove_mcp_server,
            check_for_updates,
            install_update,
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

#[cfg(test)]
mod tests {
    use super::{git_branch, mode_auto_approves, resolve_workspace_dir, TurnMetrics, UpdateStatus};
    use ff_core::Mode;
    use ff_tools::Safety;

    // `UpdateStatus` has no ts-rs binding -- it is cast on the FE side from the JSON
    // this serializes to (`lib/about.ts`). Pin the wire shape so the hand-written FE
    // type and this enum cannot drift apart silently (#159).
    #[test]
    fn update_status_matches_fe_contract() {
        assert_eq!(
            serde_json::to_value(UpdateStatus::UpToDate {
                version: "0.1.0".into()
            })
            .unwrap(),
            serde_json::json!({ "kind": "upToDate", "version": "0.1.0" })
        );
        assert_eq!(
            serde_json::to_value(UpdateStatus::Available {
                version: "0.2.0".into(),
                notes: Some("notes".into()),
            })
            .unwrap(),
            serde_json::json!({ "kind": "available", "version": "0.2.0", "notes": "notes" })
        );
        assert_eq!(
            serde_json::to_value(UpdateStatus::Available {
                version: "0.2.0".into(),
                notes: None,
            })
            .unwrap(),
            serde_json::json!({ "kind": "available", "version": "0.2.0", "notes": null })
        );
    }

    // The one mode-driven auto-approve carve-out (#265): only Auto+Write is silent.
    // Dangerous always prompts (any mode), and the other modes prompt every write.
    #[test]
    fn mode_auto_approves_only_auto_write() {
        assert!(mode_auto_approves(Mode::Auto, Safety::Write));
        // Dangerous is never auto-approved by mode -- this is the #232 invariant
        // that keeps python / `rm` behind a deliberate yes.
        assert!(!mode_auto_approves(Mode::Auto, Safety::Dangerous));
        // Act and Plan prompt for writes too.
        assert!(!mode_auto_approves(Mode::Act, Safety::Write));
        assert!(!mode_auto_approves(Mode::Plan, Safety::Write));
        assert!(!mode_auto_approves(Mode::Act, Safety::Dangerous));
        assert!(!mode_auto_approves(Mode::Plan, Safety::Dangerous));
        // ReadOnly never reaches the approver, but the helper is conservative.
        assert!(!mode_auto_approves(Mode::Auto, Safety::ReadOnly));
    }

    #[test]
    fn resolve_workspace_dir_accepts_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_workspace_dir(dir.path().to_str().unwrap()).unwrap();
        assert!(resolved.is_dir());
        // Canonicalized: absolute and symlink-resolved.
        assert_eq!(resolved, std::fs::canonicalize(dir.path()).unwrap());
    }

    #[test]
    fn resolve_workspace_dir_rejects_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let err = resolve_workspace_dir(missing.to_str().unwrap()).unwrap_err();
        assert!(err.contains("cannot resolve directory"));
    }

    #[test]
    fn resolve_workspace_dir_rejects_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a-file.txt");
        std::fs::write(&file, "x").unwrap();
        let err = resolve_workspace_dir(file.to_str().unwrap()).unwrap_err();
        assert!(err.contains("not a directory"));
    }

    #[test]
    fn git_branch_reads_symbolic_ref() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        assert_eq!(git_branch(dir.path()), Some("feature/x".to_string()));
    }

    #[test]
    fn git_branch_is_none_for_detached_head() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        // Detached HEAD stores a bare commit SHA, not a `ref:` line.
        std::fs::write(
            dir.path().join(".git/HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        assert_eq!(git_branch(dir.path()), None);
    }

    #[test]
    fn git_branch_is_none_when_not_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(git_branch(dir.path()), None);
    }

    // F1 (#427): the turn-metrics accumulator counts one round-trip per distinct
    // assistant message, breaks the turn into per-iteration wall-clock, and counts
    // silent memory flushes -- the baseline the performance epic measures against.
    #[test]
    fn turn_metrics_counts_round_trips_flushes_and_iterations() {
        let mut m = TurnMetrics::default();
        // Two iterations (two distinct message ids); repeats are idempotent.
        m.note_turn("m1");
        m.note_turn("m1");
        m.chars += 5;
        m.note_flush();
        m.note_turn("m2");
        m.note_turn("m2");

        let (chars, turns) = m.snapshot();
        assert_eq!(chars, 5);
        assert_eq!(turns, 2, "two distinct assistant messages = two turns");

        let (round_trips, iter_ms, flushes) = m.timing(std::time::Instant::now());
        assert_eq!(round_trips, 2, "one round-trip per distinct message id");
        assert_eq!(iter_ms.len(), 2, "one wall-clock sample per iteration");
        assert_eq!(flushes, 1, "exactly one mid-turn flush counted");
    }

    // A turn that never reached the model (no assistant message) reports a clean
    // zero baseline rather than panicking on the empty iteration vector.
    #[test]
    fn turn_metrics_empty_turn_is_zeroed() {
        let m = TurnMetrics::default();
        let (round_trips, iter_ms, flushes) = m.timing(std::time::Instant::now());
        assert_eq!(round_trips, 0);
        assert!(iter_ms.is_empty());
        assert_eq!(flushes, 0);
        // F1b (#441): a turn whose Done carried no telemetry reports a clean zero.
        assert!(m.prefill_estimates.is_empty());
        assert_eq!(m.tier1_fires, 0);
        assert_eq!(m.tier2_fires, 0);
    }

    #[test]
    fn turn_metrics_note_done_folds_f1b_telemetry() {
        // #441: the per-round-trip prefill estimate and the two compaction-fire
        // counts from the turn's Done event are captured verbatim for `turn:stats`.
        let mut m = TurnMetrics::default();
        m.note_done(&[120, 340, 75], 2, 1);
        assert_eq!(m.prefill_estimates, vec![120, 340, 75]);
        assert_eq!(m.tier1_fires, 2);
        assert_eq!(m.tier2_fires, 1);
    }
}
