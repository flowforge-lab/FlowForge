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
    /// Ledger steps this stub reports per turn, keyed by 1-based turn number.
    steps_on: Vec<(u32, LedgerStep)>,
    calls: AtomicU32,
    now: AtomicU32,
    /// One snapshot per `save` call: the goal state persisted at that boundary.
    /// Captures the **ledger** (`GoalLedgerEntry` clones), not just the status,
    /// so a test can prove what actually reached the persist seam — the commit
    /// ordering the disk write depends on (#1226 re-review: AC4).
    saves: Mutex<Vec<SaveSnapshot>>,
}
#[derive(Clone)]
struct SaveSnapshot {
    status: GoalStatus,
    ledger: Vec<GoalLedgerEntry>,
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
            steps_on: Vec::new(),
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
            ledger_steps: self
                .steps_on
                .iter()
                .filter(|(turn, _)| *turn == n)
                .map(|(_, s)| s.clone())
                .collect(),
        }
    }
    fn save(&self, goal: &Goal) {
        self.saves.lock().unwrap().push(SaveSnapshot {
            status: goal.status,
            ledger: goal.ledger.clone(),
        });
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
        iter.saves.lock().unwrap().last().unwrap().status,
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

fn step(claim: &str) -> LedgerStep {
    LedgerStep {
        id: None,
        claim: claim.to_string(),
        verdict: None,
        evidence: Vec::new(),
    }
}

#[tokio::test]
async fn a_recorded_step_lands_in_the_ledger() {
    let mut iter = StubIter::completing(2, 100);
    iter.steps_on = vec![(1, step("ran the suite"))];
    let mut g = Goal::new("s", "obj", 0);

    drive_goal(&mut g, &iter).await;

    assert_eq!(g.ledger.len(), 1, "the step must be committed");
    assert_eq!(g.ledger[0].claim, "ran the suite");
    assert!(
        !g.ledger[0].id.is_empty(),
        "an appended step needs an addressable id"
    );
}

/// AC4 (#1225): the committed step must survive to the **persist seam**, not just
/// the in-memory goal. `a_recorded_step_lands_in_the_ledger` asserts `g.ledger`
/// after the loop returns, which cannot see whether `commit_ledger_steps` runs
/// *before* the `save` that writes it — reorder them and that test stays green
/// while the real disk write drops the turn's steps. This asserts the ledger
/// snapshot captured at each `save` call, so the commit-before-save ordering is
/// pinned by a test rather than by a comment alone (#1226 re-review, Blocker 1).
#[tokio::test]
async fn a_committed_step_is_present_in_the_persisted_ledger_snapshot() {
    // The step is reported on the *completing* turn, so its commit and the
    // completing `save` race at the same boundary — this is what makes the test
    // sensitive to committing after the save rather than before it.
    let mut iter = StubIter::completing(1, 100);
    iter.steps_on = vec![(1, step("ran the suite"))];
    let mut g = Goal::new("s", "obj", 0);

    drive_goal(&mut g, &iter).await;

    let saves = iter.saves.lock().unwrap();
    let last = saves.last().expect("the completing run must persist");
    assert_eq!(
        last.status,
        GoalStatus::Completed,
        "the final save is the completed state"
    );
    assert_eq!(
        last.ledger.len(),
        1,
        "the committed step must be in the ledger at the moment it is persisted, \
         not committed only after the save (AC4)"
    );
    assert_eq!(last.ledger[0].claim, "ran the suite");
}

/// AC5 (#1225): a subsequent turn's system prompt contains the recorded claim.
/// The render side (`goal_block_caps_ledger_to_last_five`) proves the template
/// surfaces ledger entries, but nothing joined the *write* path to it — so a
/// change that committed steps in a shape the prompt does not render would pass
/// both sides' tests. This drives the real write path (`commit_ledger_steps`)
/// and then renders, asserting the claim reaches the prompt end to end (#1226).
#[test]
fn a_committed_claim_reaches_the_next_turns_system_prompt() {
    use crate::system_prompt::{build_system_prompt, SystemPromptInputs, UserContext};
    use ff_core::Mode;
    use ff_skills::SkillRegistry;

    let mut g = Goal::new("s", "objective", 0);
    let claimed = LedgerStep {
        id: None,
        claim: "migrated the schema".into(),
        verdict: Some(Verdict::Match),
        evidence: Vec::new(),
    };
    commit_ledger_steps(&mut g, &[claimed], 0);

    let reg = SkillRegistry::new();
    let user = UserContext::now();
    let prompt = build_system_prompt(&SystemPromptInputs {
        goal: Some(&g),
        ..SystemPromptInputs::new(&reg, &[], &user, Mode::Auto)
    })
    .full();

    assert!(
        prompt.contains("migrated the schema"),
        "the committed claim must appear in the rendered system prompt (AC5); got:\n{prompt}"
    );
}

/// The write path must be inert when the agent records nothing — otherwise every
/// turn would invent a ledger entry.
#[tokio::test]
async fn a_turn_without_steps_leaves_the_ledger_empty() {
    let iter = StubIter::completing(2, 100);
    let mut g = Goal::new("s", "obj", 0);

    drive_goal(&mut g, &iter).await;

    assert!(g.ledger.is_empty());
}

/// Re-recording the same id must revise that entry, not append a near-duplicate:
/// the ledger is a record of steps, not of tool calls.
#[tokio::test]
async fn re_recording_an_id_updates_in_place() {
    let mut iter = StubIter::completing(3, 100);
    let first = LedgerStep {
        id: Some("s1".into()),
        claim: "checking the FK".into(),
        verdict: None,
        evidence: Vec::new(),
    };
    let revised = LedgerStep {
        id: Some("s1".into()),
        claim: "checking the FK".into(),
        verdict: Some(Verdict::Match),
        evidence: vec!["277 tests pass".into()],
    };
    iter.steps_on = vec![(1, first), (2, revised)];
    let mut g = Goal::new("s", "obj", 0);

    drive_goal(&mut g, &iter).await;

    assert_eq!(g.ledger.len(), 1, "same id must not append a second entry");
    assert_eq!(g.ledger[0].verdict, Some(Verdict::Match));
    assert_eq!(g.ledger[0].evidence, vec!["277 tests pass".to_string()]);
    assert_eq!(
        g.ledger[0].status,
        StepStatus::Done,
        "a Match closes the step"
    );
}

/// An id the ledger has never seen must still be recorded. Dropping it would lose
/// the agent's work to a typo.
#[tokio::test]
async fn an_unknown_id_is_appended_not_dropped() {
    let mut iter = StubIter::completing(2, 100);
    iter.steps_on = vec![(
        1,
        LedgerStep {
            id: Some("never-seen".into()),
            claim: "orphan".into(),
            verdict: None,
            evidence: Vec::new(),
        },
    )];
    let mut g = Goal::new("s", "obj", 0);

    drive_goal(&mut g, &iter).await;

    assert_eq!(g.ledger.len(), 1);
    assert_eq!(g.ledger[0].id, "never-seen");
}

/// The point of the ledger: a step that could not be checked stays open, so a
/// later iteration can see unfinished business (`Verdict` docs, #74).
#[tokio::test]
async fn only_a_match_closes_a_step() {
    for (verdict, expected) in [
        (Verdict::Match, StepStatus::Done),
        (Verdict::Drift, StepStatus::Active),
        (Verdict::Unverifiable, StepStatus::Active),
    ] {
        let mut iter = StubIter::completing(2, 100);
        iter.steps_on = vec![(
            1,
            LedgerStep {
                id: None,
                claim: "c".into(),
                verdict: Some(verdict),
                evidence: Vec::new(),
            },
        )];
        let mut g = Goal::new("s", "obj", 0);

        drive_goal(&mut g, &iter).await;

        assert_eq!(g.ledger[0].status, expected, "verdict {verdict:?}");
    }
}

/// A panicked turn yields the default outcome, which must carry no steps — the
/// loop must not fabricate a ledger entry for a turn that never reported.
#[tokio::test]
async fn a_panicked_turn_records_no_step() {
    let mut iter = StubIter::completing(5, 100);
    iter.panic_on = Some(1);
    iter.steps_on = vec![(1, step("should never land"))];
    let mut g = Goal::new("s", "obj", 0);

    drive_goal(&mut g, &iter).await;

    assert!(g.ledger.is_empty(), "a panicked turn must not commit steps");
}

// ---- TurnLedger: the shared observation state machine (#1226) ----

fn started(call_id: &str, name: &str, args: serde_json::Value) -> AgentEvent {
    AgentEvent::ToolCallStarted {
        message_id: "m".into(),
        call_id: call_id.into(),
        name: name.into(),
        args,
    }
}

fn finished(call_id: &str, success: bool) -> AgentEvent {
    AgentEvent::ToolCallFinished {
        message_id: "m".into(),
        call_id: call_id.into(),
        success,
        result: String::new(),
        observer_intent: None,
    }
}

fn step_started(call_id: &str, claim: &str) -> AgentEvent {
    started(
        call_id,
        ff_tools::GOAL_STEP_TOOL_NAME,
        serde_json::json!({ "claim": claim, "verdict": "match" }),
    )
}

/// A `goal_step` is committed only after its call finishes successfully:
/// started-then-finished lands exactly one step (AC2/AC3).
#[test]
fn a_successful_goal_step_commits_once() {
    let mut l = TurnLedger::new();
    l.observe(&step_started("c1", "did the thing"));
    assert!(
        !l.completed(),
        "no goal_complete seen; must stay incomplete"
    );
    l.observe(&finished("c1", true));

    let steps = l.into_steps();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].claim, "did the thing");
}

