//! Goal mode: the durable objective + evidence-first ledger (RFC 0020, #715).
//!
//! A [`Goal`] is the persistent state of a single-session autonomous objective
//! loop. It is deliberately pure data + a small path-injected JSON store: a
//! goal's *state* is independent of how its actions are gated (the permission
//! matrix, #682) or driven (the loop, #716), so this layer builds and tests on
//! its own.
//!
//! ## Loop-state vs conversation-state (#74)
//!
//! The transcript is conversation state; a goal is *loop* state. They are kept
//! separate on purpose: the ephemeral `todo` tool is the current-turn
//! scratchpad, while the [`GoalLedger`] is a durable, evidence-first record a
//! fresh-context iteration can reconstruct progress from without trusting the
//! previous model's prose summary. This is the anti-context-rot mechanism: each
//! iteration reads the ledger, does one thing, and writes back a result **with
//! evidence and a verdict** — an explicit [`Verdict::Unverifiable`] rather than
//! silent confidence. (Design adopted from the #74 discussion, incl. external
//! contributor @HarperZ9's ledger-entry shape.)

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Default iteration ceiling for a goal loop (RFC 0020 §3, §10). One iteration is a
/// full `run_turn` to a terminal answer (itself many tool sub-turns), so this is a
/// coarse safety cap, not a work estimate. It is also the *only* budget dimension
/// bounded by default (`max_tokens` / `max_wall_ms` are optional and unbounded), so
/// it doubles as the runaway-cost backstop; kept moderate and overridable per-goal.
pub const DEFAULT_MAX_ITERATIONS: u32 = 40;

/// A persistent autonomous objective bound to one session (#683, RFC 0020 §3).
///
/// Persisted to `<goals_dir>/<session_id>.json`. Only `session_id`, `objective`,
/// and `status` are truly required to read a file back; every other field has a
/// serde default so an older/partial file stays forward-compatible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct Goal {
    pub session_id: String,
    /// The objective text the loop iterates toward.
    pub objective: String,
    pub status: GoalStatus,
    /// Completed iterations so far; bumped at each iteration boundary.
    #[serde(default)]
    pub iteration: u32,
    #[serde(default)]
    pub budget: GoalBudget,
    /// Cumulative usage across all iterations, checked against `budget`.
    #[serde(default)]
    pub spent: GoalSpend,
    /// The evidence-first ledger (#74). Each fresh-context iteration reads it,
    /// advances one entry, and writes back result + evidence + verdict. Empty
    /// until the loop records its first step.
    #[serde(default)]
    pub ledger: Vec<GoalLedgerEntry>,
    /// A user steer to fold into the next iteration (RFC 0020 §6); cleared once
    /// consumed. Distinct from a normal turn — it refines the objective without
    /// racing the loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pending_steer: Option<String>,
    #[serde(default)]
    #[ts(type = "number")]
    pub created_ms: i64,
    #[serde(default)]
    #[ts(type = "number")]
    pub updated_ms: i64,
}

/// Lifecycle state of a goal (RFC 0020 §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum GoalStatus {
    /// The loop is self-continuing.
    Active,
    /// User-paused (or halted on an `ask_user`-class decision) at a boundary;
    /// resumable.
    Paused,
    /// The agent judged the objective met (`goal_complete`).
    Completed,
    /// A stop condition or unrecoverable error ended it.
    Failed,
    /// A budget dimension (iterations / tokens / wall-clock) was exhausted before
    /// completion.
    Exhausted,
}

/// Stop conditions for a goal loop (RFC 0020 §3). A `None` optional dimension is
/// unbounded; `max_iterations` always has a value (default
/// [`DEFAULT_MAX_ITERATIONS`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct GoalBudget {
    pub max_iterations: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub max_wall_ms: Option<i64>,
}

impl Default for GoalBudget {
    fn default() -> Self {
        Self {
            max_iterations: DEFAULT_MAX_ITERATIONS,
            max_tokens: None,
            max_wall_ms: None,
        }
    }
}

