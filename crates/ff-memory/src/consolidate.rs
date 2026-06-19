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

use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};

use chrono::{Local, NaiveDate};

use crate::{MemoryChunk, MemorySource, Result, WriteTarget};

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
    format!("{source_tag}:{}", content_key(chunk))
}

/// Source-agnostic content identity: `heading-path + hash(normalized text)`.
///
/// Unlike [`chunk_key`] this deliberately omits the source tag, so the *same
/// fact* captured in a daily log and again in curated Markdown shares one key.
/// Consolidation uses it to count how often a fact recurs (promotion) and to
/// detect a curated fact that is already present (merge / skip).
fn content_key(chunk: &MemoryChunk) -> String {
    let heading_path = chunk.heading.as_deref().unwrap_or("");
    let normalized = normalize_text(&chunk.text);

    // SHA-256 is stable across Rust versions - critical because RFC 0007 sec 4
    // persists these keys in the chunk_stats side table. Truncated to 16 hex
    // chars (64 bits) for compact keys; collision risk is negligible at memory
    // scale (~thousands of chunks).
    let hash = Sha256::digest(normalized.as_bytes());
    let text_hash: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();

    format!("{heading_path}:{text_hash}")
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

// ---------------------------------------------------------------------------
// Consolidation pass (issue #223 P2, RFC 0007 sec 6)
// ---------------------------------------------------------------------------

/// Minimum [`Salience`] score for a recurring daily fact to be promoted into
/// curated Markdown. With the default recency x frequency salience this means a
/// fact must be both recent and seen on more than one day -- a one-off daily
/// note scores ~`1/saturation` and stays in the daily log.
const PROMOTION_SCORE_CUTOFF: f32 = 0.5;

/// Outcome of a [`Memory::consolidate`] pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsolidationReport {
    /// Whether the pass changed anything (and thus rewrote curated Markdown).
    /// A re-run with nothing to do returns `ran = false` and touches no files.
    pub ran: bool,
    /// Near-identical curated facts collapsed into one.
    pub merged: usize,
    /// Recurring, recent daily facts lifted into curated.
    pub promoted: usize,
    /// Lowest-salience curated facts evicted back to the daily log.
    pub demoted: usize,
    /// Curated file size before the pass, in bytes.
    pub bytes_before: usize,
    /// Curated file size after the pass (== `bytes_before` when `!ran`).
    pub bytes_after: usize,
}

