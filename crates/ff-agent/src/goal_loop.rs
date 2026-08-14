//! Goal mode: the self-continue loop skeleton (RFC 0020 §5.2, #716).
//!
//! This is the headless driver that turns a durable [`Goal`] into repeated
//! agent turns until the objective is met, a budget dimension is exhausted, the
//! user pauses, or a stop condition fires. It deliberately mirrors the
//! `ff-scheduled` runner design: the loop mechanics (budget accrual,
//! checkpointing, boundary decisions, gating placeholder) live here behind the
//! [`GoalIteration`] trait, so the desktop host supplies an impl that wraps
//! `spawn_assistant_turn` / `run_turn` while the mechanics are exercised by
//! stubs in unit tests — no Tauri, no real provider needed.
//!
//! ## What this skeleton does NOT do yet (per #716 "Scope (out)")
//! - **Real permission-matrix gating.** The loop consults a [`GateDecision`]
//!   supplied by the host through [`GoalIteration::gate`]; #719/#682 will feed it
//!   the matrix cell. Today the host passes *today's* `mode_auto_approves`
//!   placeholder — a one-line swap at that single seam.
//! - **`ask_user` mid-loop pause wiring** (Track F) — the loop already honors a
//!   [`GateDecision::Pause`] boundary, so wiring Ask later is just returning
//!   `Pause` from `gate`.
//! - **Rich ledger verdicts.** The skeleton appends only the minimal boundary
//!   bookkeeping the host hands back; evidence-first verdict adjudication is a
//!   separate, larger piece.

use std::panic::AssertUnwindSafe;

use async_trait::async_trait;
use ff_core::{Goal, GoalLedgerEntry, GoalStatus, StepStatus, Verdict};
use futures_util::FutureExt;

use crate::AgentEvent;

/// Why a single loop pass stopped, returned by [`drive_goal`]. The loop persists
/// the goal before returning, so the caller only needs this to decide what to
/// surface (event / log / FE poll).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopStop {
    /// The agent called `goal_complete`: the objective is met.
    Completed,
    /// A budget dimension (iterations / tokens / wall-clock) was exhausted.
    Exhausted,
    /// The user (or an `ask_user`-class decision) paused at a boundary; resumable.
    Paused,
    /// A stop condition or unrecoverable iteration error ended the loop.
    Failed,
}

/// The gate decision applied at the top of each iteration (RFC 0020 §5.2). This
/// is the single seam where #719/#682 will plug the permission matrix; the
/// skeleton lets the host return today's placeholder verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Run this iteration.
    Proceed,
    /// Halt at this boundary and mark the goal `Paused` (resumable). Used for the
    /// future `ask_user` circuit-breaker join point.
    Pause,
    /// Halt and mark the goal `Failed` (a policy denial that is not resumable).
    Deny,
}

/// The result of one agent turn, handed back by [`GoalIteration::run_once`] so
/// the loop can accrue spend, checkpoint, and detect completion. Kept minimal on
/// purpose (skeleton): richer ledger evidence is a later piece.
#[derive(Debug, Clone, Default)]
pub struct IterationOutcome {
    /// Tokens consumed by this turn, added to `Goal.spent.tokens`.
    pub tokens: u64,
    /// Wall-clock this turn took, added to `Goal.spent.wall_ms`.
    pub wall_ms: i64,
    /// The agent signalled the objective is met this turn (`goal_complete`).
    pub goal_complete: bool,
    /// The user interrupted the turn (Stop button). The loop halts and leaves the
    /// goal **`Paused`** — resumable from the last checkpoint — not `Failed`.
    /// Takes precedence over `failed` (a cancel often also surfaces as an error).
    pub cancelled: bool,
    /// The turn failed unrecoverably (provider error). Ends the loop as `Failed`.
    pub failed: bool,
    /// The iteration consumed the goal's `pending_steer` (a mid-loop user
    /// message). The loop clears `pending_steer` on the in-memory goal before it
    /// checkpoints, so the steer is applied exactly once and not re-persisted on
    /// the next boundary (#753 review nit 1).
    pub steer_consumed: bool,
    /// Ledger steps the agent recorded this turn via `goal_step` (#1225), in call
    /// order. The host collects them from the event stream — a tool's `run` gets
    /// no `Goal` handle, so it can only signal — and the loop commits them at the
    /// iteration boundary so they persist with the goal.
    pub ledger_steps: Vec<LedgerStep>,
}

