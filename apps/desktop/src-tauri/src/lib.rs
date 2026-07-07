//! Thin Tauri shell. Per the SOP, this layer contains only command/event glue —
//! all business logic lives in the `ff-*` crates. Each handler deserializes,
//! calls into a crate, and returns. Streaming responses go out as Tauri events.

mod dev_update_watcher;
mod git_watch;
mod optimize;
mod secrets;
mod state;
mod tools;

use async_trait::async_trait;
use ff_agent::{
    drive_goal, run_turn, AgentEvent, Approver, CancelToken, GateDecision, GoalIteration,
    IterationOutcome, ToolContext,
};
use ff_core::events::{
    ApprovalSafety, EvolveCostEstimate, IntentionSignal, McpStatusChangedEvent, MemoryFlushedEvent,
    OutputStreamKind, PhenotypeMcpUnavailableEvent, ReasoningEvent, SessionTitleUpdatedEvent,
    SkillActivated, SkillCompleted, SkillEvolveApprovalRequestEvent,
    SkillInstallApprovalRequestEvent, SkillsChangedEvent, TokenEvent, ToolApprovalRequestEvent,
    ToolAskRequestEvent, ToolCallEvent, ToolOutputChunkEvent, ToolResultEvent, TurnDoneEvent,
    TurnErrorEvent, TurnStatsEvent, UpdateProgressEvent,
};
use ff_core::{
    Attachment, BedrockAuth, CreateScheduledTaskInput, Format, Goal, GoalStatus, McpServerConfig,
    McpServerStatus, MemoryFileInfo, MemoryFileKind, MemoryOverview, Message, Mode, ModelSelection,
    PermissionCell, PermissionMatrixView, Phenotype, ProviderConfig, ProviderConnection,
    ProviderKind, ProviderRegistry, ResolvedModel, Role, RunRecord, RunStatus, ScheduledTask,
    SearchConfig, SecretKind, Session, SessionWorkspace, Skill, SkillInfo, SkillManifest, TaskKind,
};
use ff_scheduled::ScheduledApprover;
use ff_signals::SkillAggregate;
use ff_tools::Safety;
use state::AppState;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tauri::{Emitter, Manager, State};
use tauri_plugin_shell::ShellExt;
use uuid::Uuid;

// Boot-timing trace (#599 item 0). A single shared clock for the cold-start
// milestones — webview page-load, FE first paint, `AppState::new()` (now run
// post-paint on the background hydrate task — see `run`), and `app_ready` — so
// the <200ms-to-paint target is measured against real numbers instead of
// guesses. Every `[boot-trace]` line reports milliseconds since `BOOT_T0` (set
// as the first line of `run()`). Entirely gated behind the `FF_BOOT_TRACE` env
// var: unset, each stamp is a cheap `OnceLock` load and returns before
// formatting or I/O, so a normal launch pays nothing.
static BOOT_T0: OnceLock<std::time::Instant> = OnceLock::new();
static BOOT_TRACE_ON: OnceLock<bool> = OnceLock::new();

// Paint-first boot (#599): flipped by the background hydrate task in `run` once
// `AppState::new()` has finished and the state has been managed. The FE gates
// its backend-dependent work on the `app:ready` event plus this flag
// (subscribe-then-check) so the invoke handlers — which read
// `State<'_, Arc<AppState>>` — are never hit before the state exists.
static APP_READY: AtomicBool = AtomicBool::new(false);

fn boot_trace_enabled() -> bool {
    *BOOT_TRACE_ON.get_or_init(|| std::env::var_os("FF_BOOT_TRACE").is_some())
}

/// Emit `[boot-trace] <label> +<ms>ms[ (<extra>)]`, the elapsed since `BOOT_T0`.
/// Used for the cumulative milestones (window-relative), not per-step durations.
pub(crate) fn boot_trace(label: &str, extra: Option<&str>) {
    if !boot_trace_enabled() {
        return;
    }
    let ms = BOOT_T0
        .get()
        .map(|t0| t0.elapsed().as_secs_f64() * 1000.0)
        .unwrap_or(0.0);
    match extra {
        Some(e) => eprintln!("[boot-trace] {label} +{ms:.1}ms ({e})"),
        None => eprintln!("[boot-trace] {label} +{ms:.1}ms"),
    }
}

/// Emit `[boot-trace] <label> <ms>ms` for a single step's own duration (not
/// since-`BOOT_T0`). Used by `AppState::new()` to break down each blocking open
/// so items 1-4 are driven by which SQLite open / scan actually costs.
pub(crate) fn boot_trace_step(label: &str, dur: Duration) {
    if !boot_trace_enabled() {
        return;
    }
    eprintln!("[boot-trace] {label} {:.1}ms", dur.as_secs_f64() * 1000.0);
}

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

/// The invocation-time gate for a resolved permission cell (#702): `Some(true)`
/// auto-approves, `Some(false)` rejects, `None` means prompt the user (`Ask`).
/// Superseded by [`pre_prompt_decision`] in production, but kept for the
/// `edited_cell_flips_the_invocation_gate` test which exercises the raw cell semantics.
#[cfg(test)]
fn matrix_gate(cell: PermissionCell) -> Option<bool> {
    if cell.is_allow() {
        Some(true)
    } else if cell.is_deny() {
        Some(false)
    } else {
        None
    }
}

/// Routes write/dangerous tool calls through a UI confirmation. Read-only calls
/// never reach this approver — the agent loop short-circuits them.
/// Whether the active autonomy mode auto-approves a call of this safety without a
struct UiApprover {
    app: tauri::AppHandle,
    state: Arc<AppState>,
    session_id: String,
    /// The session's resolved autonomy mode for this turn (#265).
    mode: Mode,
}

/// Resolve the "relevant argument" for scoped permission rules (#712).
///
/// Each entry is verified against the real tool arg schema in `ff-tools`
/// (#768 review B2): `bash` takes `command`, `python` takes `code`, and the
/// filesystem mutators take `path`. Only tools that can actually reach the
/// approval gate are listed — the read-only search tools (`glob`, `grep`)
/// short-circuit as `Safety::ReadOnly` before `approve()`, so a rule on them
/// would never fire; listing them (with the wrong key, as before) was dead,
/// misleading code.
fn resolve_tool_arg(name: &str, args: &serde_json::Value) -> Option<String> {
    let key = match name {
        "bash" => "command",
        "python" => "code",
        "view" | "edit" | "write" => "path",
        _ => return None,
    };
    args.get(key).and_then(|v| v.as_str()).map(Into::into)
}

/// The synchronous, pre-prompt decision for a tool call (#828 Part C, #829 review).
/// Pure — no AppHandle, no async, no state beyond the inputs. Testable directly,
/// so a regression that reorders the allowlist above the Deny gate is caught.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrePromptDecision {
    /// The matrix denies this call outright (e.g. Plan x Write).
    Deny,
    /// Auto-approved (allowlist hit, scoped Allow rule, or matrix Allow cell).
    Allow,
    /// None of the sync gates resolved it — prompt the user asynchronously.
    Prompt,
}

