//! Lexical ranking over the installed skill registry (RFC 0001 §6).
//!
//! Pure and deterministic: no I/O and no embeddings — semantic search is a later
//! milestone (M4). The same [`search_skills`] backs both the palette-facing
//! `search_skills` Tauri command and the agent's `search_skills` tool, so
//! discovery ranks identically wherever it surfaces. An empty query lists every
//! skill, name-sorted.

use ff_core::SkillManifest;

use crate::registry::SkillRegistry;

/// A scored search result: the matched skill's manifest plus its relevance to the
/// query. Higher `score` ranks first; ties break by name for deterministic output.
pub struct SkillHit<'a> {
    pub manifest: &'a SkillManifest,
    pub score: u32,
}

/// Rank installed skills against `query`.
///
/// A non-empty query keeps only skills that match on at least one of (best first):
/// an exact keyword, a name prefix, a name substring, or a description substring.
/// All comparisons are case-insensitive. An empty/whitespace query returns every
/// skill with score `0`. Output is sorted by descending score, then by name.
pub fn search_skills<'a>(reg: &'a SkillRegistry, query: &str) -> Vec<SkillHit<'a>> {
    let q = query.trim().to_lowercase();
    let mut hits: Vec<SkillHit<'a>> = reg
        .list()
        .filter_map(|skill| {
            let score = if q.is_empty() {
                0
            } else {
                match score_skill(&skill.manifest, &q) {
                    0 => return None,
                    s => s,
                }
            };
            Some(SkillHit {
                manifest: &skill.manifest,
                score,
            })
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.manifest.name.cmp(&b.manifest.name))
    });
    hits
}

/// Relevance of one skill to a normalized (trimmed, lowercased) non-empty query.
/// `0` means no match. The bands are deliberately coarse — a precise ordering is
/// the job of M4 semantic search; this is enough for a command palette.
fn score_skill(manifest: &SkillManifest, q: &str) -> u32 {
    let name = manifest.name.to_lowercase();
    if manifest.keywords.iter().any(|k| k.to_lowercase() == q) {
        4
    } else if name.starts_with(q) {
        3
    } else if name.contains(q) {
        2
    } else if manifest.description.to_lowercase().contains(q) {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests;