/// One `goal_step` call observed on the event stream, normalised into the fields
/// the loop commits to `Goal.ledger`.
///
/// `claim`, `verdict`, and `evidence` all reach the model: `system_prompt.rs`
/// renders each entry's claim and verdict, plus a bounded number of `evidence`
/// pointers (capped per entry and truncated per item) beneath it, for the last
/// few entries (#1242).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerStep {
    /// Id of an existing entry to update in place; `None` appends a new entry.
    pub id: Option<String>,
    pub claim: String,
    pub verdict: Option<Verdict>,
    pub evidence: Vec<String>,
}

/// The result of running a goal's `verify_cmd` against a claimed completion
/// (#684 D3). Distinct from a `run_once` turn: verification is a mechanical,
/// deterministic gate the loop applies to a `goal_complete` signal, so the
/// agent's self-declared "done" becomes *proven* done before the loop accepts
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// The command exited zero — the completion claim is proven; accept it.
    Passed,
    /// The command exited non-zero — the claim is rejected. Carries the
    /// captured command output verbatim; the loop bounds it via
    /// [`bound_verify_output`] when it builds the `Drift` ledger entry, then
    /// feeds that back into the next iteration as evidence.
    Failed { output: String },
    /// No verification ran (the goal has no `verify_cmd`, or the host does not
    /// wire it) — the loop falls back to trusting the claim, as it did pre-D3.
    Skipped,
}

/// Run a goal's `verify_cmd` in `workspace` and map the result to a
/// [`VerifyOutcome`] (#684 D3). Shared by both hosts (CLI + desktop) so the
/// two cannot drift into different readings of "verified": the command runs
/// via the platform shell with stdout+stderr merged, a zero exit is
/// [`VerifyOutcome::Passed`], any non-zero exit (or a spawn failure — a
/// verifier we could not even run has NOT proven completion) is
/// [`VerifyOutcome::Failed`] carrying the combined output. A goal with no
/// `verify_cmd` yields [`VerifyOutcome::Skipped`] without spawning anything.
pub async fn run_verify_command(goal: &Goal, workspace: &std::path::Path) -> VerifyOutcome {
    let Some(cmd) = goal.verify_cmd.as_deref().filter(|c| !c.trim().is_empty()) else {
        return VerifyOutcome::Skipped;
    };

    #[cfg(windows)]
    let mut command = {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    command.current_dir(workspace);

    match command.output().await {
        Ok(out) if out.status.success() => VerifyOutcome::Passed,
        Ok(out) => {
            let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !stderr.trim().is_empty() {
                if !combined.trim().is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&stderr);
            }
            let code = out
                .status
                .code()
                .map_or_else(|| "signal".to_string(), |c| c.to_string());
            VerifyOutcome::Failed {
                output: format!("`{cmd}` exited with status {code}:\n{combined}"),
            }
        }
        Err(e) => VerifyOutcome::Failed {
            output: format!("verify command `{cmd}` could not be run: {e}"),
        },
    }
}

/// One iteration of the goal loop, abstracted so the loop mechanics are testable
/// without Tauri / a real provider (mirrors `ff-scheduled::TaskRunner`). The
/// desktop host implements this over `spawn_assistant_turn` / `run_turn`;
/// persistence at the boundary is the host's job via [`GoalIteration::save`].
#[async_trait]
pub trait GoalIteration: Send + Sync {
    /// Gate the *next* iteration before it runs. #719 swaps this for a matrix
    /// lookup; the placeholder host returns [`GateDecision::Proceed`] whenever
    /// today's mode would auto-approve.
    fn gate(&self, goal: &Goal) -> GateDecision;

