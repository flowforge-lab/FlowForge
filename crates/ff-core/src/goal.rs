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
    /// A shell command the loop runs to *verify* a claimed completion before it
    /// accepts `goal_complete` (RFC 0020 §5.1, #684 D3). `None` keeps the pre-D3
    /// behaviour — a `goal_complete` is trusted as-is — so non-code goals (a
    /// research write-up) are unaffected. When set, the loop runs it on a
    /// completion signal: a green exit accepts `Completed`; a non-zero exit
    /// rejects the claim, records a `Drift` ledger entry carrying the command's
    /// output as evidence, and lets the loop keep iterating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub verify_cmd: Option<String>,
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
            verify_cmd: None,
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

/// The canonical goals directory: `dirs::config_dir()/flowforge/goals` (RFC
/// 0020 §5). Shared by the desktop and the CLI so a goal created in either
/// surface is visible to the other.
pub fn goal_store_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flowforge")
        .join("goals")
}

/// Durable per-session goal store (RFC 0020 §3). Path-injected — the caller
/// supplies the directory (the desktop layer passes `~/.flowforge/goals/` via
/// [`goal_store_dir`]), so the store carries no home-dir dependency and is
/// fully testable with a tempdir. Mirrors the atomic temp-file + rename write
/// used for the other `~/.flowforge` config files.
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

    /// The directory this store reads from / writes to.
    pub fn dir(&self) -> &Path {
        &self.dir
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

    /// The `Active` goals persisted in this store, for resume-on-restart (#802).
    ///
    /// Best-effort by design: a missing or unreadable directory yields an empty
    /// vec, and a single unreadable or unparseable checkpoint is logged and
    /// skipped rather than failing the whole scan -- a corrupt file must never
    /// block boot from resuming the healthy goals. The atomic-write sibling
    /// (`*.json.tmp`) and any non-`.json` entry are ignored.
    pub fn list_active(&self) -> Vec<Goal> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                tracing::warn!(dir = %self.dir.display(), error = %e, "goal store: cannot scan for active goals");
                return Vec::new();
            }
        };
        let mut active = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let raw = match fs::read_to_string(&path) {
                Ok(raw) => raw,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "goal store: skipping unreadable checkpoint");
                    continue;
                }
            };
            match serde_json::from_str::<Goal>(&raw) {
                Ok(goal) if goal.status == GoalStatus::Active => active.push(goal),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "goal store: skipping unparseable checkpoint");
                }
            }
        }
        // `read_dir` yields entries in a filesystem-dependent order, so sort by
        // session id for a deterministic resume order (and reproducible tests).
        active.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        active
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
mod tests;
