//! Outcome-gated reinforcement — the consumer that turns a finished session's
//! verdict into memory weight changes (#1292 block A, RFC 0022).
//!
//! # The seam
//!
//! [`MemoryOutcomeSink::settle`] is the single point where an outcome verdict
//! meets the memory index. **How the verdict reaches it is deliberately not this
//! module's concern**, and that is the whole design:
//!
//! - **Block A (now):** the session/goal termination hook calls [`settle`]
//!   directly — both the CLI (`ff goal`) and desktop goal loops drive their loop
//!   to a terminal `LoopStop`, map it to a [`Verdict`] via the shared
//!   `LoopStop::verdict` (in `ff-agent`), and settle the [`TouchLog`] the run's
//!   `memory_write` calls filled.
//! - **Block C (later, RFC 0022):** the `ff-signals` aggregator, on ingesting a
//!   `Signal::Outcome`, calls the *same* [`settle`] — the transport becomes the
//!   signal bus, but this consumer and its tests do not change.
//!
//! Keeping the consumer isolated behind one function is what lets block A ship
//! without writing throwaway code: block C reroutes the caller, not the logic.
//!
//! [`settle`]: MemoryOutcomeSink::settle
//! [`TouchLog`]: crate::TouchLog

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::index::MemoryIndex;
use crate::Result;

/// A session-scoped, shared record of the `chunk_key`s a session *touched* —
/// currently the Daily chunks it wrote (RFC 0022, #1292). The producer
/// (`memory_write`) records into it as the session runs; the termination hook
/// drains it and hands the keys to [`MemoryOutcomeSink::settle`].
///
/// It is a cheap `Arc<Mutex<..>>` handle so the tool registry and the loop that
/// outlives it can share one buffer without threading a lifetime through every
/// layer. A default-constructed log that is never wired is inert — recording
/// into it costs a lock and a set insert, and if nothing ever drains it the
/// keys are simply dropped.
#[derive(Clone, Default)]
pub struct TouchLog {
    keys: Arc<Mutex<HashSet<String>>>,
}

impl TouchLog {
    /// A fresh, empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a touched `chunk_key`. Idempotent within a session.
    pub fn record(&self, key: impl Into<String>) {
        // A mutex is poisoned only if a holder panicked; the set itself is intact,
        // so recover it rather than silently dropping the touch.
        self.keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.into());
    }

    /// Record several touched `chunk_key`s at once.
    pub fn extend<I, S>(&self, keys: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(keys.into_iter().map(Into::into));
    }

    /// Take the accumulated keys, leaving the log empty. Called once at
    /// settlement so a re-run of the hook cannot double-apply.
    pub fn drain(&self) -> Vec<String> {
        // Recover from a poisoned lock: dropping the whole touch set here would
        // make settlement a silent no-op with no signal that anything was lost.
        self.keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .collect()
    }

    /// Number of distinct keys recorded so far (mainly for tests/telemetry).
    pub fn len(&self) -> usize {
        self.keys.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether no keys have been recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The resolved outcome of a session or goal, as it bears on memory.
///
/// A verdict is a *judgement about the work*, not a raw terminal state — the
/// termination hook is responsible for the mapping (e.g. a goal loop that
/// exhausted its iterations without completing is a [`Failure`], a paused loop
/// is [`Undecided`]). Kept minimal on purpose: block C's signal payload maps
/// onto the same three cases.
///
/// [`Failure`]: Verdict::Failure
/// [`Undecided`]: Verdict::Undecided
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The session/goal reached its intended end. Touched chunks are reinforced
    /// and any promotion suppression on them is lifted.
    Success,
    /// The session/goal ended without reaching its intent (abandoned, failed, or
    /// exhausted). Touched chunks have promotion suppressed so a dead end does
    /// not get consolidated into curated memory — non-destructively, so a later
    /// success can rehabilitate them.
    Failure,
    /// No actionable judgement (e.g. paused, or still active). Memory is left
    /// exactly as is.
    Undecided,
}

/// Applies session/goal outcomes to a [`MemoryIndex`]. A thin, transport-free
/// consumer — see the [module docs](self) for why it is isolated.
pub struct MemoryOutcomeSink<'a> {
    index: &'a dyn MemoryIndex,
}