/// AC4: a rejected or failed `goal_step` must leave nothing in the ledger — a
/// claim the tool did not accept must not be recorded.
#[test]
fn a_failed_goal_step_records_nothing() {
    let mut l = TurnLedger::new();
    l.observe(&step_started("c1", "unverified claim"));
    l.observe(&finished("c1", false));

    assert!(
        l.into_steps().is_empty(),
        "a failed goal_step must not be committed"
    );
}

/// A `ToolCallStarted` whose args do not parse into a step is dropped at the
/// door, so its later finish commits nothing.
#[test]
fn an_unparseable_goal_step_is_ignored() {
    let mut l = TurnLedger::new();
    l.observe(&started(
        "c1",
        ff_tools::GOAL_STEP_TOOL_NAME,
        serde_json::json!({ "claim": "" }), // empty claim → parse returns None
    ));
    l.observe(&finished("c1", true));

    assert!(l.into_steps().is_empty());
}

/// The guard that the CLI `ledger` module's `another_tools_call_is_not_recorded`
/// used to cover, restored on `TurnLedger` (#1226 re-review nit): only the
/// goal-step tool produces a step. A different tool that happens to carry a
/// `claim`-shaped argument must record nothing — otherwise dropping the
/// `if name == GOAL_STEP_TOOL_NAME` guard on the `Started` arm would silently
/// turn any such call into a ledger entry, with no test failing.
#[test]
fn a_non_goal_step_tool_call_is_not_recorded() {
    let mut l = TurnLedger::new();
    l.observe(&started(
        "c1",
        "bash",
        serde_json::json!({ "claim": "not a real step", "verdict": "match" }),
    ));
    l.observe(&finished("c1", true));

    assert!(
        l.into_steps().is_empty(),
        "only the goal-step tool records a step; another tool must not"
    );
}

