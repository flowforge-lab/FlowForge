//! Consolidation pass foundation (RFC 0006 §7.3, issue #223 P1).
//!
//! **Invariant**: Consolidation (via `rewrite_curated`) is the sole path that
//! does a **full-file rewrite** of curated `MEMORY.md`. The normal agent/user
//! capture path (`memory_write` → `WriteTarget::Curated`) still **appends** —
//! that's expected and not a violation. Decay/dormancy (M6, RFC 0007) NEVER
//! edits Markdown — it only controls what gets *injected*. Both consume
//! `chunk_key` and `Salience`.
//!
//! This module provides:
//! - [`chunk_key`] — stable identity for dedup and M6's `chunk_stats` table.
//! - [`Salience`] trait — ranks chunks for promotion/demotion decisions.
//! - [`RecencyFrequencySalience`] — mechanical default (recency × frequency).

use sha2::{Digest, Sha256};

use chrono::Local;

use crate::{MemoryChunk, MemorySource};

/// Stable chunk identity: `source + heading-path + hash(normalized text)`.
///
/// This key is shared between the consolidation pass (dedup) and M6's future
/// `chunk_stats` side table (RFC 0007 §4). It must be stable across:
/// - line-number shifts (reindex)
/// - trailing whitespace changes
/// - reordering of chunks in the same file
///
/// Changing the *content* of a chunk intentionally produces a new key (the old
/// stats are orphaned and swept on rebuild — the right default for genuinely
/// new content, per RFC 0007 §4).
pub fn chunk_key(chunk: &MemoryChunk) -> String {
    let source_tag = match &chunk.source {
        MemorySource::Curated => "curated".to_string(),
        MemorySource::Daily { date } => format!("daily:{}", date.format("%Y-%m-%d")),
    };
    let heading_path = chunk.heading.as_deref().unwrap_or("");
    let normalized = normalize_text(&chunk.text);

    // SHA-256 is stable across Rust versions — critical because RFC 0007 §4
    // persists these keys in the chunk_stats side table. Truncated to 16 hex
    // chars (64 bits) for compact keys; collision risk is negligible at memory
    // scale (~thousands of chunks).
    let hash = Sha256::digest(normalized.as_bytes());
    let text_hash: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();

    format!("{source_tag}:{heading_path}:{text_hash}")
}

