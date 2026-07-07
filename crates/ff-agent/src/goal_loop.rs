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
use ff_core::{Goal, GoalStatus};
use futures_util::FutureExt;

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

    /// Persist the goal at an iteration boundary (post-checkpoint / on stop).
    /// Best-effort: the host logs a failed persist but the loop does not abort —
    /// the next boundary retries. Kept on the trait (not a separate sink) so the
    /// desktop's single `GoalStore`-backed impl owns both turn-running and saving.
    fn save(&self, goal: &Goal);

    /// Current wall-clock in epoch-ms, injected so tests are deterministic.
    fn now_ms(&self) -> i64;
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
            goal.status = GoalStatus::Completed;
            goal.updated_ms = iter.now_ms();
            iter.save(goal);
            return LoopStop::Completed;
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
