//! Durable, local-first user memory (RFC 0006, M5).
//!
//! Memory is **plain Markdown the user owns**, under `~/.flowforge/memory/`:
//! a small curated `MEMORY.md` (facts, preferences, decisions) and an
//! append-only `daily/YYYY-MM-DD.md` working log. This crate owns those files,
//! the (later) index, and recall.
//!
//! **M5.0 scope is ambient injection only.** [`Memory::ambient_block`] renders a
//! compact, budget-bounded block — curated memory plus today + yesterday's daily
//! log — that the host prepends to the system prompt through the RFC 0001 §4 hook
//! (the same seam RFC 0002 uses for ambient context). There is no index or recall
//! tool yet; those are M5.1. The [`MemoryChunk`] / [`MemorySource`] types are
//! defined here now (RFC 0006 §4) so the index can consume them without churn.
//!
//! Reads are **lenient**: a missing file is "nothing recorded yet" (empty), never
//! an error, so callers never need a try/catch around a fresh install.

use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};

/// Where a chunk came from (RFC 0006 §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemorySource {
    /// The curated `MEMORY.md`.
    Curated,
    /// A dated daily log.
    Daily { date: NaiveDate },
}

/// One indexed unit of memory, chunked from a Markdown file (RFC 0006 §4).
///
/// M5.0 defines the shape but only chunks for tests; the FTS5 index (M5.1)
/// consumes these. `embedding` is reserved for the optional hybrid backend
/// (M5.3) and is always `None` today.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryChunk {
    pub id: i64,
    pub source: MemorySource,
    pub path: PathBuf,
    /// Nearest Markdown heading, for context. `None` for pre-heading preamble.
    pub heading: Option<String>,
    /// The chunk body, including its heading line (FTS5-indexed in M5.1).
    pub text: String,
    /// 1-based, inclusive line span — lets `memory_get` target a chunk (M5.1).
    pub line_start: u32,
    pub line_end: u32,
    /// Reserved for the hybrid backend (M5.3); always `None` in FTS-only mode.
    pub embedding: Option<Vec<f32>>,
}

/// Memory behaviour knobs. Defaults make memory on with a small ambient budget;
/// `enabled = false` is the full disable path (RFC 0006 §8).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Byte budget for the curated section of the ambient block (a pragmatic
    /// stand-in for a token budget). The curated file is truncated to a
    /// line-boundary head past this, with a pointer to `memory_search`.
    #[serde(default = "default_budget")]
    pub injection_budget_bytes: usize,
}

fn default_enabled() -> bool {
    true
}
fn default_budget() -> usize {
    4096
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            injection_budget_bytes: default_budget(),
        }
    }
}

/// Owns the memory directory and renders the ambient block.
#[derive(Debug, Clone)]
pub struct Memory {
    root: PathBuf,
    config: MemoryConfig,
}

impl Memory {
    /// Bind to an explicit memory root (used in tests).
    pub fn new(root: impl Into<PathBuf>, config: MemoryConfig) -> Self {
        Self {
            root: root.into(),
            config,
        }
    }

    /// Bind to the default `~/.flowforge/memory/` root.
    pub fn with_default_root(config: MemoryConfig) -> Self {
        Self::new(default_root(), config)
    }

    /// Path to the curated `MEMORY.md`.
    pub fn curated_path(&self) -> PathBuf {
        self.root.join("MEMORY.md")
    }

    /// Path to a given day's log, `daily/YYYY-MM-DD.md`.
    pub fn daily_path(&self, date: NaiveDate) -> PathBuf {
        self.root
            .join("daily")
            .join(format!("{}.md", date.format("%Y-%m-%d")))
    }

    /// The ambient block to prepend to the system prompt, or `None` when memory
    /// is disabled or nothing has been recorded yet. Curated memory comes first
    /// (budget-bounded), then today + yesterday's daily log.
    pub fn ambient_block(&self) -> Option<String> {
        self.ambient_block_for(Local::now().date_naive())
    }

    /// [`ambient_block`](Self::ambient_block) with an injected "today" — the
    /// testable core (the public method just supplies the host clock).
    pub fn ambient_block_for(&self, today: NaiveDate) -> Option<String> {
        if !self.config.enabled {
            return None;
        }
        let curated = self.curated_section();
        let daily = self.daily_section(today);
        if curated.is_none() && daily.is_none() {
            return None;
        }
        let mut out = String::from("## Memory\n");
        if let Some(c) = curated {
            out.push('\n');
            out.push_str(&c);
            out.push('\n');
        }
        if let Some(d) = daily {
            out.push_str(&d);
        }
        Some(out)
    }

