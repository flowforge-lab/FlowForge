use serde::{Deserialize, Serialize};

use crate::Mode;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum SessionStatus {
    Active,
    Done,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct Session {
    pub id: String,
    /// The stated intention for this session (Intention-Aware Sessions principle).
    pub goal: Option<String>,
    /// Human-facing display label, server-truth. Auto-derived from the first user
    /// message (see [`auto_title`]) and overridable via the `rename_session` ipc.
    pub title: Option<String>,
    /// Reserved for a future LLM-generated session summary. Unwired today; the
    /// field exists so adding summaries later is not another contract migration.
    pub summary: Option<String>,
    pub status: SessionStatus,
    /// Unix epoch milliseconds.
    #[ts(type = "number")]
    pub created_at: i64,
    /// Unix epoch milliseconds.
    #[ts(type = "number")]
    pub updated_at: i64,
    /// The phenotype this session runs as (#246). The *name* of a phenotype
    /// (`default` or a file stem under `~/.flowforge/phenos/`), resolved per
    /// turn to its persona / skills / model. `None` means "inherit the global
    /// active phenotype" — so two panes can run different phenotypes while
    /// untouched sessions always track the last-used global choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub phenotype: Option<String>,
    /// The agent autonomy mode this session runs as (RFC 0011 P2, #265). `None`
    /// means "inherit the global `defaultMode` preference" — so a new session tracks
    /// the user's default while a pane can override it independently (#148).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mode: Option<Mode>,
}

/// A session's working directory as surfaced to the frontend selector (#200,
/// #211). `path` is the absolute, symlink-resolved cwd; `git_branch` is the
/// repo's current branch when the cwd is a git working tree (`None` otherwise,
/// e.g. not a repo or detached HEAD), so the selector can render `repo - branch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct SessionWorkspace {
    pub path: String,
    pub git_branch: Option<String>,
}

/// Words skipped at the start of a prompt before extracting a title: pronouns,
/// articles, modals, question stems, and proxy verbs that precede the real
/// subject ("understand how X" -> skip to X). Kept in sync with the frontend's
/// former `autoTitle` so server- and client-derived titles agree.
const STOP_WORDS: &[&str] = &[
    "a",
    "an",
    "the",
    "is",
    "are",
    "was",
    "were",
    "i",
    "you",
    "we",
    "they",
    "it",
    "he",
    "she",
    "my",
    "your",
    "our",
    "their",
    "in",
    "on",
    "at",
    "to",
    "for",
    "of",
    "and",
    "or",
    "but",
    "how",
    "what",
    "when",
    "where",
    "why",
    "who",
    "do",
    "does",
    "did",
    "can",
    "could",
    "would",
    "should",
    "will",
    "please",
    "help",
    "me",
    "us",
    "understand",
    "explain",
    "tell",
    "show",
    "describe",
    "clarify",
    "give",
];

/// Derive a short, readable title from a user's first prompt. Leading stop-words
/// are skipped to land on the first meaningful word, then the word count scales
/// with input length (2 -> 5 words). Mirrors the frontend's former `autoTitle`.
pub fn auto_title(content: &str) -> String {
    let words: Vec<&str> = content.split_whitespace().collect();
    if words.is_empty() {
        return "New session".to_string();
    }

    // Advance past all leading stop-words, but always keep at least one word.
    let mut start = 0;
    while start < words.len() - 1 && is_stop_word(words[start]) {
        start += 1;
    }
    let meaningful = &words[start..];

    // Scale the word count on input length (chars, matching the FE's string length).
    let len = content.chars().count();
    let cap = if len <= 25 {
        2
    } else if len <= 50 {
        3
    } else if len <= 100 {
        4
    } else {
        5
    };
    let count = meaningful.len().min(cap);

    let title = meaningful[..count].join(" ");
    capitalize_first(&title)
}

/// Stop-word test: lowercase, then strip everything outside `a..=z` (so "How," or
/// "I'd" normalize), matching the frontend's `replace(/[^a-z]/g, "")`.
fn is_stop_word(word: &str) -> bool {
    let normalized: String = word
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase())
        .collect();
    STOP_WORDS.contains(&normalized.as_str())
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_new_session() {
        assert_eq!(auto_title(""), "New session");
        assert_eq!(auto_title("   "), "New session");
    }

    #[test]
    fn skips_leading_stop_words() {
        // "how do i" are all stop-words; lands on "deploy".
        assert_eq!(auto_title("how do i deploy"), "Deploy");
    }

    #[test]
    fn keeps_at_least_one_word_when_all_stop_words() {
        // Every word is a stop-word; the loop keeps the last one.
        assert_eq!(auto_title("please help me"), "Me");
    }

    #[test]
    fn scales_word_count_with_length() {
        // <= 25 chars -> 2 words (only *leading* stop-words are skipped).
        assert_eq!(auto_title("deploy parser service now"), "Deploy parser");
        // 26..=50 chars -> 3 words.
        let s = "refactor the session store and its tests";
        assert_eq!(s.len(), 40);
        assert_eq!(auto_title(s), "Refactor the session");
    }

    #[test]
    fn strips_punctuation_when_matching_stop_words() {
        // "How," normalizes to "how" (a stop-word) and is skipped.
        assert_eq!(auto_title("How, exactly?"), "Exactly?");
    }

    #[test]
    fn capitalizes_first_letter() {
        assert_eq!(auto_title("parser"), "Parser");
    }
}