    /// Drive one agent turn toward the objective and report what it consumed.
    async fn run_once(&self, goal: &Goal) -> IterationOutcome;

    /// Verify a claimed completion before the loop accepts it (#684 D3). Called
    /// only on a `goal_complete` signal, and only when the goal carries a
    /// `verify_cmd`. The host runs the command and maps its exit status to a
    /// [`VerifyOutcome`]; the loop — not the host — decides what a failure does
    /// (record a `Drift` entry and keep iterating). The default returns
    /// [`VerifyOutcome::Skipped`] so a host that has not wired verification, or a
    /// goal with no `verify_cmd`, keeps the pre-D3 "trust the claim" behaviour.
    async fn verify(&self, _goal: &Goal) -> VerifyOutcome {
        VerifyOutcome::Skipped
    }

    /// Persist the goal at an iteration boundary (post-checkpoint / on stop).
    /// Best-effort: the host logs a failed persist but the loop does not abort —
    /// the next boundary retries. Kept on the trait (not a separate sink) so the
    /// desktop's single `GoalStore`-backed impl owns both turn-running and saving.
    fn save(&self, goal: &Goal);

    /// Current wall-clock in epoch-ms, injected so tests are deterministic.
    fn now_ms(&self) -> i64;
}

/// Normalise a `goal_step` tool call's arguments into a [`LedgerStep`].
///
/// Lives here rather than in either host so the CLI and desktop cannot drift into
/// two different readings of the same tool call. Returns `None` when `claim` is
/// absent or blank — [`GoalStepTool`](ff_tools::GoalStepTool) already rejects
/// that, so this only guards a malformed call reaching the observer.
///
/// An unrecognised `verdict` becomes `None` (step still open) rather than an
/// error: the tool validates the vocabulary at the call site, and silently
/// downgrading here is safer than inventing a verdict the agent did not give.
pub fn parse_ledger_step(args: &serde_json::Value) -> Option<LedgerStep> {
    let claim = args.get("claim")?.as_str()?.trim();
    if claim.is_empty() {
        return None;
    }
    let verdict = args
        .get("verdict")
        .and_then(|v| v.as_str())
        .and_then(|v| match v {
            "match" => Some(Verdict::Match),
            "drift" => Some(Verdict::Drift),
            "unverifiable" => Some(Verdict::Unverifiable),
            _ => None,
        });
    let evidence = args
        .get("evidence")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(LedgerStep {
        id: args
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty()),
        claim: claim.to_string(),
        verdict,
        evidence,
    })
}

/// Per-turn collector for the `goal_step` / `goal_complete` signals a running
/// turn emits as [`AgentEvent`]s. A tool's `run` gets no `Goal` handle, so the
/// only channel is the event stream: feed every event through [`observe`] and
/// the collector runs the started→pending→finished→commit state machine, then
/// hands back the committed steps at the turn boundary.
///
/// Both hosts (CLI and desktop) drove an identical state machine inline,
/// differing only in locking (#1226); this is the shared owner so the two
/// cannot drift. Desktop wraps it in a single `Mutex` rather than scattering a
/// lock across four match arms.
///
/// [`observe`]: TurnLedger::observe
#[derive(Debug, Default)]
pub struct TurnLedger {
    /// Call id of the in-flight `goal_complete`, set on its `Started` event and
    /// promoted to `completed` when that same call finishes successfully.
    gc_call_id: Option<String>,
    completed: bool,
    /// `goal_step` calls parsed at `Started` but not yet resolved, keyed by call
    /// id. A step is committed only when its call finishes successfully.
    pending: std::collections::HashMap<String, LedgerStep>,
    steps: Vec<LedgerStep>,
}

