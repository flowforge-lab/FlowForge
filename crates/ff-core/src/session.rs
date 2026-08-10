use serde::{Deserialize, Serialize};

use crate::{McpServerConfig, Mode, ModelSelection};
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
    /// The working directory this session's tools run in (#200, #279). An absolute,
    /// symlink-resolved path; `None` means "inherit the global default workspace".
    /// Persisted in the session row so a chosen cwd survives a restart (RFC 0012 P4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workspace: Option<String>,
    /// The model this session runs on (RFC 0005 §11 Phase D, #499). A resolved
    /// `(connection, model)` pair that overrides the phenotype's model for this
    /// session only; `None` means "inherit the phenotype's model, falling back to
    /// the global active selection" -- so a pane can pin its own model while
    /// untouched sessions follow their phenotype.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<ModelSelection>,
    /// Session-tier MCP server overrides (RFC 0018 section 3.1, top tier). Whole-record
    /// overlay-by-id over the phenotype and global tiers (RFC section 11.5); `None`
    /// means "inherit the phenotype + global resolution" -- so a pane can pin its own
    /// MCP set while untouched sessions follow their phenotype. Persisted as JSON,
    /// exactly like [`model`](Self::model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcp_servers: Option<Vec<McpServerConfig>>,
    /// The session this one was forked from (#1074, RFC 0023 §4). `None` means
    /// "lineage root" -- either never forked, or forked before lineage was
    /// recorded (pre-existing history cannot be back-filled). Cleared to `None`
    /// if the parent is deleted, so a fork outlives its source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parent_session_id: Option<String>,
    /// The last parent `seq` this session's transcript copied at fork time
    /// (#1074). Because forking preserves `seq` verbatim, this is a coordinate
    /// valid in *both* sessions: `seq <= fork_point_seq` is the shared prefix,
    /// `seq > fork_point_seq` is post-fork divergence on either side.
    ///
    /// Two distinct `None` cases, told apart by [`parent_session_id`](Self::parent_session_id):
    /// with no parent it means "lineage root"; with a parent it means the parent
    /// was empty at fork time, i.e. an empty shared prefix.
    ///
    /// An **upper bound**, not a density guarantee: editing a message truncates
    /// later ones without renumbering, so a `seq` at or below this point may no
    /// longer exist on one side. Readers must intersect on actual rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub fork_point_seq: Option<i64>,
    /// Provenance for a session produced by confluence (#1229, RFC 0023 §4/§5):
    /// the source sessions whose transcripts were concatenated, in the order
    /// they were appended, each with how many messages it contributed. `None`
    /// for every ordinary session. Because segments are appended whole and never
    /// interleaved, these counts partition this session's transcript into
    /// contiguous spans, so any message maps back to its origin session by
    /// position — structured provenance, never `<session_id>` tags in content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub confluence_sources: Option<Vec<ConfluenceSource>>,
}

/// One source segment of a confluence session (#1229, RFC 0023 §4/§5): a
/// session that was concatenated into it, and the number of messages it
/// contributed. Ordered within [`Session::confluence_sources`] by append order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ConfluenceSource {
    /// The session this segment was copied from. Always `Some` when a confluence
    /// is created: V1 requires every source to exist (a missing one aborts with
    /// `SessionNotFound`). `None` is reserved for a source deleted *after* the
    /// confluence — the segment stays, its exact origin is simply lost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_id: Option<String>,
    /// How many messages this source contributed, i.e. the length of its
    /// contiguous span in the confluence transcript.
    #[ts(type = "number")]
    pub message_count: i64,
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
mod tests;