/// Cumulative spend across a goal's iterations (RFC 0020 §3, §10: per-goal, not
/// per-iteration).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct GoalSpend {
    #[serde(default)]
    #[ts(type = "number")]
    pub tokens: u64,
    #[serde(default)]
    #[ts(type = "number")]
    pub wall_ms: i64,
}

/// One evidence-first step in the goal ledger (#74; shape adopted from the
/// discussion, incl. @HarperZ9). A fresh iteration reconstructs progress from
/// these entries rather than a prose summary, and a human can see exactly where
/// the loop moved from evidence to judgment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct GoalLedgerEntry {
    /// Stable id so a step can be updated in place across iterations.
    pub id: String,
    pub status: StepStatus,
    /// What this step is supposed to prove or change.
    pub claim: String,
    /// What the agent attempted. `None` until the step is acted on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub action: Option<String>,
    /// Evidence pointers: command output, a test path, a diff, a URL, an artifact
    /// id. Empty until the step produces evidence.
    #[serde(default)]
    pub evidence: Vec<String>,
    /// The verdict once checked. `None` while pending/active. An explicit
    /// [`Verdict::Unverifiable`] is required rather than omission, so the next
    /// run never inherits confidence without evidence (#74).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub verdict: Option<Verdict>,
    /// What the loop should do next for this step. `NextAction::AskUser` is the
    /// sanctioned circuit breaker (#74) and the join point with the headless
    /// `needs_attention` outcome (RFC 0017 §3.1/§8.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next: Option<NextAction>,
    #[serde(default)]
    #[ts(type = "number")]
    pub created_ms: i64,
    #[serde(default)]
    #[ts(type = "number")]
    pub updated_ms: i64,
}

/// Progress state of a single ledger step (#74).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum StepStatus {
    Pending,
    Active,
    Blocked,
    Done,
}

/// The outcome of checking a step's claim against its evidence (#74). The
/// explicit `Unverifiable` value is the point of the ledger: a step that could
/// not be checked is recorded as such, never quietly treated as done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum Verdict {
    /// Evidence supports the claim.
    Match,
    /// Evidence contradicts the claim.
    Drift,
    /// The claim could not be checked from the available evidence.
    Unverifiable,
}

/// What the loop should do next for a step (#74).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum NextAction {
    /// Continue the loop toward the objective.
    Resume,
    /// Stop and surface for a human decision (the circuit breaker).
    AskUser,
    /// Re-attempt this step.
    Retry,
    /// End the loop.
    Stop,
}

impl Goal {
    /// A fresh, `Active` goal with the default budget and an empty ledger.
    pub fn new(session_id: impl Into<String>, objective: impl Into<String>, now_ms: i64) -> Self {
        Self {
            session_id: session_id.into(),
            objective: objective.into(),
            status: GoalStatus::Active,
            iteration: 0,
            budget: GoalBudget::default(),
            spent: GoalSpend::default(),
            ledger: Vec::new(),
            pending_steer: None,
            created_ms: now_ms,
            updated_ms: now_ms,
        }
    }

    /// Append a new ledger step, stamping its timestamps and touching the goal's
    /// `updated_ms`.
    pub fn append_entry(&mut self, mut entry: GoalLedgerEntry, now_ms: i64) {
        entry.created_ms = now_ms;
        entry.updated_ms = now_ms;
        self.ledger.push(entry);
        self.updated_ms = now_ms;
    }

    /// Update the ledger step with `id` in place via `f`, refreshing its
    /// `updated_ms` and the goal's. Returns `false` if no such step exists.
    pub fn update_entry(
        &mut self,
        id: &str,
        now_ms: i64,
        f: impl FnOnce(&mut GoalLedgerEntry),
    ) -> bool {
        let Some(entry) = self.ledger.iter_mut().find(|e| e.id == id) else {
            return false;
        };
        f(entry);
        entry.updated_ms = now_ms;
        self.updated_ms = now_ms;
        true
    }

