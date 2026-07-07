use super::*;
use ff_core::{Goal, GoalBudget, GoalStatus};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

fn active_goal(max_iterations: u32) -> Goal {
    let mut g = Goal::new("s1", "ship the feature", 0);
    g.status = GoalStatus::Active;
    g.budget = GoalBudget {
        max_iterations,
        max_tokens: None,
        max_wall_ms: None,
    };
    g
}

/// A stub iteration: completes on the Nth turn, spends fixed tokens/turn, and
/// optionally gates or panics. `now` advances by a fixed step per call so
/// `updated_ms` moves deterministically. Records every save so a test can
/// assert persistence at each boundary.
struct StubIter {
    complete_on: u32,
    tokens_per_turn: u64,
    gate: GateDecision,
    panic_on: Option<u32>,
    fail_on: Option<u32>,
    cancel_on: Option<u32>,
    steer_on: Option<u32>,
    calls: AtomicU32,
    now: AtomicU32,
    saves: Mutex<Vec<(GoalStatus, u32)>>,
}
impl StubIter {
    fn completing(complete_on: u32, tokens_per_turn: u64) -> Self {
        Self {
            complete_on,
            tokens_per_turn,
            gate: GateDecision::Proceed,
            panic_on: None,
            fail_on: None,
            cancel_on: None,
            steer_on: None,
            calls: AtomicU32::new(0),
            now: AtomicU32::new(0),
            saves: Mutex::new(Vec::new()),
        }
    }
    fn call_count(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}
#[async_trait]
impl GoalIteration for StubIter {
    fn gate(&self, _goal: &Goal) -> GateDecision {
        self.gate
    }
    async fn run_once(&self, _goal: &Goal) -> IterationOutcome {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.panic_on == Some(n) {
            panic!("boom in turn {n}");
        }
        IterationOutcome {
            tokens: self.tokens_per_turn,
            wall_ms: 10,
            goal_complete: n >= self.complete_on,
            cancelled: self.cancel_on == Some(n),
            failed: self.fail_on == Some(n),
            steer_consumed: self.steer_on == Some(n),
        }
    }
    fn save(&self, goal: &Goal) {
        self.saves
            .lock()
            .unwrap()
            .push((goal.status, goal.iteration));
    }
    fn now_ms(&self) -> i64 {
        self.now.fetch_add(1, Ordering::SeqCst) as i64
    }
}

#[tokio::test]
async fn loops_until_goal_complete() {
    let mut g = active_goal(25);
    let iter = StubIter::completing(3, 100);

    let stop = drive_goal(&mut g, &iter).await;

    assert_eq!(stop, LoopStop::Completed);
    assert_eq!(g.status, GoalStatus::Completed);
    assert_eq!(g.iteration, 3, "three turns ran");
    assert_eq!(g.spent.tokens, 300, "spend accrued across turns");
    assert_eq!(
        iter.saves.lock().unwrap().last().unwrap().0,
        GoalStatus::Completed,
        "final persist is the Completed state"
    );
}

#[tokio::test]
async fn stops_when_iteration_budget_exhausts_before_completion() {
    // Completes only on turn 10, but the budget caps at 2 iterations.
    let mut g = active_goal(2);
    let iter = StubIter::completing(10, 50);

    let stop = drive_goal(&mut g, &iter).await;

    assert_eq!(stop, LoopStop::Exhausted);
    assert_eq!(g.status, GoalStatus::Exhausted);
    assert_eq!(g.iteration, 2, "ran exactly the budgeted iterations");
    assert_eq!(g.spent.tokens, 100);
}

#[tokio::test]
async fn token_budget_exhausts_mid_loop() {
    let mut g = active_goal(25);
    g.budget.max_tokens = Some(150);
    let iter = StubIter::completing(10, 100);

    let stop = drive_goal(&mut g, &iter).await;

    assert_eq!(stop, LoopStop::Exhausted);
    // Turn 1 spends 100 (< 150, continues); turn 2 spends 100 -> 200 >= 150,
    // so the NEXT budget check exhausts. Two turns ran.
    assert_eq!(g.iteration, 2);
    assert_eq!(g.spent.tokens, 200);
}

#[tokio::test]
async fn gate_pause_halts_resumably_without_running_a_turn() {
    let mut g = active_goal(25);
    let mut iter = StubIter::completing(1, 100);
    iter.gate = GateDecision::Pause;

    let stop = drive_goal(&mut g, &iter).await;

    assert_eq!(stop, LoopStop::Paused);
    assert_eq!(g.status, GoalStatus::Paused);
    assert_eq!(g.iteration, 0, "no turn ran before the pause");
    assert_eq!(iter.call_count(), 0);
}

#[tokio::test]
async fn gate_deny_fails_the_goal() {
    let mut g = active_goal(25);
    let mut iter = StubIter::completing(1, 100);
    iter.gate = GateDecision::Deny;

    let stop = drive_goal(&mut g, &iter).await;

    assert_eq!(stop, LoopStop::Failed);
    assert_eq!(g.status, GoalStatus::Failed);
    assert_eq!(g.iteration, 0);
}

#[tokio::test]
async fn iteration_panic_is_isolated_and_fails_the_goal() {
    let mut g = active_goal(25);
    let mut iter = StubIter::completing(10, 100);
    iter.panic_on = Some(1);

    let stop = drive_goal(&mut g, &iter).await;

    assert_eq!(stop, LoopStop::Failed, "a panicking turn ends the loop");
    assert_eq!(g.status, GoalStatus::Failed);
    // The boundary still checkpoints the failed turn (iteration bumped).
    assert_eq!(g.iteration, 1);
}

#[tokio::test]
async fn iteration_failure_fails_the_goal() {
    let mut g = active_goal(25);
    let mut iter = StubIter::completing(10, 100);
    iter.fail_on = Some(2);

    let stop = drive_goal(&mut g, &iter).await;

    assert_eq!(stop, LoopStop::Failed);
    assert_eq!(g.iteration, 2, "failed on the second turn");
}

#[tokio::test]
async fn user_cancel_pauses_the_goal_resumably() {
    // A Stop-button cancel mid-turn must leave the goal Paused (resumable
    // from the last checkpoint), NOT Failed (#753 review blocker 1).
    let mut g = active_goal(25);
    let mut iter = StubIter::completing(10, 100);
    iter.cancel_on = Some(2);

    let stop = drive_goal(&mut g, &iter).await;

    assert_eq!(stop, LoopStop::Paused);
    assert_eq!(g.status, GoalStatus::Paused);
    assert_eq!(g.iteration, 2, "the interrupted turn is checkpointed");
    // Resumable: a subsequent drive continues from here (completes at turn 10,
    // i.e. 8 more turns) rather than being stuck Failed.
    g.status = GoalStatus::Active;
    let iter2 = StubIter::completing(1, 100); // completes immediately on resume
    assert_eq!(drive_goal(&mut g, &iter2).await, LoopStop::Completed);
}

#[tokio::test]
async fn cancel_takes_precedence_over_failed() {
    // An interrupted turn often also surfaces as an error; cancel wins so the
    // goal stays resumable.
    let mut g = active_goal(25);
    let mut iter = StubIter::completing(10, 100);
    iter.cancel_on = Some(1);
    iter.fail_on = Some(1);

    let stop = drive_goal(&mut g, &iter).await;

    assert_eq!(stop, LoopStop::Paused);
    assert_eq!(g.status, GoalStatus::Paused);
}

#[tokio::test]
async fn consumed_steer_is_cleared_on_the_goal() {
    // A steer consumed on iteration 1 must be cleared on the in-memory goal
    // before checkpoint, so it is applied once and not re-persisted (#753 nit
    // 1). Goal completes on turn 2 so we can observe the post-turn-1 state via
    // the persisted checkpoints.
    let mut g = active_goal(25);
    g.pending_steer = Some("focus on the API layer".to_string());
    let mut iter = StubIter::completing(2, 100);
    iter.steer_on = Some(1);

    let stop = drive_goal(&mut g, &iter).await;

    assert_eq!(stop, LoopStop::Completed);
    assert!(
        g.pending_steer.is_none(),
        "consumed steer must be cleared, not left to re-apply"
    );
}

#[tokio::test]
async fn unconsumed_steer_is_preserved() {
    // If a turn does not consume the steer, it stays for a later iteration.
    let mut g = active_goal(25);
    g.pending_steer = Some("later".to_string());
    let iter = StubIter::completing(1, 100); // steer_on = None

    drive_goal(&mut g, &iter).await;

    assert_eq!(g.pending_steer.as_deref(), Some("later"));
}

#[tokio::test]
async fn non_active_goal_is_a_noop() {
    let iter = StubIter::completing(1, 100);
    let mut paused = active_goal(25);
    paused.status = GoalStatus::Paused;

    assert_eq!(drive_goal(&mut paused, &iter).await, LoopStop::Paused);
    assert_eq!(iter.call_count(), 0, "no turn ran");
    assert!(
        iter.saves.lock().unwrap().is_empty(),
        "no persist for a no-op"
    );
}

#[tokio::test]
async fn completion_wins_over_same_iteration_budget_overrun() {
    // Budget of 1 iteration; the single turn both completes AND tips the
    // iteration count to the ceiling. Completion must win.
    let mut g = active_goal(1);
    let iter = StubIter::completing(1, 100);

    let stop = drive_goal(&mut g, &iter).await;

    assert_eq!(stop, LoopStop::Completed);
    assert_eq!(g.status, GoalStatus::Completed);
}