/// Rebuild curated Markdown from the kept chunks. Each chunk's `text` already
/// includes its heading line, so the trimmed bodies join with a blank line.
/// Re-chunking this output is stable, which the idempotent re-run relies on.
fn render_curated(chunks: &[MemoryChunk]) -> String {
    let mut out = chunks
        .iter()
        .map(|c| c.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

impl crate::Memory {
    /// Run the consolidation pass (issue #223 P2; RFC 0006 sec 7.3 / RFC 0007 sec 6).
    ///
    /// Decides merge / promote / demote entirely in memory, then performs side
    /// effects: daily-log appends for any demoted entries, followed by a single
    /// atomic curated rewrite. **Idempotent** -- a re-run with nothing to change
    /// returns `ran = false` and writes nothing.
    ///
    /// - **Merge**: collapse curated facts with identical content.
    /// - **Promote**: lift recurring, recent daily facts (score past
    ///   [`PROMOTION_SCORE_CUTOFF`]) into curated.
    /// - **Demote**: when `evict_to_budget` is set and curated still exceeds
    ///   `injection_budget_bytes`, append the lowest-salience curated facts back
    ///   to today's daily log (history, still FTS-indexed) and drop them from
    ///   curated. Never deletes -- no data loss.
    ///
    /// Does **not** reindex: the caller owns the index (and the blocking embed
    /// call it may make). See `MemoryConsolidateTool`.
    pub fn consolidate(&self, salience: &dyn Salience) -> Result<ConsolidationReport> {
        let bytes_before = std::fs::metadata(self.curated_path())
            .map(|m| m.len())
            .unwrap_or(0) as usize;
        let mut report = ConsolidationReport {
            bytes_before,
            bytes_after: bytes_before,
            ..Default::default()
        };
        if !self.is_enabled() {
            return Ok(report);
        }

        // Gather and split curated vs daily.
        let mut curated: Vec<MemoryChunk> = Vec::new();
        let mut daily: Vec<MemoryChunk> = Vec::new();
        for c in self.all_chunks() {
            match c.source {
                MemorySource::Curated => curated.push(c),
                MemorySource::Daily { .. } => daily.push(c),
            }
        }

        // Recurrence is counted per distinct day so a fact repeated within a
        // single daily log doesn't inflate its frequency.
        let mut daily_days: HashMap<String, HashSet<NaiveDate>> = HashMap::new();
        for c in &daily {
            if let MemorySource::Daily { date } = c.source {
                daily_days.entry(content_key(c)).or_default().insert(date);
            }
        }
        let occurrences =
            |key: &str| -> u32 { daily_days.get(key).map(|d| d.len() as u32).unwrap_or(0) };

        // --- Merge: keep the first of each duplicate curated content. ---
        let mut seen: HashSet<String> = HashSet::new();
        let mut kept: Vec<MemoryChunk> = Vec::new();
        for c in curated {
            if seen.insert(content_key(&c)) {
                kept.push(c);
            } else {
                report.merged += 1;
            }
        }
        let mut curated = kept;
        let curated_keys: HashSet<String> = curated.iter().map(content_key).collect();

        // --- Promote: recurring, recent daily facts not already curated. Pick
        //     the highest-scoring (freshest) copy per content key. ---
        let mut best: HashMap<String, (f32, MemoryChunk)> = HashMap::new();
        for c in &daily {
            let key = content_key(c);
            if curated_keys.contains(&key) {
                continue;
            }
            let score = salience.score(c, occurrences(&key));
            if score < PROMOTION_SCORE_CUTOFF {
                continue;
            }
            match best.get(&key) {
                Some((s, _)) if *s >= score => {}
                _ => {
                    best.insert(key, (score, c.clone()));
                }
            }
        }
        let mut promoted_keys: Vec<String> = best.keys().cloned().collect();
        promoted_keys.sort();
        for key in promoted_keys {
            let (_, mut chunk) = best.remove(&key).unwrap();
            chunk.source = MemorySource::Curated;
            curated.push(chunk);
            report.promoted += 1;
        }

        // --- Demote: hard-evict lowest-salience curated facts to fit the budget
        //     (config-gated). Append each back to today's daily log first. ---
        if self.config().evict_to_budget {
            let budget = self.config().injection_budget_bytes;
            while render_curated(&curated).len() > budget && curated.len() > 1 {
                let mut worst = 0usize;
                let mut worst_score = f32::INFINITY;
                for (i, c) in curated.iter().enumerate() {
                    let score = salience.score(c, occurrences(&content_key(c)));
                    if score < worst_score {
                        worst_score = score;
                        worst = i;
                    }
                }
                let evicted = curated.remove(worst);
                self.write(&evicted.text, WriteTarget::Daily)?;
                report.demoted += 1;
            }
        }

        report.ran = report.merged > 0 || report.promoted > 0 || report.demoted > 0;
        if report.ran {
            self.rewrite_curated(&render_curated(&curated))?;
            report.bytes_after = std::fs::metadata(self.curated_path())
                .map(|m| m.len())
                .unwrap_or(0) as usize;
        }
        Ok(report)
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

    // -- consolidation pass (issue #223 P2) --

    use crate::{Memory, MemoryConfig};

    fn mem_with(root: &std::path::Path, budget: usize, evict: bool) -> Memory {
        let config = MemoryConfig {
            injection_budget_bytes: budget,
            evict_to_budget: evict,
            ..Default::default()
        };
        Memory::new(root, config)
    }

    fn days_ago(n: i64) -> NaiveDate {
        chrono::Local::now().date_naive() - chrono::Duration::days(n)
    }

    fn write_daily(m: &Memory, date: NaiveDate, content: &str) {
        let path = m.daily_path(date);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn consolidate_merges_duplicate_curated_facts() {
        let dir = tempfile::tempdir().unwrap();
        let m = mem_with(dir.path(), 4096, true);
        m.rewrite_curated("# Prefs\nlikes rust\n\n# Prefs\nlikes rust\n")
            .unwrap();

        let report = m.consolidate(&RecencyFrequencySalience::default()).unwrap();

        assert_eq!(report.merged, 1, "the duplicate section should collapse");
        assert!(report.ran);
        let curated = std::fs::read_to_string(m.curated_path()).unwrap();
        assert_eq!(
            curated.matches("likes rust").count(),
            1,
            "only one copy left"
        );
    }

    #[test]
    fn consolidate_promotes_recurring_daily_fact() {
        let dir = tempfile::tempdir().unwrap();
        let m = mem_with(dir.path(), 4096, true);
        // Same fact captured on three recent days -> recurring -> promote.
        for n in 0..3 {
            write_daily(&m, days_ago(n), "# Project\nuses tauri\n");
        }

        let report = m.consolidate(&RecencyFrequencySalience::default()).unwrap();

        assert_eq!(
            report.promoted, 1,
            "recurring daily fact should be promoted"
        );
        assert!(report.ran);
        let curated = std::fs::read_to_string(m.curated_path()).unwrap();
        assert!(
            curated.contains("uses tauri"),
            "promoted into curated: {curated}"
        );
    }

    #[test]
    fn consolidate_skips_one_off_daily_fact() {
        let dir = tempfile::tempdir().unwrap();
        let m = mem_with(dir.path(), 4096, true);
        write_daily(&m, days_ago(0), "# One\nseen once\n");

        let report = m.consolidate(&RecencyFrequencySalience::default()).unwrap();

        assert_eq!(report.promoted, 0, "a one-off fact stays in the daily log");
        assert!(!report.ran);
    }

    #[test]
    fn consolidate_demotes_to_daily_when_over_budget() {
        let dir = tempfile::tempdir().unwrap();
        let m = mem_with(dir.path(), 40, true);
        m.rewrite_curated("# A\nalpha fact\n\n# B\nbeta fact\n\n# C\ngamma fact\n")
            .unwrap();

        let report = m.consolidate(&RecencyFrequencySalience::default()).unwrap();

        assert!(report.demoted >= 1, "over-budget curated should demote");
        assert!(report.ran);
        // Demoted entries are appended to TODAY's daily log (history, not deleted).
        let today = std::fs::read_to_string(m.daily_path(days_ago(0))).unwrap();
        assert!(
            today.contains("fact"),
            "evicted text lands in daily: {today}"
        );
        assert!(report.bytes_after < report.bytes_before, "curated shrank");
    }

    #[test]
    fn consolidate_demote_gated_off_by_config() {
        let dir = tempfile::tempdir().unwrap();
        let m = mem_with(dir.path(), 40, false);
        let curated = "# A\nalpha fact\n\n# B\nbeta fact\n\n# C\ngamma fact\n";
        m.rewrite_curated(curated).unwrap();

        let report = m.consolidate(&RecencyFrequencySalience::default()).unwrap();

        assert_eq!(report.demoted, 0, "eviction is disabled");
        assert!(
            !report.ran,
            "nothing to do when eviction is off and no merge/promote"
        );
        assert_eq!(std::fs::read_to_string(m.curated_path()).unwrap(), curated);
    }

    #[test]
    fn consolidate_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let m = mem_with(dir.path(), 4096, true);
        m.rewrite_curated("# Prefs\nlikes rust\n\n# Prefs\nlikes rust\n")
            .unwrap();

        let first = m.consolidate(&RecencyFrequencySalience::default()).unwrap();
        assert!(first.ran);
        let after_first = std::fs::read_to_string(m.curated_path()).unwrap();

        let second = m.consolidate(&RecencyFrequencySalience::default()).unwrap();
        assert!(!second.ran, "re-run must be a no-op");
        assert_eq!(second.merged + second.promoted + second.demoted, 0);
        assert_eq!(
            std::fs::read_to_string(m.curated_path()).unwrap(),
            after_first
        );
    }

    #[test]
    fn consolidate_disabled_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let config = MemoryConfig {
            enabled: false,
            ..Default::default()
        };
        let m = Memory::new(dir.path(), config);
        m.rewrite_curated("# Prefs\nlikes rust\n\n# Prefs\nlikes rust\n")
            .unwrap();
        let before = std::fs::read_to_string(m.curated_path()).unwrap();

        let report = m.consolidate(&RecencyFrequencySalience::default()).unwrap();

        assert!(!report.ran);
        assert_eq!(std::fs::read_to_string(m.curated_path()).unwrap(), before);
    }
}