    /// Close one iteration boundary (RFC 0020 §5.2): add this iteration's spend
    /// and bump the completed-iteration count + `updated_ms`.
    pub fn checkpoint(&mut self, iteration_tokens: u64, iteration_wall_ms: i64, now_ms: i64) {
        self.spent.tokens = self.spent.tokens.saturating_add(iteration_tokens);
        self.spent.wall_ms = self.spent.wall_ms.saturating_add(iteration_wall_ms);
        self.iteration = self.iteration.saturating_add(1);
        self.updated_ms = now_ms;
    }

    /// Whether any budget dimension is now exhausted (RFC 0020 §5.2). Does not
    /// mutate `status` — the loop decides the terminal transition.
    pub fn budget_exhausted(&self) -> bool {
        self.iteration >= self.budget.max_iterations
            || self
                .budget
                .max_tokens
                .is_some_and(|c| self.spent.tokens >= c)
            || self
                .budget
                .max_wall_ms
                .is_some_and(|c| self.spent.wall_ms >= c)
    }
}

/// Durable per-session goal store (RFC 0020 §3). Path-injected — the caller
/// supplies the directory (the desktop layer passes `~/.flowforge/goals/`), so
/// the store carries no home-dir dependency and is fully testable with a
/// tempdir. Mirrors the atomic temp-file + rename write used for the other
/// `~/.flowforge` config files.
#[derive(Debug, Clone)]
pub struct GoalStore {
    dir: PathBuf,
}