/// Normalize text for hashing: trim each line, collapse runs of whitespace,
/// strip trailing newlines. This makes the key resilient to formatting drift
/// while still changing when the semantic content changes.
fn normalize_text(text: &str) -> String {
    text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Salience trait
// ---------------------------------------------------------------------------

/// Ranks a chunk's importance for consolidation decisions (promotion into or
/// demotion out of curated Markdown).
///
/// **Default impl**: [`RecencyFrequencySalience`] — mechanical recency × frequency.
/// **M6.3 upgrade path** (RFC 0007 §6): swap in a `chunk_stats`-backed impl that
/// uses `access_count` + `weight` with zero rewrite of consolidation logic.
///
/// TODO(M6.3): Add an LLM-driven Salience impl that uses semantic similarity
/// and importance scoring beyond mechanical heuristics.
pub trait Salience: Send + Sync {
    /// Score a chunk in `[0.0, 1.0]`. Higher = more salient = keep/promote.
    fn score(&self, chunk: &MemoryChunk, occurrences: u32) -> f32;
}

/// Mechanical recency × frequency ranking (the P1 default).
///
/// - **Recency**: exponential decay from chunk date (daily logs) or a fixed
///   high recency for curated (already promoted = presumed relevant).
/// - **Frequency**: `min(1.0, occurrences / saturation)` — caps so a single
///   repeated fact doesn't dominate.
///
/// Both factors are in `[0, 1]`; the product is the score.
pub struct RecencyFrequencySalience {
    /// Half-life in days for recency decay.
    pub half_life_days: f32,
    /// Number of occurrences at which frequency saturates to 1.0.
    pub saturation: u32,
}

impl Default for RecencyFrequencySalience {
    fn default() -> Self {
        Self {
            half_life_days: 14.0,
            saturation: 3,
        }
    }
}

impl Salience for RecencyFrequencySalience {
    fn score(&self, chunk: &MemoryChunk, occurrences: u32) -> f32 {
        let recency = match &chunk.source {
            MemorySource::Curated => 1.0_f32, // already promoted
            MemorySource::Daily { date } => {
                let today = Local::now().date_naive();
                let age_days = (today - *date).num_days().max(0) as f32;
                // Exponential decay: 0.5^(age / half_life)
                (0.5_f32).powf(age_days / self.half_life_days)
            }
        };
        let freq = (occurrences as f32 / self.saturation as f32).min(1.0);
        recency * freq
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use std::path::PathBuf;

    fn make_chunk(source: MemorySource, heading: Option<&str>, text: &str) -> MemoryChunk {
        MemoryChunk {
            id: 0,
            source,
            path: PathBuf::from("MEMORY.md"),
            heading: heading.map(String::from),
            text: text.to_string(),
            line_start: 1,
            line_end: 1,
            embedding: None,
        }
    }

    #[test]
    fn chunk_key_stable_across_whitespace_changes() {
        let c1 = make_chunk(
            MemorySource::Curated,
            Some("Prefs"),
            "likes rust\nhates yaml",
        );
        let c2 = make_chunk(
            MemorySource::Curated,
            Some("Prefs"),
            "  likes rust  \n  hates yaml  \n",
        );
        assert_eq!(chunk_key(&c1), chunk_key(&c2));
    }

    #[test]
    fn chunk_key_changes_on_content_change() {
        let c1 = make_chunk(MemorySource::Curated, Some("Prefs"), "likes rust");
        let c2 = make_chunk(MemorySource::Curated, Some("Prefs"), "likes python");
        assert_ne!(chunk_key(&c1), chunk_key(&c2));
    }

    #[test]
    fn chunk_key_differs_by_source() {
        let c1 = make_chunk(MemorySource::Curated, Some("A"), "same text");
        let c2 = make_chunk(
            MemorySource::Daily {
                date: NaiveDate::from_ymd_opt(2026, 6, 19).unwrap(),
            },
            Some("A"),
            "same text",
        );
        assert_ne!(chunk_key(&c1), chunk_key(&c2));
    }

    #[test]
    fn chunk_key_differs_by_heading() {
        let c1 = make_chunk(MemorySource::Curated, Some("A"), "same text");
        let c2 = make_chunk(MemorySource::Curated, Some("B"), "same text");
        assert_ne!(chunk_key(&c1), chunk_key(&c2));
    }

    #[test]
    fn chunk_key_stable_across_line_number_shifts() {
        let mut c1 = make_chunk(MemorySource::Curated, Some("Prefs"), "likes rust");
        c1.line_start = 1;
        c1.line_end = 1;
        let mut c2 = make_chunk(MemorySource::Curated, Some("Prefs"), "likes rust");
        c2.line_start = 42;
        c2.line_end = 42;
        assert_eq!(chunk_key(&c1), chunk_key(&c2));
    }

    #[test]
    fn salience_curated_scores_high_with_occurrences() {
        let s = RecencyFrequencySalience::default();
        let c = make_chunk(MemorySource::Curated, Some("H"), "fact");
        // 3 occurrences saturates frequency to 1.0; curated recency = 1.0
        assert!((s.score(&c, 3) - 1.0).abs() < 0.001);
    }

    #[test]
    fn salience_old_daily_scores_low() {
        let s = RecencyFrequencySalience::default();
        let old_date = NaiveDate::from_ymd_opt(2020, 1, 1).unwrap();
        let c = make_chunk(MemorySource::Daily { date: old_date }, Some("H"), "fact");
        let score = s.score(&c, 3);
        assert!(
            score < 0.01,
            "old daily chunk should score very low: {score}"
        );
    }

    #[test]
    fn salience_zero_occurrences_is_zero() {
        let s = RecencyFrequencySalience::default();
        let c = make_chunk(MemorySource::Curated, Some("H"), "fact");
        assert_eq!(s.score(&c, 0), 0.0);
    }
}