/// `goal_complete` flips `completed` only on a successful finish of the same
/// call id that started it (AC1).
#[test]
fn goal_complete_requires_a_successful_finish_of_its_own_call() {
    let mut l = TurnLedger::new();
    l.observe(&started(
        "gc",
        ff_tools::GOAL_COMPLETE_TOOL_NAME,
        serde_json::json!({}),
    ));
    // A different call finishing must not complete the goal.
    l.observe(&finished("other", true));
    assert!(!l.completed(), "an unrelated finish must not complete");
    // A failed goal_complete must not complete either.
    l.observe(&finished("gc", false));
    assert!(!l.completed(), "a failed goal_complete must not complete");

    let mut l = TurnLedger::new();
    l.observe(&started(
        "gc",
        ff_tools::GOAL_COMPLETE_TOOL_NAME,
        serde_json::json!({}),
    ));
    l.observe(&finished("gc", true));
    assert!(l.completed());
}

/// Interleaved calls each resolve against their own id: several steps in one
/// turn all land, in observation order (AC5 — a turn may record many steps).
#[test]
fn interleaved_steps_each_resolve_by_call_id() {
    let mut l = TurnLedger::new();
    l.observe(&step_started("c1", "first"));
    l.observe(&step_started("c2", "second"));
    l.observe(&finished("c2", true));
    l.observe(&finished("c1", true));

    let claims: Vec<_> = l.into_steps().into_iter().map(|s| s.claim).collect();
    // Pushed in finish order: c2 resolved before c1.
    assert_eq!(claims, vec!["second".to_string(), "first".to_string()]);
}