    fn curated_section(&self) -> Option<String> {
        let raw = read_lenient(&self.curated_path());
        let curated = raw.trim();
        if curated.is_empty() {
            return None;
        }
        let budget = self.config.injection_budget_bytes;
        if curated.len() > budget {
            let head = head_within(curated, budget);
            Some(format!(
                "{head}\n\n_(memory truncated — use `memory_search` for the rest)_"
            ))
        } else {
            Some(curated.to_string())
        }
    }

    fn daily_section(&self, today: NaiveDate) -> Option<String> {
        let yesterday = today.pred_opt().unwrap_or(today);
        let mut recent = String::new();
        // Oldest first so the most recent context is closest to the prompt tail.
        let mut days = vec![(yesterday, "Yesterday")];
        if yesterday != today {
            days.push((today, "Today"));
        }
        for (date, label) in days {
            let raw = read_lenient(&self.daily_path(date));
            let log = raw.trim();
            if !log.is_empty() {
                recent.push_str(&format!("\n### {label} ({date})\n{log}\n"));
            }
        }
        if recent.is_empty() {
            None
        } else {
            Some(format!("\n#### Recent daily log\n{recent}"))
        }
    }
}

/// The default memory root, `~/.flowforge/memory/` (falls back to `./.flowforge`
/// if the home directory cannot be resolved — matches the `~/.flowforge`
/// convention of `mcp.json` and `skills/`).
pub fn default_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".flowforge")
        .join("memory")
}

