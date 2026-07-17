//! Cross-turn abstractive summary cache (#757).
//!
//! The compaction summarizer produces a summary of the "cold tail" of a
//! conversation. Within a single turn this is already cached (the local
//! `last_summary` variable). But across turns the cache was lost — each new
//! turn re-summarized from scratch even when only 1-2 messages were appended.
//!
//! This module provides a shared, per-session cache that survives across turns
//! so `run_turn` can seed its local state from the previous turn's result.
//!
//! ## Bounded growth (#764)
//!
//! Entries are evicted on an LRU basis once the cache exceeds
//! [`MAX_ENTRIES`] distinct session ids. Cross-turn caching is best-effort:
//! when a session's summary is evicted, its next turn simply re-summarizes
//! from scratch (graceful degradation — no correctness impact, just one extra
//! summarizer round-trip on the cold path).

use std::num::NonZeroUsize;
use std::sync::Mutex;

use ff_core::Message;
use lru::LruCache;

/// Soft cap on the number of distinct sessions tracked. At ~2 KB / entry the
/// worst-case footprint is ~256 KB; the value is comfortably above the number
/// of concurrent sessions any realistic user runs.
const MAX_ENTRIES: NonZeroUsize = NonZeroUsize::new(128).expect("non-zero");

/// Per-session cross-turn compaction summary cache.
///
/// Thread-safe via interior `Mutex` — the lock is held only for the duration
/// of an LRU op (nanoseconds), never across awaits. Only one `run_turn` per
/// session runs at a time (enforced by the cancel-before-spawn invariant
/// upstream), so contention is effectively zero.
pub struct CompactionCache {
    inner: Mutex<LruCache<String, CachedEntry>>,
}

/// The cached compaction state for one session. Holds both tier-2 abstractive
/// summary and tier-1 frozen prefix independently — both share the same LRU
/// lifecycle (invalidated together on edit/truncate/model change).
#[derive(Debug, Clone)]
struct CachedEntry {
    /// Tier-2 abstractive summary (RFC 0016 M7.0, #757).
    tier2: Option<Tier2Entry>,
    /// Tier-1 frozen compacted prefix (#933 A.2 step 2).
    tier1: Option<Tier1Entry>,
}

/// Tier-2 abstractive summary state.
#[derive(Debug, Clone)]
struct Tier2Entry {
    /// Index into the wire at which this summary ends (exclusive). Messages
    /// `[0..boundary]` are covered by the summary; `[boundary..]` are verbatim.
    boundary: usize,
    /// The summary message itself (role=System or User depending on prompt).
    summary: Message,
    /// Transcript message count when the summary was produced. Used by
    /// `summary_due()` to decide whether the cache is stale.
    message_count: u64,
}

/// Tier-1 frozen compacted prefix (#933 A.2).
#[derive(Debug, Clone)]
struct Tier1Entry {
    /// How many history messages this compacted prefix covers (`history[0..boundary]`).
    boundary: usize,
    /// The compacted messages for the cold prefix, byte-stable across turns.
    prefix: Vec<Message>,
    /// Transcript message count when this prefix was produced. Used as a
    /// staleness guard: the caller rejects the seed if the transcript shrank
    /// (edit/delete) since production, even if `invalidate` was missed.
    message_count: u64,
    /// Target-seeking deepening level this prefix was compacted at (#989). The
    /// caller reuses the frozen prefix only when it still wants this same level;
    /// if a deeper level is needed to hold `wire ≤ T`, it recompacts in full.
    level: usize,
}

impl CompactionCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(LruCache::new(MAX_ENTRIES)),
        }
    }

    // --- Tier-2 abstractive summary (#757) ---

    /// Retrieve the cached tier-2 summary for a session, if any.
    pub fn get(&self, session_id: &str) -> Option<(usize, Message, u64)> {
        let mut guard = self.inner.lock().unwrap();
        guard.get(session_id).and_then(|e| {
            e.tier2
                .as_ref()
                .map(|t| (t.boundary, t.summary.clone(), t.message_count))
        })
    }

    /// Store (or overwrite) the tier-2 summary for a session.
    pub fn put(&self, session_id: &str, boundary: usize, summary: Message, message_count: u64) {
        let mut guard = self.inner.lock().unwrap();
        let entry = guard.get_or_insert_mut(session_id.to_owned(), || CachedEntry {
            tier2: None,
            tier1: None,
        });
        entry.tier2 = Some(Tier2Entry {
            boundary,
            summary,
            message_count,
        });
    }

    // --- Tier-1 frozen prefix (#933 A.2 step 2) ---

    /// Retrieve the cached tier-1 frozen prefix for a session, if any.
    /// Returns `(boundary, prefix, message_count, level)`.
    pub fn get_tier1(&self, session_id: &str) -> Option<(usize, Vec<Message>, u64, usize)> {
        let mut guard = self.inner.lock().unwrap();
        guard.get(session_id).and_then(|e| {
            e.tier1
                .as_ref()
                .map(|t| (t.boundary, t.prefix.clone(), t.message_count, t.level))
        })
    }

    /// Store (or overwrite) the tier-1 frozen prefix for a session. `level` is the
    /// target-seeking deepening level the prefix was compacted at (#989), so the
    /// caller only reuses it when it still wants that same level.
    pub fn put_tier1(
        &self,
        session_id: &str,
        boundary: usize,
        prefix: Vec<Message>,
        message_count: u64,
        level: usize,
    ) {
        let mut guard = self.inner.lock().unwrap();
        let entry = guard.get_or_insert_mut(session_id.to_owned(), || CachedEntry {
            tier2: None,
            tier1: None,
        });
        entry.tier1 = Some(Tier1Entry {
            boundary,
            prefix,
            message_count,
            level,
        });
    }

    // --- Invalidation ---

    /// Invalidate both tiers for a session. Called on edit/truncate where the
    /// old boundaries are no longer valid.
    pub fn invalidate(&self, session_id: &str) {
        let mut guard = self.inner.lock().unwrap();
        guard.pop(session_id);
    }

    /// Invalidate all sessions. Called on provider/model change where cached
    /// compaction state may no longer be coherent.
    pub fn invalidate_all(&self) {
        let mut guard = self.inner.lock().unwrap();
        guard.clear();
    }
}

impl Default for CompactionCache {
    fn default() -> Self {
        Self::new()
    }
}

// `LruCache::iter_mut` would let consumers iterate, but the cache deliberately
// exposes only the four mutating ops above so eviction stays encapsulated.
#[cfg(test)]
mod tests;