impl<'a> MemoryOutcomeSink<'a> {
    /// Wrap an index. Borrow-only; holds no state of its own.
    pub fn new(index: &'a dyn MemoryIndex) -> Self {
        Self { index }
    }

    /// Settle `touched` (the `chunk_key`s the session leaned on — its ambient
    /// injections and recalls) against `verdict`:
    ///
    /// - [`Verdict::Success`] → [`reinforce_outcome`], which bumps weight and
    ///   clears any promotion suppression.
    /// - [`Verdict::Failure`] → [`set_suppress_promotion`]`(_, true)` on each key.
    /// - [`Verdict::Undecided`] → no-op.
    ///
    /// Empty `touched` is a no-op regardless of verdict — a session that
    /// surfaced no memory has nothing to reinforce or suppress.
    ///
    /// Suppression is matched and cleared by *content* identity (see
    /// [`reinforce_outcome`]), so when a `Success` and a `Failure` touch the
    /// same content — whether in one pass or across sessions — the outcome that
    /// settles **last** wins: a later `Success` rehabilitates content an earlier
    /// `Failure` suppressed, and a later `Failure` re-suppresses it. There is no
    /// per-pass merge; settlement is applied in call order.
    ///
    /// [`reinforce_outcome`]: MemoryIndex::reinforce_outcome
    /// [`set_suppress_promotion`]: MemoryIndex::set_suppress_promotion
    pub fn settle(&self, verdict: Verdict, touched: &[String]) -> Result<()> {
        if touched.is_empty() {
            return Ok(());
        }
        match verdict {
            Verdict::Success => self.index.reinforce_outcome(touched)?,
            Verdict::Failure => {
                for key in touched {
                    self.index.set_suppress_promotion(key, true)?;
                }
            }
            Verdict::Undecided => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consolidate::chunk_key;
    use crate::index::Fts5Index;
    use crate::{chunk_markdown, MemorySource};
    use std::path::Path;

    fn index_with(md: &str) -> (Fts5Index, Vec<String>) {
        let idx = Fts5Index::open_in_memory().unwrap();
        let chunks = chunk_markdown(md, MemorySource::Curated, Path::new("MEMORY.md"));
        idx.reindex(&chunks).unwrap();
        // Give each chunk a stats row so reinforcement has something to bump.
        let hits = idx.search("alpha OR beta", 10).unwrap();
        idx.reinforce(&hits).unwrap();
        let keys = chunks.iter().map(chunk_key).collect();
        (idx, keys)
    }

    #[test]
    fn success_reinforces_and_clears_suppression() {
        let (idx, keys) = index_with("## A\nalpha\n\n## B\nbeta");
        // A prior failure suppressed both.
        for k in &keys {
            idx.set_suppress_promotion(k, true).unwrap();
        }
        MemoryOutcomeSink::new(&idx)
            .settle(Verdict::Success, &keys)
            .unwrap();
        let flagged = idx.suppressed_promotion_keys().unwrap();
        assert!(flagged.is_empty(), "success clears all suppression");
    }

    #[test]
    fn failure_suppresses_every_touched_key() {
        let (idx, keys) = index_with("## A\nalpha\n\n## B\nbeta");
        MemoryOutcomeSink::new(&idx)
            .settle(Verdict::Failure, &keys)
            .unwrap();
        let flagged = idx.suppressed_promotion_keys().unwrap();
        assert_eq!(flagged.len(), keys.len(), "failure suppresses all touched");
        for k in &keys {
            assert!(flagged.contains(k));
        }
    }

    #[test]
    fn undecided_is_a_noop() {
        let (idx, keys) = index_with("## A\nalpha\n\n## B\nbeta");
        idx.set_suppress_promotion(&keys[0], true).unwrap();
        MemoryOutcomeSink::new(&idx)
            .settle(Verdict::Undecided, &keys)
            .unwrap();
        // Untouched: the pre-existing flag remains, nothing else changes.
        let flagged = idx.suppressed_promotion_keys().unwrap();
        assert_eq!(flagged.len(), 1);
        assert!(flagged.contains(&keys[0]));
    }

    #[test]
    fn empty_touched_is_a_noop_for_every_verdict() {
        let (idx, _) = index_with("## A\nalpha");
        let sink = MemoryOutcomeSink::new(&idx);
        for v in [Verdict::Success, Verdict::Failure, Verdict::Undecided] {
            sink.settle(v, &[]).unwrap();
        }
        assert!(idx.suppressed_promotion_keys().unwrap().is_empty());
    }

    #[test]
    fn touch_log_dedups_and_drains_once() {
        let log = TouchLog::new();
        assert!(log.is_empty());
        log.record("daily:2026-08-26:abc");
        log.record("daily:2026-08-26:abc"); // duplicate within a session
        log.extend(["daily:2026-08-26:def", "daily:2026-08-26:ghi"]);
        assert_eq!(log.len(), 3);

        let mut drained = log.drain();
        drained.sort();
        assert_eq!(
            drained,
            vec![
                "daily:2026-08-26:abc".to_string(),
                "daily:2026-08-26:def".to_string(),
                "daily:2026-08-26:ghi".to_string(),
            ]
        );
        // A second drain yields nothing — settlement cannot double-apply.
        assert!(log.drain().is_empty());
        assert!(log.is_empty());
    }

    #[test]
    fn shared_touch_log_clones_see_the_same_buffer() {
        let producer = TouchLog::new();
        let consumer = producer.clone();
        producer.record("daily:2026-08-26:xyz");
        assert_eq!(consumer.len(), 1);
        assert_eq!(consumer.drain(), vec!["daily:2026-08-26:xyz".to_string()]);
        // Drain through one handle empties the other.
        assert!(producer.is_empty());
    }

    #[test]
    fn drain_recovers_keys_after_a_poisoned_lock() {
        // #1292 review F6: a poisoned mutex (a holder panicked) must not make
        // settlement a silent no-op that drops every touched key. The set itself
        // is intact, so record/drain recover it via `into_inner`.
        let log = TouchLog::new();
        log.record("daily:2026-08-31:abc");

        let clone = log.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = clone.keys.lock().unwrap();
            panic!("poison the mutex while holding the guard");
        }));