impl TurnLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Route one agent event through the state machine.
    pub fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::ToolCallStarted { call_id, name, .. }
                if name == ff_tools::GOAL_COMPLETE_TOOL_NAME =>
            {
                self.gc_call_id = Some(call_id.clone());
            }
            // A tool's `run` gets no `Goal` handle, so `goal_step` can only signal:
            // parse its args here and commit at the iteration boundary (#1225).
            AgentEvent::ToolCallStarted {
                call_id,
                name,
                args,
                ..
            } if name == ff_tools::GOAL_STEP_TOOL_NAME => {
                if let Some(step) = parse_ledger_step(args) {
                    self.pending.insert(call_id.clone(), step);
                }
            }
            AgentEvent::ToolCallFinished {
                call_id,
                success: true,
                ..
            } if self.gc_call_id.as_deref() == Some(call_id.as_str()) => {
                self.completed = true;
            }
            // Only a successful call is recorded: a rejected or failed `goal_step`
            // must not leave a claim in the ledger.
            AgentEvent::ToolCallFinished {
                call_id, success, ..
            } if self.pending.contains_key(call_id.as_str()) => {
                // Drop the pending step either way: on failure it must not survive to be
                // committed by a later call's event, and it can never be finished twice.
                if let Some(step) = self.pending.remove(call_id.as_str()) {
                    if *success {
                        self.steps.push(step);
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether a `goal_complete` call finished successfully this turn.
    pub fn completed(&self) -> bool {
        self.completed
    }

    /// The steps committed this turn, in observation order. Consumes the
    /// collector — a turn's ledger is read exactly once, at its boundary.
    pub fn into_steps(self) -> Vec<LedgerStep> {
        self.steps
    }
}

/// Commit the `goal_step` calls observed during one turn into `Goal.ledger`.
///
/// An entry carrying an `id` that matches an existing step updates it in place;
/// anything else appends. A verdict implies the step's status: `Match` closes it,
/// while `Drift` and `Unverifiable` leave it `Active` so a later iteration can
/// see there is unfinished business — a step that could not be checked is never
/// quietly treated as done (`StepStatus`/`Verdict` docs, #74).
fn commit_ledger_steps(goal: &mut Goal, steps: &[LedgerStep], now_ms: i64) {
    for step in steps {
        let status = match step.verdict {
            Some(Verdict::Match) => StepStatus::Done,
            Some(_) => StepStatus::Active,
            None => StepStatus::Active,
        };
        // `update_entry` reports whether the id existed; an unknown id falls
        // through to an append so a step is never silently dropped.
        let updated = step.id.as_deref().is_some_and(|id| {
            goal.update_entry(id, now_ms, |e| {
                e.claim = step.claim.clone();
                e.verdict = step.verdict;
                e.evidence = step.evidence.clone();
                e.status = status;
            })
        });
        if !updated {
            // `append_entry` does not mint ids, and an entry with an empty id could
            // never be updated in place afterwards. Derive a stable one from the
            // step's position, then bump past any id already in the ledger: a step
            // that explicitly passed e.g. `step-2` before a positional mint reaches
            // the same number would otherwise collide and make the two entries
            // un-addressable (#1226).
            let id = step
                .id
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| mint_step_id(goal));
            goal.append_entry(
                GoalLedgerEntry {
                    id,
                    status,
                    claim: step.claim.clone(),
                    action: None,
                    evidence: step.evidence.clone(),
                    verdict: step.verdict,
                    next: None,
                    created_ms: now_ms,
                    updated_ms: now_ms,
                },
                now_ms,
            );
        }
    }
}

/// Build the `Drift` ledger entry recorded when a `verify_cmd` rejects a
/// `goal_complete` claim (#684 D3).
///
/// The `claim` is the assertion being tested — the agent's own `goal_complete`
/// signal that the objective is met — so the ledger reads "claimed complete →
/// evidence → Drift", the same evidence-first shape as a `goal_step` (#74). The
/// verdict is [`Verdict::Drift`]: the verify output contradicts that claim. The
/// status is therefore [`StepStatus::Active`], matching the canonical
/// verdict→status mapping in [`commit_ledger_steps`] (`Drift` leaves the goal
/// with unfinished business) rather than a terminal `Blocked`.
///
/// The command's output is carried as evidence so #1242's renderer folds it into
/// the next iteration's prompt, giving the agent the concrete failure to fix. It
/// is bounded here (durable JSON should not hold a whole test log; the renderer
/// bounds it again per-item). `created_ms`/`updated_ms` are left `0` because
/// [`Goal::append_entry`] stamps both from the loop's clock at the call site.
fn verify_failure_entry(goal: &Goal, output: &str) -> GoalLedgerEntry {
    GoalLedgerEntry {
        id: mint_step_id(goal),
        claim: "goal_complete: the objective is met".to_string(),
        status: StepStatus::Active,
        action: None,
        evidence: vec![bound_verify_output(output)],
        verdict: Some(Verdict::Drift),
        next: None,
        created_ms: 0,
        updated_ms: 0,
    }
}

/// Cap a verify command's captured output before it is stored as ledger
/// evidence, on a UTF-8 char boundary, appending a marker when clipped. Delegates
/// to [`crate::system_prompt::clip_evidence_chars`] so the durable-file bound and
/// #1242's prompt-render bound share one implementation and one marker; only the
/// budget differs (a verify log may be far larger than a rendered pointer).
fn bound_verify_output(output: &str) -> String {
    const MAX_VERIFY_OUTPUT_CHARS: usize = 4000;
    crate::system_prompt::clip_evidence_chars(output, MAX_VERIFY_OUTPUT_CHARS)
}

/// Mint a positional `step-N` id that is not already used by an entry in the
/// ledger. The base is `len + 1` (the natural next slot); it advances past any
/// collision with an id an earlier step supplied explicitly (#1226).
fn mint_step_id(goal: &Goal) -> String {
    let mut n = goal.ledger.len() + 1;
    loop {
        let candidate = format!("step-{n}");
        if !goal.ledger.iter().any(|e| e.id == candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Drive the self-continue loop for `goal` until a terminal condition, mutating
/// `goal` in place and persisting through `iter.save` at every boundary (RFC
/// 0020 §5.2). Returns why it stopped. The caller owns activation: `goal.status`
/// must be [`GoalStatus::Active`] on entry (a non-active goal returns immediately
/// with the matching stop, so a paused/completed goal is a no-op).
///
/// Loop order per iteration (the boundary invariant #716 locks in):
/// 1. If `goal.status` is not `Active`, stop.
/// 2. If a budget dimension is already exhausted, mark `Exhausted` and stop.
/// 3. `gate()` — `Pause`/`Deny` transition and stop; `Proceed` continues.
/// 4. `run_once()` — one turn; a panic is caught and treated as `Failed` so a
///    single bad turn can never unwind the whole loop.
/// 5. `checkpoint()` — accrue spend, bump the iteration count, persist.
/// 6. Terminal precedence: `goal_complete` → `Completed` (wins even over a
///    same-iteration budget overrun); else a user cancel → `Paused` (resumable);
///    else an unrecoverable error → `Failed`; else loop.
pub async fn drive_goal<I: GoalIteration>(goal: &mut Goal, iter: &I) -> LoopStop {
    loop {
        // (1) Only an Active goal continues; map the current terminal state.
        match goal.status {
            GoalStatus::Active => {}
            GoalStatus::Completed => return LoopStop::Completed,
            GoalStatus::Exhausted => return LoopStop::Exhausted,
            GoalStatus::Paused => return LoopStop::Paused,
            GoalStatus::Failed => return LoopStop::Failed,
        }

        // (2) Budget check BEFORE spending another turn, so a goal that entered
        // already at its ceiling exhausts without an extra (billed) turn.
        if goal.budget_exhausted() {
            goal.status = GoalStatus::Exhausted;
            goal.updated_ms = iter.now_ms();
            iter.save(goal);
            return LoopStop::Exhausted;
        }

        // (3) Gate the iteration. This is the #719/#682 matrix seam.
        match iter.gate(goal) {
            GateDecision::Proceed => {}
            GateDecision::Pause => {
                goal.status = GoalStatus::Paused;
                goal.updated_ms = iter.now_ms();
                iter.save(goal);
                return LoopStop::Paused;
            }
            GateDecision::Deny => {
                goal.status = GoalStatus::Failed;
                goal.updated_ms = iter.now_ms();
                iter.save(goal);
                return LoopStop::Failed;
            }
        }

        // (4) Run one turn. Catch a panic so a single bad turn is recorded as a
        // failed iteration rather than unwinding the loop task (mirrors the
        // scheduled runner's panic isolation).
        let outcome = match AssertUnwindSafe(iter.run_once(goal)).catch_unwind().await {
            Ok(o) => o,
            Err(_panic) => IterationOutcome {
                failed: true,
                ..Default::default()
            },
        };

        // Clear a consumed steer on the in-memory goal BEFORE checkpointing, so
        // it is applied exactly once and the next boundary's save does not
        // re-persist a stale `pending_steer` (#753 review nit 1).
        if outcome.steer_consumed {
            goal.pending_steer = None;
        }

        // Commit ledger steps recorded this turn BEFORE the checkpoint, so they
        // persist in the same save as the spend they were produced by. A panicked
        // turn yields the default outcome (no steps), so nothing is invented for a
        // turn that did not report.
        commit_ledger_steps(goal, &outcome.ledger_steps, iter.now_ms());

        // (5) Close the boundary: accrue spend + bump the iteration, then persist.
        // Even a cancelled turn spent (billed) tokens, so accruing them is honest;
        // the iteration count is conservative (counts the interrupted attempt).
        goal.checkpoint(outcome.tokens, outcome.wall_ms, iter.now_ms());

        // (6) Terminal precedence: a genuine completion wins (even over a
        // same-iteration budget overrun); otherwise a user cancel PAUSES the goal
        // resumably (RFC 0020 §5.3 — resume replays from this last checkpoint),
        // and only an unrecoverable error FAILS it. Cancel takes precedence over
        // `failed` because an interrupted turn often also surfaces as an error.
        if outcome.goal_complete {
            // #684 D3: a `goal_complete` is a *claim*, not proof. When the goal
            // carries a `verify_cmd`, run it and only accept `Completed` on a
            // green result. A red result rejects the claim, records a `Drift`
            // ledger entry whose evidence is the command's output — which
            // #1242's renderer folds back into the next iteration's prompt — and
            // falls through so the loop keeps working. `Skipped` (no verify_cmd,
            // or an unwired host) keeps the pre-D3 "trust the claim" behaviour.
            match iter.verify(goal).await {
                VerifyOutcome::Passed | VerifyOutcome::Skipped => {
                    goal.status = GoalStatus::Completed;
                    goal.updated_ms = iter.now_ms();
                    iter.save(goal);
                    return LoopStop::Completed;
                }
                VerifyOutcome::Failed { output } => {
                    goal.append_entry(verify_failure_entry(goal, &output), iter.now_ms());
                    // Persist the rejection + checkpoint, then fall through: the
                    // next pass re-checks budget (a now-exhausted goal ends as
                    // `Exhausted`, not a silent success) and gives the agent the
                    // failure output to act on.
                    iter.save(goal);
                    continue;
                }
            }
        }
        if outcome.cancelled {
            goal.status = GoalStatus::Paused;
            goal.updated_ms = iter.now_ms();
            iter.save(goal);
            return LoopStop::Paused;
        }
        if outcome.failed {
            goal.status = GoalStatus::Failed;
            goal.updated_ms = iter.now_ms();
            iter.save(goal);
            return LoopStop::Failed;
        }

        // Persist the mid-loop checkpoint before deciding to continue, so an
        // interruption (crash / kill) resumes from the last completed boundary.
        iter.save(goal);
        // Fall through to the top: the next pass re-checks status + budget.
    }
}

#[cfg(test)]
mod tests;