/// Evaluate the synchronous approval gates in their canonical order (#827):
/// 1. Matrix Deny is absolute (no override).
/// 2. Allowlist accelerates Ask cells.
/// 3. Scoped rules (Deny vetoes; Allow approves unless Dangerous).
/// 4. Matrix Allow auto-approves; Ask falls through to Prompt.
fn pre_prompt_decision(
    cell: ff_core::PermissionCell,
    allowlisted: bool,
    scoped_effect: Option<ff_core::RuleEffect>,
    safety: Safety,
) -> PrePromptDecision {
    use ff_core::{PermissionCell, RuleEffect};
    if cell.is_deny() {
        return PrePromptDecision::Deny;
    }
    if allowlisted {
        return PrePromptDecision::Allow;
    }
    match scoped_effect {
        Some(RuleEffect::Deny) => return PrePromptDecision::Deny,
        Some(RuleEffect::Allow) if safety != Safety::Dangerous => {
            return PrePromptDecision::Allow;
        }
        _ => {}
    }
    match cell {
        PermissionCell::Allow => PrePromptDecision::Allow,
        PermissionCell::Deny => PrePromptDecision::Deny, // unreachable (handled above)
        PermissionCell::Ask => PrePromptDecision::Prompt,
    }
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
        // Snapshot the matrix once for this call, read live (#702/#742) so a
        // Control-panel edit takes effect on the next tool invocation.
        let matrix = self.state.permission_matrix();
        let cell = matrix.effective_cell(name, self.mode, safety);
        let allowlisted = self.state.allowlist_covers(&self.session_id, name, safety);
        let resolved_arg = resolve_tool_arg(name, args);
        let scoped_effect = matrix.evaluate_rules(name, resolved_arg.as_deref(), self.mode);

        // The synchronous pre-prompt decision encodes the canonical gate order
        // (#827/#828 Part C). Extracted so it is unit-testable without an AppHandle.
        match pre_prompt_decision(cell, allowlisted, scoped_effect, safety) {
            PrePromptDecision::Deny => return false,
            PrePromptDecision::Allow => {
                if scoped_effect == Some(ff_core::RuleEffect::Allow) {
                    tracing::info!(
                        tool = name,
                        arg = ?resolved_arg,
                        "scoped rule auto-approved"
                    );
                }
                return true;
            }
            PrePromptDecision::Prompt => {}
        }

        let approval_safety = match safety {
            Safety::Write => ApprovalSafety::Write,
            Safety::Sensitive => ApprovalSafety::Sensitive,
            Safety::Dangerous => ApprovalSafety::Dangerous,
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
                safety: approval_safety,
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
        // The loop forwards the tool args; the host reads the `question` field and
        // the optional `secret` flag (#562).
        let question = args
            .get("question")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let secret = args
            .get("secret")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let rx = self.state.register_ask(&self.session_id, call_id);
        let _ = self.app.emit(
            "tool:ask-request",
            ToolAskRequestEvent {
                session_id: self.session_id.clone(),
                message_id: message_id.to_string(),
                call_id: call_id.to_string(),
                question,
                secret,
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
    // A bare `＋` (no goal) is a deferred draft (#671 item 2a): not written to
    // disk until its first message, so clicking `＋` never accrues empty rows. A
    // goal-bearing session (scheduled/intention run, #543) commits immediately and
    // announces its intention so it lists and starts right away.
    match goal {
        Some(goal) => {
            let session = state.store.create_session(Some(goal.clone()));
            let _ = app.emit(
                "signal:intention",
                IntentionSignal {
                    session_id: session.id.clone(),
                    goal,
                },
            );
            session
        }
        None => state.store.create_draft_session(),
    }
}

#[tauri::command]
fn list_sessions(state: State<'_, Arc<AppState>>) -> Vec<Session> {
    state.store.list_sessions()
}

#[tauri::command]
fn get_messages(state: State<'_, Arc<AppState>>, session_id: String) -> Vec<Message> {
    // Reconcile orphaned empty assistant rows left by a hard kill (SIGKILL /
    // panic=abort), which runs no Drop guard (#646). Skip while a turn is live:
    // that session's reserved tail row is a legitimate transient, and the
    // AssistantRowGuard covers its graceful drop.
    if !state.has_active_turn(&session_id) {
        state
            .store
            .reconcile_orphaned_assistant_rows(&session_id, ff_agent::INTERRUPTED_NOTICE);
    }
    state.store.get_messages(&session_id)
}

/// Full-text search across all sessions (#679). Returns ranked hits with snippets.
#[tauri::command]
fn search_messages(
    state: State<'_, Arc<AppState>>,
    query: String,
    limit: Option<usize>,
) -> Vec<ff_session::SearchHit> {
    state.store.search_messages(&query, limit.unwrap_or(50))
}

/// Full-text search within a single session (#679). For in-thread find.
#[tauri::command]
fn search_in_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    query: String,
) -> Vec<ff_session::SearchHit> {
    state.store.search_in_session(&session_id, &query)
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
    state.compaction_cache.invalidate(&session_id);
    state.store.delete_session(&session_id);
    state.reap_session_processes(&session_id);
    // Release this session's per-workspace MCP instance refs, evicting any instance no
    // live session references (RFC 0018 §4.3). Spawned (not awaited) like the process
    // reap above: `delete_session` is a sync Tauri command that runs off the reactor on
    // macOS (#117), so a bare block_on/await would panic -- `tauri::async_runtime::spawn`
    // enters the shared runtime.
    let state_for_mcp = state.inner().clone();
    let sid = session_id.clone();
    tauri::async_runtime::spawn(async move {
        state_for_mcp.release_session_mcp(&sid).await;
    });
}

/// All scheduled tasks, newest first, with derived cadence label / next run
/// (RFC 0017 #540). Built-in + user-created.
#[tauri::command]
fn list_scheduled_tasks(state: State<'_, Arc<AppState>>) -> Vec<ScheduledTask> {
    state.scheduled.list()
}

/// Create a scheduled task. Validates the cron expression; the human cadence
/// label and next-run are derived server-side, never sent by the FE.
#[tauri::command]
fn create_scheduled_task(
    state: State<'_, Arc<AppState>>,
    input: CreateScheduledTaskInput,
) -> Result<ScheduledTask, String> {
    state.scheduled.create(input)
}

/// Pause/resume a task; returns the updated task. Errors on an unknown id.
#[tauri::command]
fn toggle_scheduled_task(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<ScheduledTask, String> {
    state
        .scheduled
        .toggle_paused(&id)
        .ok_or_else(|| format!("unknown scheduled task: {id}"))
}

/// Delete a task. Rejected for built-in tasks (they ship with the app).
#[tauri::command]
fn delete_scheduled_task(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    state
        .scheduled
        .delete(&id)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Preview the human cadence label a cron expression would produce, for the
/// New-task form's Custom-cron field. Derived from the same source as `list`.
#[tauri::command]
fn preview_cadence(cron: String) -> Result<String, String> {
    ff_scheduled::cron::parse(&cron)?;
    Ok(ff_scheduled::cron::cadence_label(&cron))
}

/// A task's fire history, newest first, capped at 50. Backs the run-history list
/// and the ↗ open-session affordance (RFC 0017 §6.2, #544).
#[tauri::command]
fn list_scheduled_runs(state: State<'_, Arc<AppState>>, id: String) -> Vec<RunRecord> {
    state.scheduled.runs(&id, 50)
}

/// Engage or release the global pause-all kill-switch (RFC 0017 §8.3, #544). A
/// true switch: the sweep is gated regardless of per-task `paused`, so tasks
/// created while engaged stay held too. Emits `scheduled:changed` so the UI
/// reflects the new gate.
#[tauri::command]
fn set_scheduled_paused_all(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    paused: bool,
) -> bool {
    state.scheduled.set_all_paused(paused);
    let _ = app.emit("scheduled:changed", state.scheduled.list());
    paused
}

// ===== Goal mode (RFC 0020, #716) =====
// The five `goal_*` IPC commands are the FE-facing half of the goal lifecycle;
// the `goal_complete` capability is also an agent tool (dual-surface, §7). Each
// mutation persists through the path-injected `GoalStore` and emits
// `goal:updated` so the FE panel (#717) live-refreshes without polling. The
// self-continue loop is spawned by `goal_set` / `goal_resume` — see
// `spawn_goal_loop`.

/// Begin (or replace) the active goal for a session and start the self-continue
/// loop (RFC 0020 §5.1). An empty objective is rejected. A pre-existing goal for
/// the session is overwritten — starting a new objective is a deliberate reset.
#[tauri::command]
async fn goal_set(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    session_id: String,
    objective: String,
    max_iterations: Option<u32>,
    max_tokens: Option<u64>,
    max_wall_ms: Option<i64>,
) -> Result<Goal, String> {
    let objective = objective.trim();
    if objective.is_empty() {
        return Err("goal objective must not be empty".into());
    }
    // Re-setting a goal is a deliberate reset, but a loop may still be driving the
    // OLD goal on an in-memory copy (#753 review blocker 2). Stop it first — cancel
    // the in-flight turn and wait (bounded) for the loop to release its slot — so
    // its final checkpoint can't clobber the fresh goal we're about to write.
    stop_goal_loop(state.inner(), &session_id).await;

    let mut goal = Goal::new(&session_id, objective, now_ms());
    if let Some(m) = max_iterations {
        goal.budget.max_iterations = m;
    }
    goal.budget.max_tokens = max_tokens;
    goal.budget.max_wall_ms = max_wall_ms;
    state
        .goals
        .save(&goal)
        .map_err(|e| format!("failed to persist goal: {e}"))?;
    let _ = app.emit("goal:updated", &goal);
    spawn_goal_loop(state.inner().clone(), app, session_id);
    Ok(goal)
}

/// Snapshot the current goal for a session, or `None` if there is no goal
/// checkpoint (RFC 0020 §7 — panel poll / event join). A corrupt checkpoint file
/// surfaces as an error rather than a silent `None`.
#[tauri::command]
fn goal_status(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<Option<Goal>, String> {
    state
        .goals
        .load(&session_id)
        .map_err(|e| format!("failed to read goal: {e}"))
}

/// Pause a running goal at the next boundary (RFC 0020 §5.3). Idempotent: pausing
/// an already-paused/terminal goal just returns its current state. The loop
/// observes the persisted `Paused` status at its next boundary check and stops
/// resumably; this command also flips it eagerly so the FE reflects intent now.
#[tauri::command]
async fn goal_pause(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    session_id: String,
) -> Result<Option<Goal>, String> {
    // Stop the loop first (cancel the in-flight turn + wait for it to exit).
    // The loop owns the goal in memory and never re-reads it, so writing Paused
    // here while it runs would be clobbered by its next checkpoint save. A
    // cancelled turn already transitions the goal to Paused (resumable) via
    // drive_goal; we then ensure Paused for the no-loop-running case.
    stop_goal_loop(state.inner(), &session_id).await;
    let Some(mut goal) = state
        .goals
        .load(&session_id)
        .map_err(|e| format!("failed to read goal: {e}"))?
    else {
        return Ok(None);
    };
    if goal.status == GoalStatus::Active {
        goal.status = GoalStatus::Paused;
        goal.updated_ms = now_ms();
        state
            .goals
            .save(&goal)
            .map_err(|e| format!("failed to persist goal: {e}"))?;
        let _ = app.emit("goal:updated", &goal);
    }
    Ok(Some(goal))
}

/// Resume a paused goal and restart the loop from the last persisted checkpoint
/// (RFC 0020 §5.3 — resume replays from the last completed iteration, never a
/// partial turn). Only a `Paused` goal resumes; a terminal goal is returned
/// unchanged.
#[tauri::command]
fn goal_resume(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    session_id: String,
) -> Result<Option<Goal>, String> {
    let Some(mut goal) = state
        .goals
        .load(&session_id)
        .map_err(|e| format!("failed to read goal: {e}"))?
    else {
        return Ok(None);
    };
    if goal.status == GoalStatus::Paused {
        goal.status = GoalStatus::Active;
        goal.updated_ms = now_ms();
        state
            .goals
            .save(&goal)
            .map_err(|e| format!("failed to persist goal: {e}"))?;
        let _ = app.emit("goal:updated", &goal);
        spawn_goal_loop(state.inner().clone(), app, session_id);
    }
    Ok(Some(goal))
}

/// Delete the goal for a session entirely (RFC 0020 §7 — dismiss/clear). Stops
/// any running loop first (so its next checkpoint can't recreate the file we're
/// deleting), then removes the checkpoint file. Idempotent — clearing a
/// nonexistent goal is fine.
#[tauri::command]
async fn goal_clear(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    session_id: String,
) -> Result<(), String> {
    stop_goal_loop(state.inner(), &session_id).await;
    state
        .goals
        .delete(&session_id)
        .map_err(|e| format!("failed to clear goal: {e}"))?;
    let _ = app.emit("goal:cleared", &session_id);
    Ok(())
}

/// Mark a session's goal complete from the FE (RFC 0020 §7 — dual-surface: this
/// is the IPC half of the `goal_complete` capability the agent tool also
/// exposes). Stops any running loop first, then transitions the goal to
/// `Completed` and persists. Returns `None` if there is no goal. Idempotent on a
/// terminal goal (returns it unchanged).
#[tauri::command]
async fn goal_complete(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    session_id: String,
) -> Result<Option<Goal>, String> {
    stop_goal_loop(state.inner(), &session_id).await;
    let Some(mut goal) = state
        .goals
        .load(&session_id)
        .map_err(|e| format!("failed to read goal: {e}"))?
    else {
        return Ok(None);
    };
    if !matches!(goal.status, GoalStatus::Completed) {
        goal.status = GoalStatus::Completed;
        goal.updated_ms = now_ms();
        state
            .goals
            .save(&goal)
            .map_err(|e| format!("failed to persist goal: {e}"))?;
        let _ = app.emit("goal:updated", &goal);
    }
    Ok(Some(goal))
}

/// Fire a scheduled task immediately, off-schedule (RFC 0017 §8.3). Runs the
/// same bounded headless turn the scheduler would, records the run, and stamps
/// `last_run` so the manual fire counts as the most recent run (and the
/// background sweep will not immediately re-fire it). Returns the `RunRecord`
/// and emits `scheduled:fired` + `scheduled:changed` so the UI live-updates.
#[tauri::command]
async fn run_scheduled_task_now(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    id: String,
) -> Result<RunRecord, String> {
    let task = state
        .scheduled
        .get(&id)
        .ok_or_else(|| format!("unknown scheduled task: {id}"))?;
    // The global kill-switch blocks manual fires too — "pause all" must mean
    // nothing fires unattended, including a stray "run now" (RFC 0017 §8.3).
    if state.scheduled.is_all_paused() {
        return Err("scheduled tasks are globally paused".into());
    }
    let runner = DesktopTaskRunner {
        state: state.inner().clone(),
        app: app.clone(),
    };
    let fired_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let outcome = ff_scheduled::TaskRunner::fire(&runner, &task).await;
    let record =
        state
            .scheduled
            .append_run(&task.id, outcome.session_id.as_deref(), outcome.status);
    state.scheduled.stamp_last_run(&task.id, fired_ms);
    emit_scheduled_fired(&app, &state.scheduled, &record);
    Ok(record)
}

/// Emit `scheduled:fired` (the full `RunRecord`, matching the FE binding) plus a
/// `scheduled:changed` snapshot of the task list, so the UI updates run history,
/// the ↗ session link, and the derived next/last stamps without polling (#544).
fn emit_scheduled_fired(
    app: &tauri::AppHandle,
    store: &ff_scheduled::ScheduledStore,
    run: &RunRecord,
) {
    let _ = app.emit("scheduled:fired", run);
    let _ = app.emit("scheduled:changed", store.list());
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
pub(crate) fn git_branch(dir: &std::path::Path) -> Option<String> {
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

/// Local branch names in `dir`'s git work tree, refname-sorted, for the branch
/// picker (#628). `Ok(vec![])` when `dir` is not a repo; `Err` only on an actual
/// git failure. Uses `for-each-ref` (stable plumbing) so packed refs are included
/// -- reading `.git/refs/heads/` directly, like [`git_branch`], would miss them.
/// Extracted as a free fn so it is unit-testable against a temp repo without a
/// Tauri `State`.
pub(crate) fn list_local_branches(dir: &std::path::Path) -> Result<Vec<String>, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["for-each-ref", "--format=%(refname:short)", "refs/heads/"])
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        // Not a repo (or a bare/broken one): no branches to offer rather than a
        // hard error, mirroring how `git_branch` returns `None` off-repo.
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Check out `branch` in `dir`. Validates that `branch` is one of the repo's local
/// branches *before* spawning git: this yields a clean error for a stale picker
/// entry and closes the `git checkout -<flag>` argument-injection vector (a branch
/// literally named like a flag can never reach the checkout). On a checkout that
/// git rejects (e.g. the working tree would be overwritten) the trimmed stderr is
/// surfaced verbatim. Free fn for the same testability reason as above.
pub(crate) fn switch_branch(dir: &std::path::Path, branch: &str) -> Result<(), String> {
    if !list_local_branches(dir)?.iter().any(|b| b == branch) {
        return Err(format!("unknown branch: {branch}"));
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["checkout", branch])
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        let msg = msg.trim();
        return Err(if msg.is_empty() {
            format!("git checkout {branch} failed")
        } else {
            msg.to_string()
        });
    }
    Ok(())
}

/// List the local branches of a session's cwd repo, for a switch picker (#628).
/// Read-only; empty when the cwd is not a git work tree.
#[tauri::command]
fn list_branches(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<Vec<String>, String> {
    list_local_branches(&state.session_root(&session_id))
}

/// Check out `branch` in a session's cwd and return the updated workspace (#628,
/// item #2 of #618).
///
/// DESIGN -- seamless connection with the #627 flash: this command *emits*
/// `workspace:branch-changed` itself rather than leaning solely on `GitHeadWatcher`
/// to notice the `.git/HEAD` write. The watcher is only re-pointed at turn-start
/// (`align_git_watcher`), so a watcher-only path would (a) fire ~200ms late behind
/// the debounce and (b) miss a switch made in a session that has not run a turn
/// yet. Emitting here routes the change through the exact reactive channel an
/// external checkout uses -- `onWorkspaceBranchChanged` -> store `applyBranchChanged`
/// -> `ff-branch-flash` (#627) -- so the chip updates and flashes immediately and
/// deterministically, with no extra FE wiring. When the watcher *is* pointed it may
/// re-emit the same branch ~200ms later; that is harmless: `applyBranchChanged` is
/// idempotent and #627's transition guard suppresses a second flash on an unchanged
/// value.
#[tauri::command]
fn checkout_branch(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    session_id: String,
    branch: String,
) -> Result<SessionWorkspace, String> {
    let root = state.session_root(&session_id);
    switch_branch(&root, &branch)?;
    let ws = SessionWorkspace {
        git_branch: git_branch(&root),
        path: root.display().to_string(),
    };
    let _ = app.emit("workspace:branch-changed", ws.clone());
    Ok(ws)
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

/// Wall-clock now in epoch milliseconds (the read instant for lazy decay). Uses
/// `SystemTime` to avoid pulling `chrono` into this crate just for one call.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A chunk path made relative to the memory root, with forward slashes — the
/// same shape `MemoryFileInfo::rel_path` uses (e.g. `MEMORY.md`,
/// `daily/2026-06-25.md`).
fn chunk_rel_path(root: &std::path::Path, path: &std::path::Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// First non-empty, non-heading line of a chunk, trimmed and capped to ~80
/// chars — the human-readable summary for a Salience list row (#293). Heading
/// lines (hash-prefixed) are skipped because the heading is already its own
/// field; a char slice would cut mid-word and start on blank/heading lines.
fn chunk_preview(text: &str) -> String {
    const MAX: usize = 80;
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("#"))
        .unwrap_or("");
    if line.chars().count() > MAX {
        let head: String = line.chars().take(MAX).collect();
        format!("{}…", head.trim_end())
    } else {
        line.to_string()
    }
}

/// Per-chunk salience stats for the Settings Salience surface (#293). Joins every
/// indexed chunk (`all_chunks`) with its `chunk_stats` usage
/// (`chunk_stats_snapshot`); a chunk with no stats row is a never-recalled chunk
/// (weight `1.0`, never dormant, not pinned, no `last_accessed`).
#[tauri::command]
fn list_memory_chunks(state: State<'_, Arc<AppState>>) -> Vec<ff_core::MemoryChunkStat> {
    let mem = state.memory();
    let root = mem.root().to_path_buf();
    let chunks = mem.all_chunks();
    let keys: Vec<String> = chunks.iter().map(ff_memory::chunk_key).collect();
    let stats = state
        .index()
        .chunk_stats_snapshot(&keys, now_ms())
        .unwrap_or_default();
    chunks
        .iter()
        .zip(keys.iter())
        .map(|(chunk, key)| {
            let snap = stats.get(key);
            ff_core::MemoryChunkStat {
                chunk_key: key.clone(),
                rel_path: chunk_rel_path(&root, &chunk.path),
                heading: chunk.heading.clone(),
                preview: chunk_preview(&chunk.text),
                weight: snap.map(|s| s.weight).unwrap_or(1.0),
                access_count: snap.map(|s| s.access_count).unwrap_or(0),
                last_accessed_ms: snap.map(|s| s.last_accessed_ms),
                dormant: snap.is_some_and(|s| s.dormant),
                pinned: snap.is_some_and(|s| s.pinned),
            }
        })
        .collect()
}

/// Reset (wake) a chunk: restore its weight to `1.0` and stamp `last_accessed`
/// now, creating the stats row if absent (#293). Never edits Markdown.
#[tauri::command]
fn reset_memory_chunk(state: State<'_, Arc<AppState>>, chunk_key: String) -> Result<(), String> {
    state
        .index()
        .reset_chunk(&chunk_key)
        .map_err(|e| e.to_string())
}

/// Pin/unpin a chunk: a pinned chunk holds effective weight `1.0` (decay
/// skipped) and is never dormant (#293). Never edits Markdown.
#[tauri::command]
fn set_memory_chunk_pinned(
    state: State<'_, Arc<AppState>>,
    chunk_key: String,
    pinned: bool,
) -> Result<(), String> {
    state
        .index()
        .set_chunk_pinned(&chunk_key, pinned)
        .map_err(|e| e.to_string())
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
    // Cancel any in-flight turn so a cancel+resend sequence never races two
    // parallel agent loops into the same session (#744). Mirrors edit_message.
    if let Some(token) = state.take_cancel(&session_id) {
        token.cancel();
    }
    state.cancel_pending_approvals(&session_id);

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
    let (mut provider, _) =
        state.build_provider_for(Some(&selection.connection), Some(&selection.model));
    let model = selection.model;
    let conn_id = selection.connection;
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
        // Point the workspace-aware MCP server (codegraph) at this session's workspace
        // before snapshotting tools, so its code graph reflects the active checkout
        // (#548 W1b). Idempotent; restarts codegraph only when the path changed.
        let session_root = state.session_root(&sid);
        state.align_session_mcp(&sid, &session_root).await;
        // Re-aim the git HEAD watcher at this session's checkout (#561 BE half),
        // mirroring the codegraph alignment above. Idempotent; a no-op when the path
        // is unchanged. Drives the live `workspace:branch-changed` event the FE
        // listener (PR #581) patches into the composer chip.
        state.align_git_watcher(&session_root);
        // Snapshot built-in + MCP-bridged tools for this turn (RFC 0003 §6).
        let registry = state.build_tool_registry(&session_root);
        // Snapshot the advertised-tools matrix for this turn (#702) so the model
        // sees a stable tool list. The approval gate reads the matrix live (see
        // `UiApprover::approve`), so Control-panel edits still take effect on the
        // next tool call — only the advertised set is turn-bounded.
        let permission_matrix = state.permission_matrix();
        let mut tool_ctx = ToolContext::new(
            &registry,
            &session_root,
            &approver,
            max_iterations,
            &permission_matrix,
        );
        tool_ctx.mode = mode;
        tool_ctx.abstractive = crate::state::abstractive_config_from_env();
        tool_ctx.compaction_model = state.compaction_model(&conn_id);
        tool_ctx.compaction_budget = state.compaction_budget(&conn_id);
        tool_ctx.compaction_cache = Some(&state.compaction_cache);
        // Skills + ambient context for this turn (RFC 0001 §4, RFC 0002 phase 1):
        // the resolved persona, installed-skill descriptions, the bodies of the
        // active skills, and the current local time.
        let skills = state.skills_snapshot();
        let user_ctx =
            ff_agent::UserContext::now().with_working_dir(session_root.display().to_string());
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
            None,
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

        // Prime the compaction budget against the *served* context window (#612).
        // For Ollama this is the already-cached `/api/ps` probe (the same value
        // the model chip displays); other providers no-op. Before the model is
        // resident `/api/ps` reports nothing, so this falls to the conservative
        // default -- a safe under-fill -- and picks up the real window once loaded.
        provider.set_context_budget(state.served_window(&sid).await.window);

        let thinking = state.provider_config().thinking;
        let reasoning_visibility = state.provider_config().reasoning_visibility;
        let result = run_turn(
            provider.as_ref(),
            state.store.as_ref(),
            &tool_ctx,
            &sid,
            &model,
            Some(system_prompt.as_str()),
            thinking,
            reasoning_visibility,
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
            // F1b fields are Option on the wire (#475 follow-up); the desktop
            // always populates them.
            prefill_estimates: Some(prefill_estimates),
            tier1_fires: Some(tier1_fires),
            tier2_fires: Some(tier2_fires),
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
            // LLM-summarized session title (#671 item 2b): after the first turn,
            // replace the heuristic first-message title with a one-line summary and
            // announce it so the sidebar re-titles in place. Best-effort — gated to
            // fire once per session, and a failure/timeout leaves the heuristic title.
            if let Some(title) = state
                .generate_session_title(provider.as_ref(), &sid, &model, cancel_probe.clone())
                .await
            {
                state.store.set_title(&sid, title.clone());
                let _ = app.emit(
                    "session:title-updated",
                    SessionTitleUpdatedEvent {
                        session_id: sid.clone(),
                        title,
                    },
                );
            }
            state
                .maybe_flush_memory(provider.as_ref(), &registry, &sid, &model, cancel_probe)
                .await;
        }
    });
}

/// Wall-clock ceiling on a single goal iteration's turn, mirroring
/// `SCHEDULED_FIRE_TIMEOUT`: a stuck provider must not wedge the loop. On
/// timeout the turn is cancelled and the iteration is recorded as failed.
const GOAL_ITERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// The neutral continuation nudge seeded as the user turn when a goal iteration
/// has no pending steer (#778). It deliberately does NOT repeat the objective:
/// the system-prompt goal block (#718, `ff_agent::system_prompt`) already carries
/// the objective, progress ledger, and the `goal_complete` instruction, so
/// inlining the objective here would duplicate it every iteration (and could
/// drift from the sysprompt wording). Keep this generic — it only tells the agent
/// to take the next step against the goal already described in its instructions.
const GOAL_CONTINUE_NUDGE: &str =
    "Continue toward the goal described in your instructions. Take the next \
     concrete step, or call the `goal_complete` tool if it is fully met and verified.";

/// The host-side [`GoalIteration`] (RFC 0020 §5.2, #716): drives one headless
/// agent turn toward the objective, mirroring the scheduled runner's `fire` but
/// against the live session with the interactive [`UiApprover`] — so a mid-loop
/// approval / `ask_user` still surfaces in the UI (the RFC 0017 join point).
/// Per-tool safety gating already happens inside `run_turn` via the shared
/// `permission_matrix`; the loop-level [`GoalIteration::gate`] is a coarse
/// per-iteration pre-flight (#719/#682) that halts the *whole* loop when the
/// active mode's matrix posture says an autonomous iteration shouldn't run
/// unattended.
struct GoalLoopIteration {
    state: Arc<AppState>,
    app: tauri::AppHandle,
    session_id: String,
}

/// Coarse per-iteration goal gate (#719): decide whether to spend another
/// unattended iteration given the active `mode`'s permission-matrix posture.
///
/// The loop is headless-autonomous, so we gate on the `Sensitive` tier — the
/// representative "externally-visible autonomous work" a goal iteration is
/// expected to do (network egress, sub-agent spawn; RFC 0019 §Safety). Per-tool
/// gating (including `Dangerous`→deny and the Ask *prompt*) still runs inside the
/// turn via the shared matrix; this pre-flight only governs loop continuation:
/// - `Allow`  → [`GateDecision::Proceed`] (e.g. Act: run autonomously).
/// - `Ask`    → [`GateDecision::Pause`]  (e.g. Auto: pause & surface, resumable —
///   an unattended loop must not silently auto-approve a stream of Ask calls).
/// - `Deny`   → [`GateDecision::Deny`]   (a matrix edited to deny Sensitive in
///   this mode -- no default mode does, post-#793 -- halts the loop).
fn goal_gate_for(mode: Mode, matrix: &ff_core::PermissionMatrix) -> GateDecision {
    match matrix.cell(mode, ff_core::Safety::Sensitive) {
        PermissionCell::Allow => GateDecision::Proceed,
        PermissionCell::Ask => GateDecision::Pause,
        PermissionCell::Deny => GateDecision::Deny,
    }
}

#[async_trait::async_trait]
impl GoalIteration for GoalLoopIteration {
    fn gate(&self, _goal: &Goal) -> GateDecision {
        // Coarse loop-continuation gate keyed on the active mode's matrix posture
        // for the Sensitive tier (#719). Per-tool matrix gating — including the
        // Ask *prompt* and Dangerous deny — is still enforced inside the turn via
        // the shared `permission_matrix`; a paused/denied goal is checkpointed by
        // `drive_goal` from the returned decision. Read live so a Control-panel
        // matrix edit takes effect on the next boundary (#702/#742).
        let mode = self.state.session_mode(&self.session_id);
        goal_gate_for(mode, &self.state.permission_matrix())
    }

    async fn run_once(&self, goal: &Goal) -> IterationOutcome {
        // Seed the continuation turn. A pending steer (a message the user typed
        // while the goal ran) takes priority as the turn content; otherwise a
        // neutral "continue" nudge. The objective is NOT repeated here — the
        // system-prompt goal block (#718) already carries the objective,
        // progress, and the `goal_complete` instruction, so inlining it again
        // duplicates it every iteration and drifts if it were ever reworded
        // (#778). The steer is one-shot: the loop clears `pending_steer` on the
        // in-memory goal (via `steer_consumed`) before it checkpoints, so it is
        // applied once and not re-persisted next boundary (#753 review nit 1).
        let steer = goal
            .pending_steer
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let steer_consumed = steer.is_some();
        let prompt = match steer {
            Some(s) => s.to_string(),
            None => GOAL_CONTINUE_NUDGE.to_string(),
        };
        self.state
            .store
            .add_message(&self.session_id, Role::User, prompt.clone());

        let sid = self.session_id.clone();
        let selection = self.state.resolve_model_selection(&sid);
        let (mut provider, model) = self
            .state
            .build_provider_for(Some(&selection.connection), Some(&selection.model));
        let pheno = self.state.session_phenotype(&sid);
        let persona = pheno.persona.clone();
        let mode = self.state.session_mode(&sid);
        let max_iterations = pheno
            .max_iterations
            .unwrap_or(ff_agent::DEFAULT_MAX_ITERATIONS);

        let session_root = self.state.session_root(&sid);
        self.state.align_session_mcp(&sid, &session_root).await;
        self.state.align_git_watcher(&session_root);
        let registry = self.state.build_tool_registry(&session_root);

        let approver = UiApprover {
            app: self.app.clone(),
            state: self.state.clone(),
            session_id: sid.clone(),
            mode,
        };
        // Snapshot the matrix for this turn (#702); see the other turn paths.
        let permission_matrix = self.state.permission_matrix();
        let mut tool_ctx = ff_agent::ToolContext::new(
            &registry,
            &session_root,
            &approver,
            max_iterations,
            &permission_matrix,
        );
        tool_ctx.mode = mode;
        tool_ctx.abstractive = crate::state::abstractive_config_from_env();

        let skills = self.state.skills_snapshot();
        let user_ctx =
            ff_agent::UserContext::now().with_working_dir(session_root.display().to_string());
        let active: Vec<String> = self.state.turn_active_skills(&sid);
        let (memory, _ambient_keys) = self
            .state
            .memory()
            .ambient_block_filtered_keyed(self.state.index().as_ref());
        let system_prompt = ff_agent::build_system_prompt(
            persona.as_deref(),
            &skills,
            &active,
            &user_ctx,
            memory.as_deref(),
            Some(goal),
            mode,
        );

        provider.set_context_budget(self.state.served_window(&sid).await.window);

        let cancel = CancelToken::new();
        self.state.register_cancel(&sid, cancel.clone());
        let cancel_probe = cancel.clone();
        let thinking = self.state.provider_config().thinking;
        let reasoning_visibility = self.state.provider_config().reasoning_visibility;

        // Capture per-turn tokens (from `AgentEvent::Done`) and whether the agent
        // called `goal_complete` (from `AgentEvent::ToolCallFinished`) directly in
        // the event closure, so the loop gets both without re-reading the store.
        let tokens = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // `ToolCallFinished` carries only the call_id, so remember the call_id of
        // a started `goal_complete` and confirm it on a successful finish.
        let gc_call_id = Arc::new(std::sync::Mutex::new(Option::<String>::None));
        let tokens_ev = tokens.clone();
        let completed_ev = completed.clone();
        let gc_call_ev = gc_call_id.clone();
        let app_ev = self.app.clone();
        let sid_ev = sid.clone();
        let turn_start = std::time::Instant::now();

        let turn = run_turn(
            provider.as_ref(),
            self.state.store.as_ref(),
            &tool_ctx,
            &sid,
            &model,
            Some(system_prompt.as_str()),
            thinking,
            reasoning_visibility,
            cancel.clone(),
            move |event| {
                match &event {
                    ff_agent::AgentEvent::Done {
                        token_count: Some(t),
                        ..
                    } => {
                        tokens_ev.store(*t as u64, std::sync::atomic::Ordering::SeqCst);
                    }
                    ff_agent::AgentEvent::ToolCallStarted { call_id, name, .. }
                        if name == ff_tools::GOAL_COMPLETE_TOOL_NAME =>
                    {
                        *gc_call_ev.lock().unwrap() = Some(call_id.clone());
                    }
                    ff_agent::AgentEvent::ToolCallFinished {
                        call_id,
                        success: true,
                        ..
                    } if gc_call_ev.lock().unwrap().as_deref() == Some(call_id.as_str()) => {
                        completed_ev.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    _ => {}
                }
                emit_agent_event(&app_ev, &sid_ev, event);
            },
        );
        let result = tokio::time::timeout(GOAL_ITERATION_TIMEOUT, turn).await;
        self.state.take_cancel_if(&sid, &cancel);

        // Distinguish a user Stop (cancel) — the goal should PAUSE resumably — from
        // an unrecoverable failure (timeout / provider error) — which FAILS it.
        // A timeout is not a user cancel: the user didn't stop it, so it fails.
        let user_cancelled = cancel_probe.is_cancelled();
        let (cancelled, failed) = match result {
            _ if user_cancelled => (true, false),
            Err(_elapsed) => {
                cancel.cancel();
                (false, true)
            }
            Ok(Err(_)) => (false, true),
            Ok(Ok(_)) => (false, false),
        };

        IterationOutcome {
            tokens: tokens.load(std::sync::atomic::Ordering::SeqCst),
            wall_ms: turn_start.elapsed().as_millis() as i64,
            goal_complete: completed.load(std::sync::atomic::Ordering::SeqCst),
            cancelled,
            failed,
            steer_consumed,
        }
    }

    fn save(&self, goal: &Goal) {
        if let Err(e) = self.state.goals.save(goal) {
            tracing::warn!(error = %e, session = %self.session_id, "failed to persist goal checkpoint");
        }
        let _ = self.app.emit("goal:updated", goal);
    }

    fn now_ms(&self) -> i64 {
        now_ms()
    }
}

/// Stop a running goal loop for a session and wait (bounded) for it to fully
/// exit before returning (#753 review blocker 2). Cancels the in-flight turn so
/// `drive_goal` breaks at its next boundary, then polls the single-flight slot
/// until it clears (or a short timeout). Callers that overwrite goal state
/// (`goal_set`) must await this first so the old loop's final checkpoint can't
/// race the new goal. Safe to call when no loop is running (returns promptly).
async fn stop_goal_loop(state: &Arc<AppState>, session_id: &str) {
    if !state.goal_loop_running(session_id) {
        return;
    }
    if let Some(token) = state.take_cancel(session_id) {
        token.cancel();
    }
    state.cancel_pending_approvals(session_id);
    // Bounded wait: the loop clears its slot on the next boundary after the
    // cancelled turn returns. Cap so a wedged provider can't hang goal_set.
    for _ in 0..600 {
        if !state.goal_loop_running(session_id) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    tracing::warn!(session = %session_id, "goal loop did not stop within timeout; proceeding");
}

/// Spawn the self-continue loop for a session's active goal (RFC 0020 §5). Loads
/// the goal and drives [`drive_goal`] to a terminal state on a background task.
/// Single-flight (#753 review): claims the session's goal-loop slot first and
/// refuses to spawn if a loop is already running, so `goal_set` / `goal_resume`
/// can never stack two loops racing the same transcript. The slot is released on
/// any terminal stop (including an early return / panic) via a drop guard.
fn spawn_goal_loop(state: Arc<AppState>, app: tauri::AppHandle, session_id: String) {
    if !state.try_start_goal_loop(&session_id) {
        tracing::debug!(session = %session_id, "goal loop already running; not spawning another");
        return;
    }
    tauri::async_runtime::spawn(async move {
        // Release the single-flight slot no matter how the task exits.
        struct LoopGuard {
            state: Arc<AppState>,
            session_id: String,
        }
        impl Drop for LoopGuard {
            fn drop(&mut self) {
                self.state.end_goal_loop(&self.session_id);
            }
        }
        let _guard = LoopGuard {
            state: state.clone(),
            session_id: session_id.clone(),
        };

        let mut goal = match state.goals.load(&session_id) {
            Ok(Some(g)) => g,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e, session = %session_id, "goal loop: cannot load goal");
                return;
            }
        };
        if goal.status != GoalStatus::Active {
            return;
        }
        let iter = GoalLoopIteration {
            state: state.clone(),
            app: app.clone(),
            session_id: session_id.clone(),
        };
        let stop = drive_goal(&mut goal, &iter).await;
        tracing::info!(session = %session_id, ?stop, "goal loop finished");
    });
}

/// bounded single `run_turn`; if it overruns it is cancelled and recorded
/// `cancelled`, so one hung fire cannot starve later due tasks.
const SCHEDULED_FIRE_TIMEOUT: Duration = Duration::from_secs(600);

/// Fires a due scheduled task as a headless agent turn (RFC 0017 §4). Mirrors
/// `spawn_assistant_turn`: create a session, bind the task's workspace + profile,
/// resolve the provider + phenotype, run one bounded `run_turn` under a
/// `ScheduledApprover` at the task's safety ceiling, and map the result to a
/// terminal `RunStatus`. Lives here (not in `ff-scheduled`) so that crate stays
/// Tauri-free; this is the host-supplied `TaskRunner`.
struct DesktopTaskRunner {
    state: Arc<AppState>,
    app: tauri::AppHandle,
}

impl DesktopTaskRunner {
    /// Run a built-in action directly, mapping its result to a terminal
    /// `RunStatus` (#544). A built-in has no free-text prompt and no agent loop,
    /// so it never creates a session (`session_id: None`).
    async fn fire_builtin(&self, action: ff_core::BuiltinAction) -> RunStatus {
        match action {
            ff_core::BuiltinAction::MemoryConsolidate => {
                let memory = self.state.memory();
                if !memory.is_enabled() {
                    // A disabled memory store has nothing to organize; the fire
                    // succeeded as a no-op rather than failing.
                    return RunStatus::Ok;
                }
                let index = self.state.index();
                // Both `consolidate` (file rewrite) and `reindex` (a possible
                // blocking embed HTTP call) are sync/blocking, so run them off the
                // async worker — mirrors `MemoryConsolidateTool::run`.
                let result = tokio::task::spawn_blocking(move || {
                    let report =
                        memory.consolidate(&ff_memory::RecencyFrequencySalience::default())?;
                    if report.ran {
                        // Best-effort reindex; a recall-cache failure must not fail
                        // the consolidation pass itself.
                        let _ = index.reindex(&memory.all_chunks());
                    }
                    ff_memory::Result::Ok(())
                })
                .await;
                match result {
                    Ok(Ok(())) => RunStatus::Ok,
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "scheduled builtin: memory_consolidate failed");
                        RunStatus::Error
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "scheduled builtin: consolidate task panicked");
                        RunStatus::Error
                    }
                }
            }
        }
    }
}

#[async_trait]
impl ff_scheduled::TaskRunner for DesktopTaskRunner {
    async fn fire(&self, task: &ScheduledTask) -> ff_scheduled::RunOutcome {
        // A built-in runs it's named action directly (no agent loop / session);
        // a prompt task drives a headless `run_turn` below (#544).
        let prompt = match &task.kind {
            TaskKind::Prompt(text) => text.clone(),
            TaskKind::Builtin(action) => {
                let status = self.fire_builtin(*action).await;
                return ff_scheduled::RunOutcome {
                    session_id: None,
                    status,
                };
            }
        };

        // Create the fire's session and bind the task's workspace + profile so the
        // turn runs in the intended checkout under the intended persona. A stale
        // profile name fails the fire (`error`), surfaced on the run record.
        let session = self.state.store.create_session(Some(task.name.clone()));
        let sid = session.id;
        if let Some(ws) = &task.workspace {
            self.state
                .store
                .set_session_workspace(&sid, Some(ws.clone()));
        }
        if let Some(profile) = &task.profile {
            if let Err(e) = self
                .state
                .set_session_phenotype(&sid, Some(profile.clone()))
            {
                tracing::warn!(task = %task.id, error = %e, "scheduled fire: unresolved profile");
                return ff_scheduled::RunOutcome {
                    session_id: Some(sid),
                    status: RunStatus::Error,
                };
            }
        }
        self.state.store.add_message(&sid, Role::User, prompt);

        // The safety ceiling maps to the run mode: a read-only task runs in Plan
        // (read-capable tools advertised; the headless approver auto-runs ReadOnly
        // and denies Write/Sensitive/Dangerous), a write task in Act (write tools
        // advertised; the approver allows Write and denies Dangerous).
        let mode = match task.safety_ceiling {
            ff_core::SafetyCeiling::ReadOnly => Mode::Plan,
            ff_core::SafetyCeiling::Write => Mode::Act,
        };

        let pheno = self.state.session_phenotype(&sid);
        let selection = self.state.resolve_model_selection(&sid);
        let (mut provider, _) = self
            .state
            .build_provider_for(Some(&selection.connection), Some(&selection.model));
        let model = selection.model;
        let max_iterations = pheno
            .max_iterations
            .unwrap_or(ff_agent::DEFAULT_MAX_ITERATIONS);

        let session_root = self.state.session_root(&sid);
        self.state.align_session_mcp(&sid, &session_root).await;
        // Keep the git HEAD watcher aimed at the active checkout here too (#561), so
        // a scheduled-task turn that switches branches live-updates the FE chip.
        self.state.align_git_watcher(&session_root);
        let registry = self.state.build_tool_registry(&session_root);
        let approver = ScheduledApprover::new(task.safety_ceiling);
        // Snapshot the matrix for this turn (#702); see the interactive path above.
        let permission_matrix = self.state.permission_matrix();
        let mut tool_ctx = ToolContext::new(
            &registry,
            &session_root,
            &approver,
            max_iterations,
            &permission_matrix,
        );
        tool_ctx.mode = mode;
        tool_ctx.abstractive = crate::state::abstractive_config_from_env();
        tool_ctx.compaction_model = self.state.compaction_model(&selection.connection);
        tool_ctx.compaction_budget = self.state.compaction_budget(&selection.connection);
        tool_ctx.compaction_cache = Some(&self.state.compaction_cache);

        let skills = self.state.skills_snapshot();
        let user_ctx =
            ff_agent::UserContext::now().with_working_dir(session_root.display().to_string());
        let active: Vec<String> = self.state.turn_active_skills(&sid);
        let (memory, _ambient_keys) = self
            .state
            .memory()
            .ambient_block_filtered_keyed(self.state.index().as_ref());
        let system_prompt = ff_agent::build_system_prompt(
            pheno.persona.as_deref(),
            &skills,
            &active,
            &user_ctx,
            memory.as_deref(),
            None,
            mode,
        );

        // Prime the compaction budget against the served context window (#612),
        // mirroring the interactive `send_message` path; Ollama uses the cached
        // `/api/ps` probe, other providers no-op.
        provider.set_context_budget(self.state.served_window(&sid).await.window);

        let cancel = CancelToken::new();
        let thinking = self.state.provider_config().thinking;
        let reasoning_visibility = self.state.provider_config().reasoning_visibility;
        let app = self.app.clone();
        let sid_for_events = sid.clone();
        let turn = run_turn(
            provider.as_ref(),
            self.state.store.as_ref(),
            &tool_ctx,
            &sid,
            &model,
            Some(system_prompt.as_str()),
            thinking,
            reasoning_visibility,
            cancel.clone(),
            move |event| emit_agent_event(&app, &sid_for_events, event),
        );
        let result = tokio::time::timeout(SCHEDULED_FIRE_TIMEOUT, turn).await;

        // Outcome precedence (RFC 0017 §8.4): an ask_user dismissal surfaces as
        // needs_attention regardless of how the rest of the turn ended; else a
        // timeout is a cancellation; else a run_turn error; else ok (a denied
        // write within an otherwise-complete run is still ok).
        let status = if approver.needs_attention() {
            RunStatus::NeedsAttention
        } else {
            match result {
                Err(_elapsed) => {
                    cancel.cancel();
                    RunStatus::Cancelled
                }
                Ok(Err(_)) => RunStatus::Error,
                Ok(Ok(_)) => RunStatus::Ok,
            }
        };

        ff_scheduled::RunOutcome {
            session_id: Some(sid),
            status,
        }
    }
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
    // Invalidate the cross-turn summary cache (#757): the transcript is being
    // truncated, so the old boundary is no longer valid.
    state.compaction_cache.invalidate(&session_id);

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
        reasoning_visibility: current.reasoning_visibility,
        // No warmup control on this shim either; preserve the persisted value.
        warmup_enabled: current.warmup_enabled,
        // No num_ctx control on this shim; preserve the persisted window (#651).
        num_ctx: current.num_ctx,
    };
    state.set_provider_config(config.clone());
    // Model/provider changed — invalidate all cached summaries (#757) since
    // a summary generated by the old model may be incoherent for the new one.
    state.compaction_cache.invalidate_all();
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
    let result = state.upsert_connection(conn);
    // Connection model/endpoint may have changed — invalidate summaries (#757).
    state.compaction_cache.invalidate_all();
    Ok(result)
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

/// The Control-panel settings blob (#147). Opaque JSON round-tripped verbatim;
/// the frontend (`lib/control.ts`) owns the shape. Returns the factory default on
/// first load.
#[tauri::command]
fn get_control_config(state: State<'_, Arc<AppState>>) -> serde_json::Value {
    state.control_config()
}

/// Persist the Control-panel settings blob and echo back the stored value so the
/// UI can confirm the applied state (mirrors `set_search_config`).
#[tauri::command]
fn set_control_config(
    state: State<'_, Arc<AppState>>,
    config: serde_json::Value,
) -> serde_json::Value {
    state.set_control_config(config)
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
    let (provider, _model) = state.build_provider_for(id.as_deref(), None);
    Ok(provider.list_models().await.unwrap_or_default())
}

/// Probe a connection for the settings "Test Connection" button. `id` defaults to
/// the active connection. Returns `Ok(())` on a successful round-trip, or an
/// `Err(String)` message the UI can show. Unlike `list_models`, the error is
/// surfaced (the button reports failure) rather than swallowed.
#[tauri::command]
async fn test_connection(state: State<'_, Arc<AppState>>, id: Option<String>) -> CmdResult<()> {
    let (provider, model) = state.build_provider_for(id.as_deref(), None);
    provider
        .test_connection(&model)
        .await
        .map_err(|e| e.to_string())
}

/// Wake the configured model server so its GPU/compute pipelines are hot before
/// the user's first message. The composer fires this (debounced) when it gains
/// focus. Fully best-effort: any failure (server down, busy) is swallowed so
/// warmup never blocks the UI or surfaces an error.
///
/// Gated to local kinds with warmup enabled (#61): warming a *hosted* endpoint
/// would fire a billed request on every composer focus, and a user who disabled
/// warmup (e.g. on laptop battery) must not pay for it. The frontend also gates
/// the nudge, so this is defense in depth.
#[tauri::command]
async fn warmup(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    let cfg = state.provider_config();
    if !should_warmup(cfg.kind, cfg.warmup_enabled) {
        return Ok(());
    }
    let (provider, model) = state.build_provider();
    let _ = provider.warmup(&model).await;
    Ok(())
}

/// Whether the composer warmup nudge should fire for a connection: only for
/// local backends (#61) with warmup enabled. Pure so it is unit-testable.
fn should_warmup(kind: ProviderKind, warmup_enabled: bool) -> bool {
    kind.is_local() && warmup_enabled
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

/// Persist an edited phenotype to disk (RFC 0005 Phase D / #525). Accepts the whole
/// `Phenotype` (lossless read-modify-write). The built-in `default` is immutable and
/// rejected. When the edited phenotype is the one currently active, its skills and
/// overrides are re-applied immediately and `skills:changed` is emitted so the FE
/// active set updates. Returns the saved phenotype.
#[tauri::command]
fn update_phenotype(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    phenotype: Phenotype,
) -> CmdResult<Phenotype> {
    let pheno = state.update_phenotype(phenotype)?;
    if pheno.name == state.active_phenotype().name {
        emit_skills_changed(&app, &state);
        emit_pheno_mcp_unavailable(&app, &state, &pheno.name);
    }
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
/// the global default (#265). Per-pane, like `set_session_phenotype`. When the
/// resolved mode actually changes, a System-role message is injected into the
/// transcript so the model sees an in-context signal (#828) — breaking behavioral
/// inertia from prior assistant turns that referenced the old mode.
#[tauri::command]
fn set_session_mode(state: State<'_, Arc<AppState>>, session_id: String, mode: Option<Mode>) {
    let old = state.session_mode(&session_id);
    state.set_session_mode(&session_id, mode);
    let new = state.session_mode(&session_id);
    if old != new {
        let label = match new {
            Mode::Plan => "Plan. Read-only tools only; writes are denied.",
            Mode::Auto => "Auto. Writes auto-approved; sensitive actions prompt.",
            Mode::Act => "Act. Full tool access enabled.",
        };
        state.store.add_message(
            &session_id,
            Role::System,
            format!("[Mode switched to {label}]"),
        );
    }
}

/// Pin a single session's model, or clear it (`selection: None`) so it inherits its
/// phenotype's model again (RFC 0005 §11 Phase D, #499). Per-pane, like
/// `set_session_phenotype`. Errors on an unknown connection id so a stale UI cannot
/// pin a phantom endpoint. The FE per-pane model chip (#523) calls this.
#[tauri::command]
fn set_session_model_selection(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    selection: Option<ModelSelection>,
) -> CmdResult<()> {
    state.set_session_model(&session_id, selection)?;
    Ok(())
}

/// A session's explicit per-pane model selection, or `None` if it inherits its
/// phenotype's model (#499). The raw binding, *not* the resolved selection -- use
/// `resolve_model_selection` for what a turn actually runs on.
#[tauri::command]
fn get_session_model_selection(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Option<ModelSelection> {
    state.session_model(&session_id)
}

/// The resolved `(connection, model)` a turn for this session runs on, applying the
/// RFC 0005 §11.2 three-tier precedence session > phenotype > global (#499). The FE
/// renders this on the per-pane model chip so the displayed model matches what the
/// next turn will actually use.
#[tauri::command]
async fn resolve_model_selection(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<ResolvedModel, String> {
    let mut resolved = state.resolve_model_selection(&session_id);
    // Fold the async-only served-window probe onto the sync resolver's output
    // (#602). Cast u64 -> u32 so the ts-rs binding stays `number` (u64 maps to
    // `bigint`, which the FE `ServedWindow.window: number` cannot consume); a
    // window above `u32::MAX` (~4.29B tokens) is absurd, so try_from is just a
    // safety net rather than a real constraint. The `Result` wrapper is required
    // for async tauri commands with reference inputs; we never actually err.
    let probe = state.served_window(&session_id).await;
    resolved.context_window = probe.window.and_then(|n| u32::try_from(n).ok());
    resolved.trained_context_window = probe.trained.and_then(|n| u32::try_from(n).ok());
    resolved.context_window_source = probe.source;
    // Ollama reports vision capability via `/api/show`; OR it onto the name-based
    // gate so a daemon-reported multimodal model (e.g. a qwen3-vl MoE) isn't
    // blocked by a stale name allow-list (#625). `None`/`Some(false)` leave the
    // name-based result untouched.
    resolved.supports_vision = resolved.supports_vision || probe.supports_vision.unwrap_or(false);
    Ok(resolved)
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

/// The Control-panel view of the permission matrix (#702, RFC 0019 §3): every
/// Mode × Safety cell as a flat, self-describing list.
#[tauri::command]
fn get_permission_matrix(state: State<'_, Arc<AppState>>) -> PermissionMatrixView {
    state.permission_matrix_view()
}

/// Edit and persist a single matrix cell (#702), returning the updated view. Takes
/// effect on the next tool invocation: the approval gate reads the matrix live, and
/// the advertised-tools gate picks it up on the next turn.
#[tauri::command]
fn set_permission_cell(
    state: State<'_, Arc<AppState>>,
    mode: Mode,
    safety: Safety,
    cell: PermissionCell,
) -> PermissionMatrixView {
    state.set_permission_cell(mode, safety, cell)
}

/// Set and persist a per-tool override (#700/#702), returning the updated view.
/// The override replaces the matrix cell for the named tool across every
/// Mode × Safety combination; it is read live by the approval gate.
#[tauri::command]
fn set_tool_override(
    state: State<'_, Arc<AppState>>,
    tool: String,
    cell: PermissionCell,
) -> PermissionMatrixView {
    state.set_tool_override(tool, cell)
}

/// Remove a per-tool override (#700/#702), returning the updated view.
#[tauri::command]
fn remove_tool_override(state: State<'_, Arc<AppState>>, tool: String) -> PermissionMatrixView {
    state.remove_tool_override(&tool)
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
        scope: def.scope,
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
/// Whether the `localUpdateChannel` experimental flag is active for this session.
/// Set by [`start_dev_update_watcher`] (called by the FE exactly when the flag is on).
static LOCAL_UPDATE_CHANNEL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Default port for the local dev-release HTTP server (matches `scripts/dev-release.sh`).
const DEV_RELEASE_PORT: u16 = 8787;

fn updater(app: &tauri::AppHandle) -> CmdResult<tauri_plugin_updater::Updater> {
    use tauri_plugin_updater::UpdaterExt;
    // Priority: explicit env var > localUpdateChannel flag > compiled-in default.
    if let Ok(endpoint) = std::env::var("FF_UPDATER_ENDPOINT") {
        let endpoint = url::Url::parse(&endpoint).map_err(|e| e.to_string())?;
        app.updater_builder()
            .endpoints(vec![endpoint])
            .map_err(|e| e.to_string())?
            .version_comparator(|current, update| update.version != current)
            .build()
            .map_err(|e| e.to_string())
    } else if LOCAL_UPDATE_CHANNEL.load(std::sync::atomic::Ordering::Relaxed) {
        let endpoint = url::Url::parse(&format!("http://localhost:{DEV_RELEASE_PORT}/latest.json"))
            .map_err(|e| e.to_string())?;
        app.updater_builder()
            .endpoints(vec![endpoint])
            .map_err(|e| e.to_string())?
            // Any different version is an update (dev timestamps are always unique).
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
    let progress_app = app.clone();
    let finish_app = app.clone();
    let mut downloaded: u64 = 0;
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ =
                    progress_app.emit("update:progress", UpdateProgressEvent { downloaded, total });
            },
            move || {
                let _ = finish_app.emit("update:download-finished", ());
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    app.restart();
}

/// Start the local dev-update file watcher (#705, Phase 2). Called by the FE on
/// boot when the `localUpdateChannel` experimental flag is on. The watcher observes
/// `~/.config/flowforge/dev-update/latest.json` via kqueue/inotify and emits
/// `update:local-feed-changed` instantly when the file is written, so the FE can
/// trigger an immediate `refresh()` without waiting for the 15s poll tick.
///
/// Idempotent: calling twice is a no-op (the watcher is stored in managed state and
/// lives for the app lifetime; there is no stop — it is zero-cost when idle).
#[tauri::command]
fn start_dev_update_watcher(app: tauri::AppHandle) {
    use std::sync::Once;
    LOCAL_UPDATE_CHANNEL.store(true, std::sync::atomic::Ordering::Relaxed);
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        match dev_update_watcher::DevUpdateWatcher::spawn() {
            Ok((_watcher, mut rx)) => {
                // Leak the watcher so it lives for the process lifetime (zero-cost
                // when idle; the OS kqueue fd stays open).
                std::mem::forget(_watcher);
                let emit_app = app.clone();
                tauri::async_runtime::spawn(async move {
                    while rx.recv().await.is_some() {
                        let _ = emit_app.emit("update:local-feed-changed", ());
                    }
                });
                tracing::info!("dev-update watcher started");
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to start dev-update watcher");
            }
        }
    });
}

/// Map an [`AgentEvent`] to its frontend Tauri event and emit it.
///
/// This is the single source of truth for the `AgentEvent → app.emit(…)` wire
/// mapping shared by both turn paths — the in-process `run_turn` closure (in
/// `send_message`) and the CLI sidecar loop below (`run_sidecar_turn`).
/// Keeping both paths on one helper is exactly what the sidecar parity
/// smoke-test (RFC 0004 §5) guards against drift: if the mapping changes, it
/// changes in one place.
///
/// Generic over `R: tauri::Runtime` so the sidecar parity integration test can
/// drive it through a `MockRuntime` app handle (see `tests::sidecar_turn_*`).
fn emit_agent_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    session_id: &str,
    event: AgentEvent,
) {
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
            stop_reason,
            ..
        } => {
            let _ = app.emit(
                "turn:done",
                TurnDoneEvent {
                    session_id: session_id.to_string(),
                    message_id,
                    token_count,
                    stop_reason,
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
        AgentEvent::ToolOutputChunk {
            message_id,
            call_id,
            stream,
            delta,
        } => {
            let stream = match stream {
                ff_tools::OutputStream::Stdout => OutputStreamKind::Stdout,
                ff_tools::OutputStream::Stderr => OutputStreamKind::Stderr,
            };
            let _ = app.emit(
                "tool:output",
                ToolOutputChunkEvent {
                    session_id: session_id.to_string(),
                    message_id,
                    call_id,
                    stream,
                    delta,
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
async fn run_sidecar_turn<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
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

/// Boot trace (#599 item 0): the FE reports first paint on the same `BOOT_T0`
/// clock. `fe_nav_ms` is the webview-internal `performance.now()` (≈ navigation
/// start → paint), which isolates the WKWebView/process floor from our own work.
#[tauri::command]
fn mark_fe_ready(phase: String, fe_nav_ms: f64) {
    boot_trace(
        &format!("fe.{phase}"),
        Some(&format!("fe_nav {fe_nav_ms:.1}ms")),
    );
}

/// Paint-first boot (#599): has the background hydrate task finished
/// `AppState::new()` and published the managed state yet? Reads a static flag
/// (no `AppState` dependency) so it is safe to call before the state is managed
/// — the FE uses it as the "subscribe-then-check" half of the `app:ready` gate,
/// closing the race where the event fired before its listener attached.
#[tauri::command]
fn is_app_ready() -> bool {
    APP_READY.load(Ordering::SeqCst)
}

// Boot finalization seam (#599 review nit): the three side effects of "hydrate
// done" — `manage(state)`, `APP_READY = true`, `emit("app:ready")` — must run in
// exactly that order, or the FE's subscribe-then-check gate silently breaks:
//   * `store` before `manage` → `is_app_ready()` is true while no state is
//     managed, so a command the FE fires on the gate reads an unresolved
//     `State<'_, Arc<AppState>>`.
//   * `emit` before `store` → the FE's `isAppReady()` poll (run on receiving
//     `app:ready`) returns false and the loading state hangs.
// The order was correct as written but nothing guarded it — a future reorder of
// those three lines would slip past review. `publish_app_ready` makes the order
// a single reviewable unit, and `BootFinalize` lets a test substitute a spy
// that asserts the order at call time (see `tests::boot_finalize_orders_*`).
trait BootFinalize {
    /// Publish the managed state to the command layer. Runs BEFORE the
    /// `APP_READY` flag flips.
    fn manage_state(&self, state: Arc<AppState>);
    /// Notify the FE the loading gate may drop. Runs AFTER the `APP_READY`
    /// flag flips AND after `manage_state`.
    fn emit_ready(&self);
}

/// Render a caught panic payload (`Box<dyn Any + Send>`) into a human-readable,
/// `context`-prefixed message. A panic payload is one of two common stringy forms
/// (`&'static str` from `panic!("literal")`, `String` from `panic!("{fmt}")`) or,
/// rarely, something else; fall back to a generic note so the FE never gets an
/// opaque "unknown" with no clue where to look. Shared by the detached MCP-init
/// task and the outer `post-init` `catch_unwind` so both format identically.
fn panic_message(context: &str, payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        format!("{context} panicked: {s}")
    } else if let Some(s) = payload.downcast_ref::<String>() {
        format!("{context} panicked: {s}")
    } else {
        format!("{context} panicked (non-string panic payload)")
    }
}

/// Run the boot-finalization sequence in the order the FE gate requires:
/// `manage` → `APP_READY = true` → `emit("app:ready")`. The only call site is
/// the hydrate task in `run()`; the order is the invariant guarded by
/// `tests::boot_finalize_orders_manage_before_flag_before_emit`.
fn publish_app_ready<F: BootFinalize>(finalizer: &F, state: Arc<AppState>) {
    finalizer.manage_state(state);
    APP_READY.store(true, Ordering::SeqCst);
    boot_trace("app_ready", None);
    finalizer.emit_ready();
}

impl<R: tauri::Runtime> BootFinalize for tauri::AppHandle<R> {
    fn manage_state(&self, state: Arc<AppState>) {
        self.manage(state);
    }
    fn emit_ready(&self) {
        let _ = self.emit("app:ready", ());
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Boot-trace origin (#599 item 0): the earliest FlowForge-controlled point.
    // OS process spawn + WKWebView init happen before any of our code and are
    // unmeasurable from here (the platform floor the issue calls out); the FE's
    // `performance.now()` in `mark_fe_ready` gives the closest proxy for that.
    let _ = BOOT_T0.set(std::time::Instant::now());
    // Paint-first boot (#599): `AppState::new()` and the supervisor / watcher /
    // reaper / scheduler wiring are deferred to a background hydrate task (spawned
    // from `setup` below) so the window is created and painted FIRST — the FE
    // renders a loading state until the `app:ready` event (mirrors the
    // `mcp:status-changed` / `workspace:branch-changed` hydrate pattern) and the
    // `is_app_ready` flag. This is a reordering, not new logic: the same work runs,
    // just off the synchronous pre-window path.
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        // Boot trace (#599 item 0): stamp when the webview begins loading our
        // HTML (webview process up) and when the document finishes loading. These
        // now land BEFORE `app_state_new` — the win this reordering buys.
        .on_page_load(|_webview, payload| match payload.event() {
            tauri::webview::PageLoadEvent::Started => boot_trace("webview.page-load-started", None),
            tauri::webview::PageLoadEvent::Finished => {
                boot_trace("webview.page-load-finished", None)
            }
        })
        .setup(move |app| {
            // Spawn the heavy init off the synchronous pre-window path. `setup`
            // returns immediately, the window paints, and this task runs to
            // completion before publishing the state and emitting `app:ready`.
            //
            // `AppState::new()` is sync blocking I/O (SQLite opens, skill scans,
            // the FTS5 index build, fs seed); `spawn_blocking` runs it on the
            // blocking pool so it never stalls an async worker, and — like the
            // main-thread path it replaces — needs no entered reactor (the skill /
            // memory / git watchers are `std::thread`, not `tokio::spawn`).
            // `init_mcp` / `start_process_reaper` enter the runtime themselves
            // (issue #117), so they are safe to call from this async task too.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = match tokio::task::spawn_blocking(AppState::new).await {
                    Ok(s) => Arc::new(s),
                    Err(e) => {
                        let msg = format!("AppState::new() panicked: {e}");
                        tracing::error!("{msg}");
                        let _ = app_handle.emit("app:init-error", &msg);
                        return;
                    }
                };
                // Cumulative since t0 ≈ the blocking `AppState::new()` cost (now
                // post-paint); per-step durations are emitted from `state.rs`
                // (registry/seed/skills/memory/DBs).
                boot_trace("app_state_new", None);

                // Guard the post-spawn_blocking body so no panic goes unlogged.
                // This task's `JoinHandle` is dropped, so a panic in
                // `init_git_watcher` / `start_process_reaper` / the scheduler
                // wiring would be silently discarded by Tokio — leaving
                // `APP_READY` false and the FE on a dead `<BootSplash>` spinner
                // with no clue where it stopped (regression: previously this ran
                // on the main thread, where a panic was a visible crash). Catch
                // the unwind, log it, and emit `app:init-error` (the same wire the
                // `AppState::new()` panic path above uses) so the FE can surface an
                // actionable error. `error_handle` stays outside the closure so it
                // survives the panic to emit. (The detached MCP-init task below
                // guards its own body the same way — the outer catch covers only
                // its `spawn` call, not the task's later execution.)
                let error_handle = app_handle.clone();
                let post_init = move || {
                    // MCP servers are not needed for first paint (RFC 0003; the
                    // composer works with zero tools, and `list_mcp_servers`
                    // returns empty-not-error before the host is up), so init runs
                    // in its OWN spawned task rather than inline here — keeping the
                    // synchronous `mcp.json` read + OS watcher registration inside
                    // `init_mcp` off the path to `publish_app_ready` (#599 item 5).
                    // `init_mcp` enters the shared Tokio runtime itself, so it's
                    // safe from this task (issue #117). The `mcp:status-changed`
                    // forwarder stays in the SAME task, sequenced AFTER `init_mcp`
                    // returns, so `mcp_handle()` is populated before it is read —
                    // no status updates are lost to a race. This task is detached,
                    // so guard its body: a panic here would otherwise be discarded
                    // by Tokio (the outer `catch_unwind` covers only the spawn).
                    {
                        let state = state.clone();
                        let app_handle = app_handle.clone();
                        tauri::async_runtime::spawn(async move {
                            // `&AppState` is not `UnwindSafe`, so assert it across
                            // the catch boundary (the state is only read here).
                            let guarded = AssertUnwindSafe(|| state.init_mcp());
                            if let Err(payload) = catch_unwind(guarded) {
                                let msg = panic_message("mcp init", &payload);
                                tracing::error!("{msg}");
                                let _ = app_handle.emit("app:init-error", &msg);
                                return;
                            }
                            // Forward supervisor status changes to the FE as
                            // `mcp:status-changed`, so the servers panel reflects
                            // start/stop/restart/health without polling. The watch
                            // tick coalesces; we re-snapshot on each wake.
                            if let Some(handle) = state.mcp_handle() {
                                let mut rx = handle.status_changed_rx();
                                while rx.changed().await.is_ok() {
                                    let servers = handle.status_snapshot();
                                    let _ = app_handle.emit(
                                        "mcp:status-changed",
                                        McpStatusChangedEvent { servers },
                                    );
                                }
                            }
                        });
                    }
                    // Live-sync the active session's git branch (#561 BE half): the
                    // GitHeadWatcher observes the workspace's `.git/HEAD` and emits
                    // `workspace:branch-changed` on a real branch change, which the FE
                    // listener merged in PR #581 patches into the composer chip with no
                    // remount. A `None` rx means the watcher could not start -- live sync
                    // degrades to off rather than failing the app.
                    if let Some(mut rx) = state.init_git_watcher() {
                        let app_handle = app_handle.clone();
                        tauri::async_runtime::spawn(async move {
                            while let Some(ws) = rx.recv().await {
                                let _ = app_handle.emit("workspace:branch-changed", ws);
                            }
                        });
                    }
                    // Drive the periodic idle-process reaper
                    // (`ProcessSupervisor::reap_idle`): finished processes and ones the
                    // agent abandoned (started but never polled again) are cleaned up on
                    // a timer. Enters the runtime itself, so it's safe here too.
                    state.start_process_reaper();
                    // Drive the scheduled-task firing engine (RFC 0017 section 4, #542):
                    // a background sweep fires due tasks through the desktop runner. The
                    // tick is coarse; the due predicate is minute-granular, so a 30s sweep
                    // never misses a slot and never double-fires (stamped last_run gates
                    // it). The sweep loop is inlined here rather than calling
                    // `ff_scheduled::spawn_scheduler` (which does its own internal
                    // `tokio::spawn`), so this single task owns it directly.
                    let scheduler_runner: Arc<dyn ff_scheduled::TaskRunner> =
                        Arc::new(DesktopTaskRunner {
                            state: state.clone(),
                            app: app_handle.clone(),
                        });
                    {
                        let sched_store = state.scheduled.clone();
                        let sched_app = app_handle.clone();
                        tauri::async_runtime::spawn(async move {
                            let mut interval = tokio::time::interval(Duration::from_secs(30));
                            loop {
                                interval.tick().await;
                                let fired = ff_scheduled::run_due_once(
                                    sched_store.as_ref(),
                                    scheduler_runner.as_ref(),
                                )
                                .await;
                                // Emit one `scheduled:fired` per appended run, plus a
                                // single `scheduled:changed` snapshot when anything fired,
                                // so the UI live-updates without polling (#544).
                                for run in &fired {
                                    emit_scheduled_fired(&sched_app, sched_store.as_ref(), run);
                                }
                            }
                        });
                    }

                    // Resume any goal that was `Active` when the app last closed
                    // (RFC 0020 §5.3, #802). `spawn_goal_loop` is single-flight
                    // guarded and re-checks `status`, so a racing IPC `goal_resume`
                    // can never double-spawn. This is best-effort and off the
                    // first-paint path (post_init already runs after `app:ready` is
                    // scheduled), so a slow or failed scan never blocks boot. The
                    // loops spawn before `publish_app_ready` (they hold their own
                    // `Arc<AppState>`, not the managed state), but the initial
                    // `goal:updated` emit is deferred until after it so the FE panel
                    // is already listening when the "running" snapshot arrives.
                    let resumed = state.goals.list_active();
                    for goal in &resumed {
                        spawn_goal_loop(state.clone(), app_handle.clone(), goal.session_id.clone());
                    }

                    // State is now usable: publish it to the command layer and notify
                    // the FE to drop its loading gate. Commands read
                    // `State<'_, Arc<AppState>>`, which only resolves once `manage` runs
                    // — the FE never invokes them before `app:ready` (gated by
                    // `is_app_ready`), so the pre-ready window sees no unmanaged-state
                    // error. `publish_app_ready` runs the three steps in the order the
                    // gate requires (see its invariant); doing it inline would let a
                    // future reorder break the gate silently.
                    publish_app_ready(&app_handle, state);

                    for goal in &resumed {
                        let _ = app_handle.emit("goal:updated", goal);
                    }
                };
                if let Err(payload) = catch_unwind(AssertUnwindSafe(post_init)) {
                    let msg = panic_message("post-init", &payload);
                    tracing::error!("{msg}");
                    let _ = error_handle.emit("app:init-error", &msg);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            create_session,
            list_sessions,
            get_messages,
            search_messages,
            search_in_session,
            export_session,
            rename_session,
            delete_session,
            fork_session,
            list_scheduled_tasks,
            create_scheduled_task,
            toggle_scheduled_task,
            delete_scheduled_task,
            run_scheduled_task_now,
            list_scheduled_runs,
            set_scheduled_paused_all,
            goal_set,
            goal_status,
            goal_pause,
            goal_resume,
            goal_clear,
            goal_complete,
            preview_cadence,
            get_session_workspace,
            set_session_workspace,
            list_branches,
            checkout_branch,
            list_memory_files,
            read_memory_file,
            memory_overview,
            list_memory_chunks,
            reset_memory_chunk,
            set_memory_chunk_pinned,
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
            get_control_config,
            set_control_config,
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
            update_phenotype,
            set_session_phenotype,
            set_session_mode,
            set_session_model_selection,
            get_session_model_selection,
            resolve_model_selection,
            get_default_mode,
            set_default_mode,
            get_permission_matrix,
            set_permission_cell,
            set_tool_override,
            remove_tool_override,
            list_mcp_servers,
            restart_mcp_server,
            set_mcp_server_enabled,
            add_mcp_server,
            remove_mcp_server,
            check_for_updates,
            install_update,
            start_dev_update_watcher,
            mark_fe_ready,
            is_app_ready,
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
    use super::state::AppState;
    use super::{
        emit_agent_event, git_branch, goal_gate_for, is_app_ready, list_local_branches,
        matrix_gate, panic_message, pre_prompt_decision, publish_app_ready, resolve_tool_arg,
        resolve_workspace_dir, run_sidecar_turn, should_warmup, switch_branch, BootFinalize,
        PrePromptDecision, TurnMetrics, UpdateStatus, APP_READY,
    };
    use ff_agent::{AgentEvent, GateDecision};
    use ff_core::events::TurnDoneEvent;
    use ff_core::{Mode, ProviderKind};
    use ff_tools::Safety;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    // `APP_READY` is a process-global static, and the boot-state tests below are
    // the only tests in the crate that touch it. Guard them with a mutex so the
    // two never race each other (parallel `cargo test` threads share the flag).
    static BOOT_TEST_LOCK: Mutex<()> = Mutex::new(());

    // #768 review B2: the scoped-rule arg table must read each tool's REAL
    // argument key, checked against the ff-tools schemas. A wrong key silently
    // resolves to `None`, so the rule never fires (fail-open for deny backstops).
    #[test]
    fn resolve_tool_arg_reads_real_schema_keys() {
        use serde_json::json;
        assert_eq!(
            resolve_tool_arg("bash", &json!({"command": "cargo build"})),
            Some("cargo build".into())
        );
        // python's key is `code`, NOT `command`.
        assert_eq!(
            resolve_tool_arg("python", &json!({"code": "print(1)"})),
            Some("print(1)".into())
        );
        assert_eq!(
            resolve_tool_arg("python", &json!({"command": "print(1)"})),
            None
        );
        for tool in ["view", "edit", "write"] {
            assert_eq!(
                resolve_tool_arg(tool, &json!({"path": "src/main.rs"})),
                Some("src/main.rs".into())
            );
        }
        // Read-only search tools short-circuit before approve() — not listed.
        assert_eq!(resolve_tool_arg("grep", &json!({"pattern": "x"})), None);
        assert_eq!(
            resolve_tool_arg("glob", &json!({"pattern": "**/*.rs"})),
            None
        );
        // Formerly-listed phantom keys resolve to nothing now.
        assert_eq!(resolve_tool_arg("rg", &json!({"path": "."})), None);
        assert_eq!(resolve_tool_arg("fd", &json!({"path": "."})), None);
    }

    // Spy that asserts the `manage` → `store` → `emit` ordering at call time: a
    // reordered `publish_app_ready` (e.g. `store` hoisted above `manage`, or
    // `emit` moved above `store`) trips the assert inside the offending method
    // and fails the test, instead of silently breaking the FE gate.
    #[derive(Default)]
    struct BootFinalizeSpy {
        managed: std::sync::atomic::AtomicBool,
        emitted: std::sync::atomic::AtomicBool,
    }

    impl BootFinalize for BootFinalizeSpy {
        fn manage_state(&self, _state: Arc<AppState>) {
            // `manage` runs BEFORE the flag flips — if it's already true here,
            // someone hoisted `APP_READY.store` above `manage` and the FE would
            // see `is_app_ready()==true` with no managed state.
            assert!(
                !APP_READY.load(Ordering::SeqCst),
                "invariant violation: manage_state ran after APP_READY was already true \
                 (store was reordered before manage)"
            );
            self.managed.store(true, Ordering::SeqCst);
        }
        fn emit_ready(&self) {
            // `emit` runs AFTER the flag flips AND after `manage` — if the flag
            // is false here, `emit` was moved before `store` (the FE's
            // post-event `isAppReady()` poll would then read false and hang); if
            // `manage` didn't run yet, `emit` was moved before `manage`.
            assert!(
                APP_READY.load(Ordering::SeqCst),
                "invariant violation: emit_ready ran before APP_READY was set \
                 (emit was reordered before store)"
            );
            assert!(
                self.managed.load(Ordering::SeqCst),
                "invariant violation: emit_ready ran before manage_state \
                 (emit was reordered before manage)"
            );
            self.emitted.store(true, Ordering::SeqCst);
        }
    }

    // #599 boot state machine: `is_app_ready()` reads a static flag flipped once
    // at the end of the hydrate task. Pin the false-then-true transition so a
    // future change that, say, reads the wrong flag or inverts the polarity is
    // caught here rather than in a manual cold-start check.
    #[test]
    fn is_app_ready_flips_false_to_true() {
        let _guard = BOOT_TEST_LOCK.lock().unwrap();
        APP_READY.store(false, Ordering::SeqCst);
        assert!(!is_app_ready(), "flag must read false before finalization");
        APP_READY.store(true, Ordering::SeqCst);
        assert!(
            is_app_ready(),
            "flag must read true after APP_READY.store(true)"
        );
        APP_READY.store(false, Ordering::SeqCst);
    }

    // #599 boot state machine: the manage→store→emit ordering is the invariant
    // `publish_app_ready` exists to encode. A spy that self-asserts at each call
    // catches every reorder that breaks the subscribe-then-check gate
    // (store-before-manage, emit-before-store, emit-before-manage), and the
    // post-call asserts catch a call being dropped entirely. A future reorder of
    // those three lines now fails this test instead of slipping past review.
    #[test]
    fn publish_app_ready_orders_manage_before_flag_before_emit() {
        let _guard = BOOT_TEST_LOCK.lock().unwrap();
        APP_READY.store(false, Ordering::SeqCst);

        let spy = BootFinalizeSpy::default();
        publish_app_ready(&spy, Arc::new(AppState::new()));

        assert!(
            is_app_ready(),
            "APP_READY must be true after publish_app_ready"
        );
        assert!(
            spy.managed.load(Ordering::SeqCst),
            "manage_state was never called"
        );
        assert!(
            spy.emitted.load(Ordering::SeqCst),
            "emit_ready was never called"
        );

        APP_READY.store(false, Ordering::SeqCst);
    }

    // `panic_message` renders both stringy panic-payload forms and the rare
    // non-string fallback with the caller's context prefix, so a detached-task
    // panic surfaces an actionable `app:init-error` rather than an opaque note.
    #[test]
    fn panic_message_formats_each_payload_form() {
        let literal: Box<dyn std::any::Any + Send> = Box::new("boom");
        assert_eq!(
            panic_message("mcp init", &literal),
            "mcp init panicked: boom"
        );

        let owned: Box<dyn std::any::Any + Send> = Box::new(String::from("kaboom"));
        assert_eq!(
            panic_message("post-init", &owned),
            "post-init panicked: kaboom"
        );

        let other: Box<dyn std::any::Any + Send> = Box::new(42u32);
        assert_eq!(
            panic_message("mcp init", &other),
            "mcp init panicked (non-string panic payload)"
        );
    }

    #[test]
    fn should_warmup_only_for_local_kinds_with_warmup_enabled() {
        assert!(should_warmup(ProviderKind::CandleVllm, true));
        assert!(should_warmup(ProviderKind::Ollama, true));
        // Disabled by the user -> no warmup even on a local kind.
        assert!(!should_warmup(ProviderKind::Ollama, false));
        // Hosted kinds never warm (would fire a billed request).
        assert!(!should_warmup(ProviderKind::OpenAi, true));
        assert!(!should_warmup(ProviderKind::Bedrock, true));
        assert!(!should_warmup(ProviderKind::SiliconFlow, true));
    }

    // `UpdateStatus` has no ts-rs binding -- it is cast on the FE side from the JSON
    // this serializes to (`lib/about.ts`). Pin the wire shape so the handwritten FE
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

    // Permission matrix correctness is tested in ff-core::permission::tests.
    // This spot-check confirms the matrix drives the approval path as expected.
    #[test]
    fn permission_matrix_default_matches_expected() {
        use ff_core::{PermissionCell, PermissionMatrix};
        let m = PermissionMatrix::default();
        // Auto: Write is auto-approved, Sensitive prompts, Dangerous denied.
        assert_eq!(m.cell(Mode::Auto, Safety::Write), PermissionCell::Allow);
        assert_eq!(m.cell(Mode::Auto, Safety::Sensitive), PermissionCell::Ask);
        assert_eq!(m.cell(Mode::Auto, Safety::Dangerous), PermissionCell::Deny);
        // Act: Write+Sensitive auto-approved, Dangerous prompts.
        assert_eq!(m.cell(Mode::Act, Safety::Write), PermissionCell::Allow);
        assert_eq!(m.cell(Mode::Act, Safety::Sensitive), PermissionCell::Allow);
        assert_eq!(m.cell(Mode::Act, Safety::Dangerous), PermissionCell::Ask);
        // Plan: only ReadOnly allowed.
        assert_eq!(m.cell(Mode::Plan, Safety::ReadOnly), PermissionCell::Allow);
        assert_eq!(m.cell(Mode::Plan, Safety::Write), PermissionCell::Deny);
    }

    // #719: the coarse goal-loop gate keys on the Sensitive tier of the active
    // mode's matrix posture — Auto pauses (Ask), Act proceeds (Allow), Plan
    // denies (Deny). Locks the mapping against the default matrix so a matrix
    // change that would let an unattended loop run in Plan (or auto-run Sensitive
    // work in Auto) fails here.
    #[test]
    fn goal_gate_maps_matrix_posture_per_mode() {
        use ff_core::PermissionMatrix;
        let m = PermissionMatrix::default();
        // Auto: Sensitive = Ask -> pause & surface, don't auto-run unattended.
        assert_eq!(goal_gate_for(Mode::Auto, &m), GateDecision::Pause);
        // Act: Sensitive = Allow -> run autonomously.
        assert_eq!(goal_gate_for(Mode::Act, &m), GateDecision::Proceed);
        // Plan: Sensitive = Ask (#793) -> pause & surface; the goal asks the
        // human before an externally-visible read (web_fetch) rather than
        // hard-denying. Write/Dangerous stay denied inside the turn.
        assert_eq!(goal_gate_for(Mode::Plan, &m), GateDecision::Pause);
    }

    // #719: an edited matrix cell flips the goal gate on the next boundary (read
    // live), mirroring the per-tool gate acceptance (#702) at the loop level.
    #[test]
    fn goal_gate_follows_an_edited_sensitive_cell() {
        use ff_core::{PermissionCell, PermissionMatrix};
        let mut m = PermissionMatrix::default();
        // Default Auto+Sensitive is Ask -> Pause.
        assert_eq!(goal_gate_for(Mode::Auto, &m), GateDecision::Pause);
        // Operator allows Sensitive in Auto -> the loop may now proceed unattended.
        m.set_cell(Mode::Auto, Safety::Sensitive, PermissionCell::Allow);
        assert_eq!(goal_gate_for(Mode::Auto, &m), GateDecision::Proceed);
        // Operator hard-denies it -> the loop halts.
        m.set_cell(Mode::Auto, Safety::Sensitive, PermissionCell::Deny);
        assert_eq!(goal_gate_for(Mode::Auto, &m), GateDecision::Deny);
    }

    // #778: the neutral continuation nudge must NOT inline the objective — the
    // system-prompt goal block (#718) is the single source for it, so repeating
    // it here duplicated it every iteration. Guard that the nudge stays generic
    // while still pointing at goal_complete.
    #[test]
    fn goal_continue_nudge_does_not_inline_the_objective() {
        let n = super::GOAL_CONTINUE_NUDGE;
        assert!(
            n.contains("goal_complete"),
            "nudge should still point at the completion tool"
        );
        assert!(
            n.to_lowercase().contains("continue toward the goal"),
            "nudge should be a neutral continue"
        );
        // It must reference the goal only by indirection ("in your instructions"),
        // never carry an objective string or a format placeholder.
        assert!(
            n.contains("described in your instructions"),
            "nudge must defer to the system-prompt goal block for the objective"
        );
        assert!(
            !n.contains("{}") && !n.contains("{0}"),
            "nudge must be a static string, not an objective-interpolating format"
        );
    }

    // Acceptance (#702): editing a matrix cell changes the invocation-time gate
    // decision that `UiApprover::approve` applies to the next tool call. Exercises
    // the same pure `matrix_gate` the approver calls, so no AppHandle / live model
    // turn is required.
    #[test]
    fn edited_cell_flips_the_invocation_gate() {
        use ff_core::{PermissionCell, PermissionMatrix};
        let mut m = PermissionMatrix::default();
        // Mirror `UiApprover::approve`: resolve the effective cell (per-tool
        // override else the mode×safety cell, #742) then gate on it.
        let gate = |m: &PermissionMatrix, tool: &str, mode: Mode, safety: Safety| {
            matrix_gate(m.effective_cell(tool, mode, safety))
        };

        // Default: a Dangerous call in Act prompts the user (Ask → None).
        assert_eq!(gate(&m, "bash", Mode::Act, Safety::Dangerous), None);
        // A Sensitive call in Auto also prompts by default.
        assert_eq!(gate(&m, "web_fetch", Mode::Auto, Safety::Sensitive), None);

        // Deny the dangerous cell → the next invocation is rejected outright.
        m.set_cell(Mode::Act, Safety::Dangerous, PermissionCell::Deny);
        assert_eq!(gate(&m, "bash", Mode::Act, Safety::Dangerous), Some(false));

        // Allow the sensitive cell → the next invocation auto-approves (no prompt).
        m.set_cell(Mode::Auto, Safety::Sensitive, PermissionCell::Allow);
        assert_eq!(
            gate(&m, "web_fetch", Mode::Auto, Safety::Sensitive),
            Some(true)
        );

        // A per-tool override (#742) wins over the matrix cell for that tool only.
        m.set_override("web_fetch", PermissionCell::Deny);
        assert_eq!(
            gate(&m, "web_fetch", Mode::Auto, Safety::Sensitive),
            Some(false)
        );
        assert_eq!(
            gate(&m, "web_search", Mode::Auto, Safety::Sensitive),
            Some(true)
        );

        // Untouched cells keep their default decision.
        assert_eq!(gate(&m, "read_file", Mode::Act, Safety::Write), Some(true));
        assert_eq!(
            gate(&m, "write_file", Mode::Plan, Safety::Write),
            Some(false)
        );
    }

    // #827/#828 Part C: pre_prompt_decision encodes the canonical gate order.
    // A regression that reorders allowlist-first is caught here directly.
    #[test]
    fn pre_prompt_deny_overrides_allowlist() {
        use ff_core::PermissionCell;
        assert_eq!(
            pre_prompt_decision(PermissionCell::Deny, true, None, Safety::Write),
            PrePromptDecision::Deny
        );
        assert_eq!(
            pre_prompt_decision(PermissionCell::Deny, true, None, Safety::Sensitive),
            PrePromptDecision::Deny
        );
    }

    #[test]
    fn pre_prompt_allowlist_accelerates_ask() {
        use ff_core::PermissionCell;
        assert_eq!(
            pre_prompt_decision(PermissionCell::Ask, true, None, Safety::Write),
            PrePromptDecision::Allow
        );
        assert_eq!(
            pre_prompt_decision(PermissionCell::Ask, false, None, Safety::Write),
            PrePromptDecision::Prompt
        );
    }

    #[test]
    fn pre_prompt_scoped_deny_vetoes_when_not_allowlisted() {
        use ff_core::{PermissionCell, RuleEffect};
        // Scoped Deny vetoes when the tool is NOT on the allowlist.
        assert_eq!(
            pre_prompt_decision(
                PermissionCell::Ask,
                false,
                Some(RuleEffect::Deny),
                Safety::Write
            ),
            PrePromptDecision::Deny
        );
        // But the allowlist fires first — if allowlisted, scoped rules are skipped.
        assert_eq!(
            pre_prompt_decision(
                PermissionCell::Ask,
                true,
                Some(RuleEffect::Deny),
                Safety::Write
            ),
            PrePromptDecision::Allow
        );
    }

    #[test]
    fn pre_prompt_scoped_allow_does_not_clear_dangerous() {
        use ff_core::{PermissionCell, RuleEffect};
        assert_eq!(
            pre_prompt_decision(
                PermissionCell::Ask,
                false,
                Some(RuleEffect::Allow),
                Safety::Dangerous
            ),
            PrePromptDecision::Prompt
        );
        assert_eq!(
            pre_prompt_decision(
                PermissionCell::Ask,
                false,
                Some(RuleEffect::Allow),
                Safety::Write
            ),
            PrePromptDecision::Allow
        );
    }

    #[test]
    fn pre_prompt_plan_write_denied_despite_allowlist() {
        let state = AppState::new();
        state.set_session_approve("s1", "github");
        let matrix = state.permission_matrix();
        let cell = matrix.effective_cell("github", Mode::Plan, Safety::Write);
        let allowlisted = state.allowlist_covers("s1", "github", Safety::Write);
        assert!(allowlisted);
        assert_eq!(
            pre_prompt_decision(cell, allowlisted, None, Safety::Write),
            PrePromptDecision::Deny,
            "Plan x Write = Deny must override the allowlist"
        );
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

    /// Init a temp repo with one commit on `main` plus the extra `branches`, all
    /// pointing at that commit. Returns the tempdir (keep it alive for the test).
    /// Skips (returns `None`) if `git` is unavailable so the suite still passes on
    /// a host without git installed.
    #[cfg(test)]
    fn temp_repo(branches: &[&str]) -> Option<tempfile::TempDir> {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| -> bool {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !run(&["init", "-q", "-b", "main"]) {
            return None; // git missing or too old for `-b`; skip.
        }
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(dir.path().join("f.txt"), "x").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
        for b in branches {
            run(&["branch", b]);
        }
        Some(dir)
    }

    #[test]
    fn list_local_branches_returns_all_local_branches_sorted() {
        let Some(dir) = temp_repo(&["develop", "feature/x"]) else {
            return;
        };
        let mut got = list_local_branches(dir.path()).unwrap();
        got.sort();
        assert_eq!(got, vec!["develop", "feature/x", "main"]);
    }

    #[test]
    fn list_local_branches_is_empty_when_not_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            list_local_branches(dir.path()).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn switch_branch_checks_out_and_moves_head() {
        let Some(dir) = temp_repo(&["develop"]) else {
            return;
        };
        assert_eq!(git_branch(dir.path()), Some("main".to_string()));
        switch_branch(dir.path(), "develop").unwrap();
        assert_eq!(git_branch(dir.path()), Some("develop".to_string()));
    }

    #[test]
    fn switch_branch_rejects_unknown_branch() {
        let Some(dir) = temp_repo(&[]) else {
            return;
        };
        let err = switch_branch(dir.path(), "nope").unwrap_err();
        assert!(err.contains("unknown branch"));
        // HEAD did not move.
        assert_eq!(git_branch(dir.path()), Some("main".to_string()));
    }

    #[test]
    fn switch_branch_rejects_flag_like_branch_before_spawning_git() {
        let Some(dir) = temp_repo(&[]) else {
            return;
        };
        // A branch literally named like a flag is never a real local branch, so the
        // membership check rejects it before it can reach `git checkout`.
        let err = switch_branch(dir.path(), "--orphan").unwrap_err();
        assert!(err.contains("unknown branch"));
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

    // ---- CLI.7 sidecar parity integration test (RFC 0004 §5) ----
    //
    // `run_sidecar_turn` is the Tauri command that spawns the bundled `flowforge`
    // CLI as a sidecar, reads its `--json` stdout line-by-line, and re-emits every
    // parsed `AgentEvent` through `emit_agent_event` — the same helper the
    // in-process `run_turn` path uses. This test exercises the full
    // spawn → stdout-parse → emit pipeline end-to-end:
    //
    //   1. Requires the sidecar binary at `target/<profile>/flowforge` (staged by
    //      `scripts/stage-sidecar.sh` or a bare `cargo build -p ff-cli`). The
    //      `tauri_plugin_shell` sidecar resolver looks for the binary relative to
    //      `current_exe()`; under `cargo test` that resolves to the workspace
    //      `target/<profile>/` directory.
    //   2. Stands up a wiremock OpenAI-compatible endpoint so the CLI can actually
    //      complete a turn (the `run` subcommand would otherwise fail to reach a
    //      provider and exit non-zero before emitting `turn:done`).
    //   3. Overrides `HOME` so the CLI reads a temp `provider.json` pointing at
    //      the mock. A process-wide mutex serializes the override against parallel
    //      tests that might also touch `HOME`.
    //   4. Drives `run_sidecar_turn` through a `MockRuntime` Tauri app (the
    //      command is generic over `R: tauri::Runtime` for exactly this reason)
    //      and asserts the `turn:done` event fires.

    /// `HOME` is a process-global env var; serialize tests that override it so
    /// they don't race each other or other tests that read `HOME`.
    ///
    /// NOTE: `std::env::set_var` is deprecated as of Rust 1.80 on soundness
    /// grounds — it's UB in multithreaded programs because another thread
    /// can read `HOME` concurrently with the mutation. This mutex only
    /// serializes *these tests*; it does not stop unrelated threads from
    /// reading `HOME` mid-override. Acceptable today because each
    /// `#[tokio::test]` here runs on a single task with no other threads
    /// touching `HOME`, but a future edition is expected to make this a hard
    /// error — at which point the override should move to per-spawn env
    /// injection (e.g. `Command::env` on the sidecar subprocess) rather than
    /// mutating the process-global `HOME`.
    static SIDECAR_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Resolve the sidecar binary path that `tauri_plugin_shell`'s
    /// `relative_command_path` will look for under `cargo test` — the workspace
    /// `target/<profile>/flowforge` (no target-triple suffix at runtime; the
    /// suffix is only used by the `tauri build` bundler).
    fn sidecar_binary_path() -> std::path::PathBuf {
        let mut path = std::env::current_exe().expect("current_exe");
        // The test binary lives in `target/<profile>/deps/<name>-<hash>`. Pop
        // `deps/<name>-<hash>` to land on `target/<profile>/`.
        path.pop(); // <name>-<hash>
        path.pop(); // deps
        path.push("flowforge");
        path
    }

    /// Write a `provider.json` that points the CLI's `CandleVllm` provider at the
    /// mock server, under the temp `HOME`'s platform-specific config dir. Returns
    /// the temp dir (kept alive for the test duration).
    fn stage_provider_config(base_url: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let temp_home = tempfile::tempdir().expect("temp HOME");

        // `dirs::config_dir()` respects `HOME` on Unix and macOS — so set it
        // before computing the path.
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", temp_home.path());

        let config_dir = dirs::config_dir()
            .expect("config dir resolves under temp HOME")
            .join("flowforge");
        std::fs::create_dir_all(&config_dir).expect("create flowforge config dir");

        let provider_json = serde_json::json!({
            "kind": "candleVllm",
            "baseUrl": base_url,
            "model": "test-sidecar-model",
            "hasKey": false,
            "thinking": false,
        });
        let provider_path = config_dir.join("provider.json");
        std::fs::write(&provider_path, provider_json.to_string()).expect("write provider.json");

        // Restore HOME — the override is re-applied for the actual sidecar spawn
        // in the test body. We only needed it here to compute the platform path.
        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }

        (temp_home, provider_path)
    }

    #[tokio::test]
    #[ignore = "requires staged sidecar — run ./scripts/stage-sidecar.sh, then `cargo test ... -- --ignored`"]
    #[allow(clippy::await_holding_lock)]
    async fn sidecar_turn_emits_turn_done_event() {
        // The `std::sync::Mutex` guard is held across `.await` points because
        // the `HOME` override must stay in place for the entire sidecar spawn
        // + turn duration. This is safe: the lock is only acquired by this one
        // test, runs on a single tokio task, and guards a process-global env
        // var — no other async task contends on it.
        //
        // `#[ignore]` makes this opt-in: `cargo test --workspace` reports it as
        // ignored (not passed), so CI can't silently drop it behind a soft-skip
        // `return` and advertise false confidence. Run it explicitly with
        // `--ignored` after staging the sidecar.
        let sidecar = sidecar_binary_path();
        assert!(
            sidecar.exists(),
            "sidecar binary not found at {} — run `./scripts/stage-sidecar.sh` \
             (or `cargo build -p ff-cli`) first",
            sidecar.display(),
        );

        let _guard = SIDECAR_TEST_LOCK.lock().unwrap();

        // Mock OpenAI-compatible endpoint: one content delta, then [DONE].
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n\
                     data: [DONE]\n\n",
            ))
            .mount(&server)
            .await;

        // Stage provider.json under a temp HOME.
        let (temp_home, _provider_path) = stage_provider_config(&server.uri());

        // Override HOME for the sidecar subprocess (it inherits the parent's env).
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", temp_home.path());

        // Mock Tauri app with the shell plugin — `run_sidecar_turn` is generic
        // over `R: tauri::Runtime` so it accepts the `MockRuntime` app handle.
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_shell::init())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app builds");

        // Listen for the `turn:done` event that `emit_agent_event` emits.
        use tauri::Listener;
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_clone = done.clone();
        app.listen("turn:done", move |_| {
            done_clone.store(true, Ordering::SeqCst);
        });

        // Also track the total event count for a richer assertion.
        let token = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let token_clone = token.clone();
        app.listen("turn:token", move |_| {
            token_clone.fetch_add(1, Ordering::SeqCst);
        });

        let handle = app.handle().clone();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            run_sidecar_turn(handle, "hello".into()),
        )
        .await;

        // Restore HOME before asserting so a panic doesn't leak the temp HOME.
        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        drop(_guard);

        let result = result.expect("run_sidecar_turn timed out after 30s");
        assert!(
            result.is_ok(),
            "run_sidecar_turn failed: {:?}",
            result.err()
        );
        assert!(
            done.load(Ordering::SeqCst),
            "turn:done event was not received — the sidecar spawned but the \
             AgentEvent → Tauri event pipeline did not deliver the terminal event"
        );
        assert!(
            token.load(Ordering::SeqCst) >= 1,
            "expected at least one turn:token event from the sidecar"
        );
    }

    #[tokio::test]
    #[ignore = "requires the sidecar NOT be staged — inverse of the happy-path test; run with `--ignored` only when target/flowforge is absent"]
    async fn sidecar_turn_returns_error_without_sidecar_binary() {
        // When the sidecar binary is absent, `run_sidecar_turn` must surface a
        // clear error rather than panicking or hanging. This guards the resolver
        // path (`app.shell().sidecar`) against silent failures.
        //
        // `#[ignore]` + a hard assert (not a soft-skip `return`) so CI can't
        // silently drop it: it fails loudly if the binary is present. Mutually
        // exclusive with `sidecar_turn_emits_turn_done_event` — don't run both
        // via `--ignored` together (one needs the binary staged, the other absent).
        let sidecar = sidecar_binary_path();
        assert!(
            !sidecar.exists(),
            "sidecar binary found at {} — this test requires the sidecar NOT be \
             staged (it is the inverse of `sidecar_turn_emits_turn_done_event`)",
            sidecar.display(),
        );

        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_shell::init())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app builds");

        let handle = app.handle().clone();
        let result = run_sidecar_turn(handle, "hello".into()).await;
        assert!(
            result.is_err(),
            "run_sidecar_turn should fail when the sidecar binary is missing, got: {:?}",
            result
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("sidecar") || err.contains("failed to"),
            "error should mention the sidecar resolution failure, got: {err}"
        );
    }

    // `emit_agent_event` is generic over `R: tauri::Runtime`; the sidecar path
    // and the in-process turn path both feed through it. A direct unit-level
    // assertion that the `Done` variant maps to `turn:done` (not, say, silently
    // dropped) catches a mapping regression without the subprocess overhead.
    #[tokio::test]
    async fn emit_agent_event_maps_done_to_turn_done_event() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app builds");

        use tauri::Listener;
        let received = Arc::new(Mutex::new(None));
        let received_clone = received.clone();
        app.listen("turn:done", move |event| {
            let payload: TurnDoneEvent =
                serde_json::from_str(event.payload()).expect("payload deserializes");
            *received_clone.lock().unwrap() = Some(payload);
        });

        let done_event = AgentEvent::Done {
            message_id: "msg-1".into(),
            final_message: Some("hello".into()),
            stop_reason: None,
            turns: Some(1),
            token_count: Some(42),
            prefill_estimates: None,
            tier1_fires: None,
            tier2_fires: None,
            cache_hit_tokens: Some(10),
            cache_miss_tokens: Some(32),
        };
        emit_agent_event(app.handle(), "session-1", done_event);

        // The mock runtime may deliver events asynchronously; yield once.
        tokio::task::yield_now().await;

        let received = received.lock().unwrap().clone();
        let payload = received.expect("turn:done event was not delivered");
        assert_eq!(payload.session_id, "session-1");
        assert_eq!(payload.message_id, "msg-1");
    }
}
