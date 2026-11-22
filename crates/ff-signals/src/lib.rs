//! Signal bus + per-skill telemetry aggregates. FlowForge emits signals during a
//! turn; `ff-signals` folds the skill-telemetry ones into rolling per-skill
//! aggregates (RFC 0001 §8) and persists them. NeuroForge consumes the
//! intention/outcome signals for cognitive-health analysis.
//!
//! M3.5 makes this a real substrate: `SkillActivated`/`SkillCompleted` (defined in
//! `ff-core::events`) feed a [`SignalStore`] that maintains, per skill, activation
//! and completion counts, mean token cost, mean turns, mean latency, and success
//! rate. The aggregates back the manual optimize flow's cost estimates. The
//! autonomous trigger that reads them is deferred to M4; the schema is kept
//! forward-compatible.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ff_memory::{MemoryIndex, MemoryOutcomeSink, Verdict};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use ff_core::events::{IntentionSignal, OutcomeSignal, SkillActivated, SkillCompleted};
pub use ff_core::SessionStatus;

/// A signal emitted by FlowForge. The intention variant feeds NeuroForge; the
/// outcome variant feeds NeuroForge *and* (when the store is wired) memory
/// settlement; the skill-telemetry variants feed [`SignalStore`]. Non-exhaustive
/// in spirit: new variants (e.g. an M4 autonomous-trigger signal) extend this
/// without replacing it.
#[derive(Debug, Clone)]
pub enum Signal {
    Intention(IntentionSignal),
    Outcome(OutcomeSignal),
    SkillActivated(SkillActivated),
    SkillCompleted(SkillCompleted),
}

/// Rolling telemetry for one skill (RFC 0001 §8). Counts are cumulative; the means
/// are maintained incrementally (Welford-style running mean) so the store never
/// holds an unbounded sum. `successRate` is `successes / completions`. Means are
/// `0.0` until the first completion.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct SkillAggregate {
    pub skill: String,
    /// Times the skill was active at the start of a turn.
    pub activations: u32,
    /// Turns that finished with the skill active (the denominator for the means).
    pub completions: u32,
    /// Of `completions`, how many ended cleanly (not error/cancel).
    pub successes: u32,
    /// Rolling mean of the per-turn token cost proxy.
    pub mean_tokens: f64,
    /// Rolling mean of agent loop iterations per turn.
    pub mean_turns: f64,
    /// Rolling mean of wall-clock turn latency, in milliseconds.
    pub mean_latency_ms: f64,
    /// `successes / completions`, or `0.0` before the first completion.
    pub success_rate: f64,
}

impl SkillAggregate {
    fn new(skill: &str) -> Self {
        Self {
            skill: skill.to_string(),
            ..Default::default()
        }
    }

    fn record_completed(&mut self, ev: &SkillCompleted) {
        self.completions += 1;
        if ev.success {
            self.successes += 1;
        }
        let n = f64::from(self.completions);
        // Incremental (running) mean: mean += (x - mean) / n.
        self.mean_tokens += (f64::from(ev.tokens) - self.mean_tokens) / n;
        self.mean_turns += (f64::from(ev.turns) - self.mean_turns) / n;
        self.mean_latency_ms += (f64::from(ev.latency_ms) - self.mean_latency_ms) / n;
        self.success_rate = f64::from(self.successes) / n;
    }
}

/// Per-skill aggregates, optionally persisted to a JSON file. Best-effort I/O: a
/// load failure starts empty and a save failure is logged-and-ignored, mirroring the
/// app's other config persistence — telemetry must never break a turn.
///
/// In addition to folding skill telemetry, the store can be wired to a
/// [`MemoryIndex`] via [`with_memory_index`]; when it is, an ingested
/// `Signal::Outcome` settles the session's touched chunks through
/// [`MemoryOutcomeSink`] (RFC 0022 block C, #1305 Phase 2).
///
/// [`with_memory_index`]: SignalStore::with_memory_index
#[derive(Default)]
pub struct SignalStore {
    aggregates: HashMap<String, SkillAggregate>,
    path: Option<PathBuf>,
    memory_index: Option<Arc<dyn MemoryIndex>>,
}

impl std::fmt::Debug for SignalStore {
    /// [`dyn MemoryIndex`](MemoryIndex) is not `Debug`, so the index is reported
    /// only as present/absent.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalStore")
            .field("aggregates", &self.aggregates)
            .field("path", &self.path)
            .field(
                "memory_index",
                &if self.memory_index.is_some() {
                    "Some(_)"
                } else {
                    "None"
                },
            )
            .finish()
    }
}

impl SignalStore {
    /// An in-memory store with no persistence (tests, or a host with no home dir).
    pub fn new() -> Self {
        Self::default()
    }

    /// Wire the store to a [`MemoryIndex`] so that an ingested `Signal::Outcome`
    /// settles the session's touched chunks (RFC 0022 block C). See
    /// [`ingest`](Self::ingest). Hosts that own a memory index during the
    /// outbound leg should call this on construction; hosts without one (or with
    /// memory settlement routed elsewhere) simply omit it and outcome signals are
    /// ignored, exactly as before.
    pub fn with_memory_index(mut self, index: Arc<dyn MemoryIndex>) -> Self {
        self.memory_index = Some(index);
        self
    }