        // The lock is now poisoned; the previously recorded key must survive, and
        // a further record must still land.
        log.record("daily:2026-08-31:def");
        let mut drained = log.drain();
        drained.sort();
        assert_eq!(
            drained,
            vec![
                "daily:2026-08-31:abc".to_string(),
                "daily:2026-08-31:def".to_string()
            ],
            "a poisoned lock must not lose recorded keys"
        );
    }

    /// The issue's acceptance scenario, corrected for content-keyed suppression
    /// (#1304 F1): two sessions touch an identical-content Daily chunk under
    /// different daily dates. Because suppression identity is the content key,
    /// not the dated `chunk_key`, they are the *same* promotion identity — so
    /// the verdicts cannot "diverge"; instead the last-settled outcome wins.
    /// A `Failure` after a `Success` suppresses; a later `Success` rehabilitates.
    #[test]
    fn identical_daily_content_resolves_by_last_verdict() {
        let idx = Fts5Index::open_in_memory().unwrap();
        let md = "## Note\nremember this fact";

        let d1: chrono::NaiveDate = "2026-08-26".parse().unwrap();
        let d2: chrono::NaiveDate = "2026-08-27".parse().unwrap();
        let earlier = chunk_markdown(md, MemorySource::Daily { date: d1 }, Path::new("x"));
        let later = chunk_markdown(md, MemorySource::Daily { date: d2 }, Path::new("x"));
        let earlier_keys: Vec<String> = earlier.iter().map(chunk_key).collect();
        let later_keys: Vec<String> = later.iter().map(chunk_key).collect();

        // Success then Failure on the same content -> Failure (last) wins: the
        // content is suppressed.
        MemoryOutcomeSink::new(&idx)
            .settle(Verdict::Success, &earlier_keys)
            .unwrap();
        MemoryOutcomeSink::new(&idx)
            .settle(Verdict::Failure, &later_keys)
            .unwrap();
        assert!(
            !idx.suppressed_promotion_keys().unwrap().is_empty(),
            "a Failure settling last suppresses the shared content"
        );

        // A subsequent Success on the same content rehabilitates it, clearing
        // the earlier-dated flag by content identity.
        MemoryOutcomeSink::new(&idx)
            .settle(Verdict::Success, &later_keys)
            .unwrap();
        assert!(
            idx.suppressed_promotion_keys().unwrap().is_empty(),
            "a later Success rehabilitates content an earlier Failure suppressed"
        );
    }
}