/// #1226: a positional mint must not collide with an id an earlier step passed
/// explicitly, or the two entries become un-addressable.
#[test]
fn mint_step_id_skips_an_explicit_collision() {
    let mut g = Goal::new("s", "obj", 0);
    // Empty ledger, len==0 → natural next slot is "step-1".
    assert_eq!(mint_step_id(&g), "step-1");

    // An explicit id occupying the slot a later positional mint would take.
    g.ledger.push(GoalLedgerEntry {
        id: "step-1".into(),
        claim: "explicit".into(),
        status: StepStatus::Active,
        action: None,
        verdict: None,
        evidence: Vec::new(),
        next: None,
        created_ms: 0,
        updated_ms: 0,
    });
    // len==1 → base "step-2"; no collision, returns it.
    assert_eq!(mint_step_id(&g), "step-2");

    // Now the collision case: base would be "step-2" but it is already taken.
    g.ledger.push(GoalLedgerEntry {
        id: "step-2".into(),
        claim: "also explicit".into(),
        status: StepStatus::Active,
        action: None,
        verdict: None,
        evidence: Vec::new(),
        next: None,
        created_ms: 0,
        updated_ms: 0,
    });
    // len==2 → base "step-3", which is free.
    assert_eq!(mint_step_id(&g), "step-3");
}

/// The bump branch itself: when the positional base slot is already occupied by
/// an explicit id, the mint advances past it rather than colliding (#1226).
#[test]
fn mint_step_id_advances_past_an_occupied_base_slot() {
    let mut g = Goal::new("s", "obj", 0);
    // One entry → len==1 → positional base is "step-2". Occupy exactly that slot.
    g.ledger.push(GoalLedgerEntry {
        id: "step-2".into(),
        claim: "explicit, occupies the base slot".into(),
        status: StepStatus::Active,
        action: None,
        verdict: None,
        evidence: Vec::new(),
        next: None,
        created_ms: 0,
        updated_ms: 0,
    });
    // Base "step-2" collides → must advance to the free "step-3".
    assert_eq!(mint_step_id(&g), "step-3");
}

/// The collision guard end-to-end through `commit_ledger_steps`: an explicit
/// `step-2` followed by a no-id step must not both claim `step-2`.
#[test]
fn committing_a_no_id_step_after_an_explicit_collision_stays_addressable() {
    let mut g = Goal::new("s", "obj", 0);
    let explicit = LedgerStep {
        id: Some("step-2".into()),
        claim: "explicit second".into(),
        verdict: None,
        evidence: Vec::new(),
    };
    let no_id = LedgerStep {
        id: None,
        claim: "auto".into(),
        verdict: None,
        evidence: Vec::new(),
    };
    commit_ledger_steps(&mut g, &[explicit, no_id], 0);

    let ids: Vec<_> = g.ledger.iter().map(|e| e.id.clone()).collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "ids must be unique and addressable");
    assert!(ids.contains(&"step-2".to_string()));
}
