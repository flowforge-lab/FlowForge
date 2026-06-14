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
mod tests {
    use super::*;
    use std::path::PathBuf;

    use ff_core::Skill;

    fn skill(name: &str, description: &str, keywords: &[&str]) -> Skill {
        Skill {
            manifest: SkillManifest {
                name: name.into(),
                description: description.into(),
                version: "0.1.0".into(),
                author: None,
                tools: vec![],
                mcp: vec![],
                keywords: keywords.iter().map(|k| k.to_string()).collect(),
            },
            body: String::new(),
            path: PathBuf::from("/tmp").join(name),
        }
    }

    fn registry(skills: Vec<Skill>) -> SkillRegistry {
        let mut reg = SkillRegistry::new();
        for s in skills {
            reg.insert_for_test(s);
        }
        reg
    }

    #[test]
    fn empty_query_lists_all_name_sorted() {
        let reg = registry(vec![
            skill("zeta", "z", &[]),
            skill("alpha", "a", &[]),
            skill("mid", "m", &[]),
        ]);
        let names: Vec<_> = search_skills(&reg, "")
            .iter()
            .map(|h| h.manifest.name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
        assert!(search_skills(&reg, "   ").len() == 3);
    }

    #[test]
    fn ranks_keyword_over_name_over_description() {
        let reg = registry(vec![
            // description-only match (lowest)
            skill("debugger", "helps with rust troubleshooting", &[]),
            // name substring match
            skill("crusty-tools", "misc", &[]),
            // exact keyword match (highest)
            skill("triage", "incident triage", &["rust"]),
            // name prefix match
            skill("rust-helper", "misc", &[]),
        ]);
        let names: Vec<_> = search_skills(&reg, "rust")
            .iter()
            .map(|h| h.manifest.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["triage", "rust-helper", "crusty-tools", "debugger"]
        );
    }

    #[test]
    fn non_matching_query_returns_empty() {
        let reg = registry(vec![skill("alpha", "a", &["x"])]);
        assert!(search_skills(&reg, "nomatch").is_empty());
    }

    #[test]
    fn tie_break_is_by_name() {
        let reg = registry(vec![
            skill("rust-z", "misc", &[]),
            skill("rust-a", "misc", &[]),
        ]);
        let names: Vec<_> = search_skills(&reg, "rust")
            .iter()
            .map(|h| h.manifest.name.as_str())
            .collect();
        assert_eq!(names, vec!["rust-a", "rust-z"]);
    }

    #[test]
    fn search_is_case_insensitive() {
        let reg = registry(vec![skill("Rust-Helper", "Systematic RUST debugging", &[])]);
        assert_eq!(search_skills(&reg, "RUST").len(), 1);
        assert_eq!(search_skills(&reg, "rust").len(), 1);
    }
}