/// Read a file, treating a missing file as empty. Any other I/O error also
/// degrades to empty so a single unreadable file never breaks a turn.
fn read_lenient(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Largest line-boundary prefix of `text` within `budget` bytes; falls back to a
/// char-boundary cut when a single line already exceeds the budget.
fn head_within(text: &str, budget: usize) -> &str {
    if text.len() <= budget {
        return text;
    }
    let mut cut = 0;
    for (i, _) in text.match_indices('\n') {
        if i > budget {
            break;
        }
        cut = i;
    }
    if cut == 0 {
        let mut b = budget.min(text.len());
        while b > 0 && !text.is_char_boundary(b) {
            b -= 1;
        }
        &text[..b]
    } else {
        &text[..cut]
    }
}

/// Split Markdown into heading-anchored chunks (RFC 0006 §4). Each chunk runs from
/// a heading line to just before the next heading; content before the first
/// heading becomes a preamble chunk. Empty chunks are dropped. Used by the M5.1
/// index; defined here with the types it produces.
pub fn chunk_markdown(text: &str, source: MemorySource, path: &Path) -> Vec<MemoryChunk> {
    let mut chunks: Vec<MemoryChunk> = Vec::new();
    let mut heading: Option<String> = None;
    let mut start_line: u32 = 1;
    let mut body: Vec<&str> = Vec::new();

    let flush = |chunks: &mut Vec<MemoryChunk>,
                 heading: &Option<String>,
                 start: u32,
                 end: u32,
                 body: &[&str]| {
        let text = body.join("\n");
        if text.trim().is_empty() {
            return;
        }
        let id = chunks.len() as i64;
        chunks.push(MemoryChunk {
            id,
            source: source.clone(),
            path: path.to_path_buf(),
            heading: heading.clone(),
            text,
            line_start: start,
            line_end: end,
            embedding: None,
        });
    };

    let total = text.lines().count() as u32;
    for (i, raw) in text.lines().enumerate() {
        let lineno = (i + 1) as u32;
        if raw.trim_start().starts_with('#') {
            flush(
                &mut chunks,
                &heading,
                start_line,
                lineno.saturating_sub(1),
                &body,
            );
            heading = Some(raw.trim_start().trim_start_matches('#').trim().to_string());
            start_line = lineno;
            body = vec![raw];
        } else {
            body.push(raw);
        }
    }
    flush(
        &mut chunks,
        &heading,
        start_line,
        total.max(start_line),
        &body,
    );
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(root: &Path) -> Memory {
        Memory::new(root, MemoryConfig::default())
    }

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn missing_root_yields_no_ambient_block() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(mem(dir.path()).ambient_block(), None);
    }

    #[test]
    fn read_lenient_treats_missing_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_lenient(&dir.path().join("nope.md")), "");
    }

    #[test]
    fn curated_only_block() {
        let dir = tempfile::tempdir().unwrap();
        let m = mem(dir.path());
        write(&m.curated_path(), "User prefers Rust.\n");
        let block = m.ambient_block().unwrap();
        assert!(block.starts_with("## Memory\n"));
        assert!(block.contains("User prefers Rust."));
        assert!(!block.contains("Recent daily log"));
    }

    #[test]
    fn daily_today_and_yesterday_included_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let m = mem(dir.path());
        let today = NaiveDate::from_ymd_opt(2026, 6, 17).unwrap();
        let yesterday = NaiveDate::from_ymd_opt(2026, 6, 16).unwrap();
        write(&m.daily_path(today), "shipped the rename");
        write(&m.daily_path(yesterday), "filed M5 epic");
        let block = m.ambient_block_for(today).unwrap();
        let y = block.find("filed M5 epic").unwrap();
        let t = block.find("shipped the rename").unwrap();
        assert!(y < t, "yesterday should precede today: {block}");
        assert!(block.contains("Yesterday (2026-06-16)"));
        assert!(block.contains("Today (2026-06-17)"));
    }

    #[test]
    fn disabled_config_yields_nothing_even_with_files() {
        let dir = tempfile::tempdir().unwrap();
        let m = Memory::new(
            dir.path(),
            MemoryConfig {
                enabled: false,
                ..Default::default()
            },
        );
        write(&m.curated_path(), "should not appear");
        assert_eq!(m.ambient_block(), None);
    }

    #[test]
    fn oversized_curated_is_truncated_with_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let m = Memory::new(
            dir.path(),
            MemoryConfig {
                enabled: true,
                injection_budget_bytes: 40,
            },
        );
        let body = (0..20)
            .map(|i| format!("line {i} with some text"))
            .collect::<Vec<_>>()
            .join("\n");
        write(&m.curated_path(), &body);
        let block = m.ambient_block().unwrap();
        assert!(block.contains("memory truncated"));
        assert!(block.contains("memory_search"));
        assert!(block.contains("line 0"));
        assert!(!block.contains("line 19"));
    }

    #[test]
    fn head_within_cuts_on_line_boundary() {
        let text = "aaaa\nbbbb\ncccc\ndddd";
        // budget lands inside the third line; keep through the second.
        assert_eq!(head_within(text, 12), "aaaa\nbbbb");
    }

    #[test]
    fn head_within_returns_all_when_under_budget() {
        let text = "short";
        assert_eq!(head_within(text, 999), "short");
    }

    #[test]
    fn chunk_markdown_splits_on_headings() {
        let md =
            "# Title\nintro line\n\n## Prefs\nlikes rust\nhates yaml\n\n## Decisions\nuse sqlite";
        let chunks = chunk_markdown(md, MemorySource::Curated, Path::new("MEMORY.md"));
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].heading.as_deref(), Some("Title"));
        assert_eq!(chunks[1].heading.as_deref(), Some("Prefs"));
        assert!(chunks[1].text.contains("likes rust"));
        assert!(chunks[1].text.contains("hates yaml"));
        assert_eq!(chunks[2].heading.as_deref(), Some("Decisions"));
        assert!(chunks[2].embedding.is_none());
    }

    #[test]
    fn chunk_markdown_preamble_before_first_heading() {
        let md = "loose note\nanother\n# Heading\nbody";
        let chunks = chunk_markdown(md, MemorySource::Curated, Path::new("MEMORY.md"));
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading, None);
        assert_eq!(chunks[0].line_start, 1);
        assert_eq!(chunks[0].line_end, 2);
        assert!(chunks[0].text.contains("loose note"));
        assert_eq!(chunks[1].heading.as_deref(), Some("Heading"));
        assert_eq!(chunks[1].line_start, 3);
    }

    #[test]
    fn chunk_markdown_empty_input_yields_no_chunks() {
        assert!(chunk_markdown("", MemorySource::Curated, Path::new("x.md")).is_empty());
        assert!(chunk_markdown("   \n  \n", MemorySource::Curated, Path::new("x.md")).is_empty());
    }
}