impl GoalStore {
    /// A store rooted at `dir` (the directory holding `<session_id>.json` files).
    /// The directory is created lazily on the first save.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path_for(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{session_id}.json"))
    }

    /// Load the goal for `session_id`, or `None` if there is no checkpoint file.
    /// A read or parse error is surfaced so a corrupt file is not silently
    /// treated as "no goal".
    pub fn load(&self, session_id: &str) -> io::Result<Option<Goal>> {
        let path = self.path_for(session_id);
        match fs::read_to_string(&path) {
            Ok(raw) => Ok(Some(serde_json::from_str(&raw).map_err(io::Error::from)?)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Persist `goal` atomically (temp-file + rename on the same directory, so a
    /// crash mid-write never leaves a partially written checkpoint).
    pub fn save(&self, goal: &Goal) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.path_for(&goal.session_id);
        let json = serde_json::to_string_pretty(goal).map_err(io::Error::from)?;
        write_atomic(&path, &json)
    }

    /// Delete the goal checkpoint for `session_id` (abort / `goal_clear`).
    /// Absent file is a no-op.
    pub fn delete(&self, session_id: &str) -> io::Result<()> {
        match fs::remove_file(self.path_for(session_id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Atomically write `contents` to `path`: write a sibling `.tmp`, then rename it
/// over the target. Rename is atomic on the same filesystem, so a reader never
/// observes a half-written file (mirrors the desktop `write_atomic`).
fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(id: &str) -> GoalLedgerEntry {
        GoalLedgerEntry {
            id: id.into(),
            status: StepStatus::Pending,
            claim: "the build passes".into(),
            action: None,
            evidence: Vec::new(),
            verdict: None,
            next: None,
            created_ms: 0,
            updated_ms: 0,
        }
    }

    #[test]
    fn default_budget_caps_iterations_at_40() {
        let b = GoalBudget::default();
        assert_eq!(b.max_iterations, DEFAULT_MAX_ITERATIONS);
        assert_eq!(b.max_iterations, 40);
        assert!(b.max_tokens.is_none());
        assert!(b.max_wall_ms.is_none());
    }

    #[test]
    fn goal_round_trips_through_json() {
        let mut g = Goal::new("sess-1", "ship goal mode", 1_000);
        g.append_entry(sample_entry("step-1"), 1_100);
        g.update_entry("step-1", 1_200, |e| {
            e.status = StepStatus::Done;
            e.action = Some("ran cargo test".into());
            e.evidence.push("crates/ff-core: 42 passed".into());
            e.verdict = Some(Verdict::Match);
            e.next = Some(NextAction::Resume);
        });
        let json = serde_json::to_string(&g).unwrap();
        let back: Goal = serde_json::from_str(&json).unwrap();
        assert_eq!(g, back);
    }

    #[test]
    fn unverifiable_verdict_round_trips_as_snake_case() {
        let mut e = sample_entry("s");
        e.verdict = Some(Verdict::Unverifiable);
        e.next = Some(NextAction::AskUser);
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""verdict":"unverifiable""#), "got: {json}");
        assert!(json.contains(r#""next":"ask_user""#), "got: {json}");
        let back: GoalLedgerEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn partial_file_loads_via_serde_defaults() {
        // Only the three truly-required fields present; everything else defaults.
        let raw = r#"{"sessionId":"s","objective":"do the thing","status":"active"}"#;
        let g: Goal = serde_json::from_str(raw).unwrap();
        assert_eq!(g.iteration, 0);
        assert_eq!(g.budget.max_iterations, 40);
        assert_eq!(g.spent.tokens, 0);
        assert!(g.ledger.is_empty());
        assert!(g.pending_steer.is_none());
    }

    #[test]
    fn append_and_update_entry_touch_timestamps() {
        let mut g = Goal::new("s", "obj", 1);
        g.append_entry(sample_entry("a"), 10);
        assert_eq!(g.ledger[0].created_ms, 10);
        assert_eq!(g.ledger[0].updated_ms, 10);
        assert_eq!(g.updated_ms, 10);

        let updated = g.update_entry("a", 20, |e| e.status = StepStatus::Active);
        assert!(updated);
        assert_eq!(g.ledger[0].status, StepStatus::Active);
        assert_eq!(g.ledger[0].updated_ms, 20);
        assert_eq!(g.ledger[0].created_ms, 10, "created is not clobbered");
        assert_eq!(g.updated_ms, 20);

        assert!(!g.update_entry("missing", 30, |_| {}), "no such id");
    }

    #[test]
    fn checkpoint_accumulates_spend_and_bumps_iteration() {
        let mut g = Goal::new("s", "obj", 0);
        g.budget.max_tokens = Some(100);
        g.checkpoint(30, 500, 10);
        g.checkpoint(40, 600, 20);
        assert_eq!(g.iteration, 2);
        assert_eq!(g.spent.tokens, 70);
        assert_eq!(g.spent.wall_ms, 1_100);
        assert_eq!(g.updated_ms, 20);
        assert!(!g.budget_exhausted(), "70 < 100 tokens, 2 < 25 iters");
        g.checkpoint(40, 0, 30);
        assert!(g.budget_exhausted(), "110 >= 100 tokens");
    }

    #[test]
    fn budget_exhausted_on_iteration_ceiling() {
        let mut g = Goal::new("s", "obj", 0);
        g.budget.max_iterations = 2;
        assert!(!g.budget_exhausted());
        g.checkpoint(0, 0, 1);
        g.checkpoint(0, 0, 2);
        assert!(g.budget_exhausted(), "2 >= 2");
    }

    #[test]
    fn store_save_load_delete_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = GoalStore::new(dir.path().join("goals"));
        assert!(store.load("s").unwrap().is_none(), "absent before save");

        let g = Goal::new("s", "obj", 42);
        store.save(&g).unwrap();
        let loaded = store.load("s").unwrap().expect("present after save");
        assert_eq!(loaded, g);

        store.delete("s").unwrap();
        assert!(store.load("s").unwrap().is_none(), "gone after delete");
        store.delete("s").unwrap(); // idempotent
    }

    #[test]
    fn store_save_leaves_no_tmp_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("goals");
        let store = GoalStore::new(&root);
        store.save(&Goal::new("s", "obj", 1)).unwrap();
        let entries: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["s.json".to_string()], "only the final file");
    }

    #[test]
    fn store_corrupt_file_surfaces_error_not_silent_none() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("goals");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("s.json"), "{ not valid json").unwrap();
        let store = GoalStore::new(&root);
        assert!(store.load("s").is_err(), "corrupt file is an error");
    }
}