    /// Load aggregates from `path` (best-effort) and remember it for subsequent
    /// saves. A missing or unparseable file starts empty.
    pub fn load(path: PathBuf) -> Self {
        let aggregates = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, SkillAggregate>>(&s).ok())
            .unwrap_or_default();
        Self {
            aggregates,
            path: Some(path),
            memory_index: None,
        }
    }

    /// Route a [`Signal`] into the store. Skill-telemetry variants update aggregates;
    /// intention/outcome variants are NeuroForge's concern except that, when a
    /// [`MemoryIndex`] is wired via [`with_memory_index`], a `Signal::Outcome`
    /// settles the session's touched chunks (RFC 0022 block C, #1305 Phase 2) — a
    /// `Done` reinforces them, an `Abandoned` suppresses their promotion, and an
    /// `Active` is undecided (no-op). Best-effort: a settlement failure is logged
    /// and the store's telemetry is unaffected.
    ///
    /// [`with_memory_index`]: SignalStore::with_memory_index
    pub fn ingest(&mut self, signal: &Signal) {
        match signal {
            Signal::SkillActivated(ev) => self.record_activated(&ev.skill),
            Signal::SkillCompleted(ev) => self.record_completed(ev),
            Signal::Intention(_) => {}
            Signal::Outcome(ev) => self.settle_outcome(ev),
        }
    }

    /// Map a session outcome to its memory verdict and settle the session's
    /// touched chunks via [`MemoryOutcomeSink`]. A no-op when no index is wired
    /// (or when [`with_memory_index`] is called with `None`-equivalent state).
    ///
    /// [`with_memory_index`]: SignalStore::with_memory_index
    fn settle_outcome(&self, ev: &OutcomeSignal) {
        let Some(index) = &self.memory_index else {
            return;
        };
        let verdict = match ev.status {
            SessionStatus::Done => Verdict::Success,
            SessionStatus::Abandoned => Verdict::Failure,
            SessionStatus::Active => Verdict::Undecided,
        };
        let sink = MemoryOutcomeSink::new(index.as_ref());
        if let Err(e) = sink.settle(verdict, &ev.touched) {
            // Settlement must never break the signal pipeline — telemetry keeps
            // flowing and the failure is visible on stderr.
            eprintln!("ff-signals: failed to settle outcome memory: {e}");
        }
    }

    /// A skill was active at the start of a turn.
    pub fn record_activated(&mut self, skill: &str) {
        self.aggregates
            .entry(skill.to_string())
            .or_insert_with(|| SkillAggregate::new(skill))
            .activations += 1;
    }

    /// A turn with `ev.skill` active finished; fold its metrics into the aggregate.
    pub fn record_completed(&mut self, ev: &SkillCompleted) {
        self.aggregates
            .entry(ev.skill.clone())
            .or_insert_with(|| SkillAggregate::new(&ev.skill))
            .record_completed(ev);
    }

    /// The aggregate for one skill, if it has any recorded signals.
    pub fn aggregate(&self, skill: &str) -> Option<SkillAggregate> {
        self.aggregates.get(skill).cloned()
    }

    /// Every aggregate, skill-name-sorted for deterministic output.
    pub fn all(&self) -> Vec<SkillAggregate> {
        let mut out: Vec<SkillAggregate> = self.aggregates.values().cloned().collect();
        out.sort_by(|a, b| a.skill.cmp(&b.skill));
        out
    }

    /// Capture the persistable state as `(path, json)` for a later write, or `None`
    /// when there is no path (in-memory store) or serialization fails. The caller
    /// runs this under whatever lock guards the store, then drops the lock and hands
    /// the payload to [`SignalStore::persist_payload`] — so the synchronous file
    /// write never happens while the lock is held (addresses #77 review nit 1: the
    /// old per-record auto-save did `2 × active-skills` locked writes per turn; the
    /// host now records in memory and persists once at turn end, lock-free).
    pub fn snapshot_payload(&self) -> Option<(PathBuf, String)> {
        let path = self.path.clone()?;
        match serde_json::to_string_pretty(&self.aggregates) {
            Ok(json) => Some((path, json)),
            Err(e) => {
                eprintln!("ff-signals: failed to serialize aggregates: {e}");
                None
            }
        }
    }

    /// Best-effort write of a [`snapshot_payload`](Self::snapshot_payload) result,
    /// meant to run *after* the store's lock is dropped. A `None` payload (no path /
    /// serialize error) is a no-op; an I/O failure is logged and ignored — telemetry
    /// must never break a turn.
    pub fn persist_payload(payload: Option<(PathBuf, String)>) {
        let Some((path, json)) = payload else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&path, json) {
            eprintln!("ff-signals: failed to persist aggregates: {e}");
        }
    }
}

#[cfg(test)]
mod tests;
