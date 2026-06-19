//! Durable, local-first user memory (RFC 0006, M5).
//!
//! Memory is **plain Markdown the user owns**, under `~/.flowforge/memory/`:
//! a small curated `MEMORY.md` (facts, preferences, decisions) and an
//! append-only `daily/YYYY-MM-DD.md` working log. This crate owns those files,
//! the (later) index, and recall.
//!
//! **Ambient injection** ([`Memory::ambient_block`]) renders a compact,
//! budget-bounded block — curated memory plus today + yesterday's daily log —
//! that the host prepends to the system prompt through the RFC 0001 §4 hook (the
//! same seam RFC 0002 uses for ambient context).
//!
//! **Recall** (M5.1) reaches past the ambient window: an [`Fts5Index`] (SQLite
//! FTS5/BM25, a deletable/rebuildable derived artifact) backs [`Memory::get`] and
//! the search/write helpers the `memory_search` / `memory_get` / `memory_write`
//! tools expose. A debounced [`watch`] reindexes on edit.
//!
//! Reads are **lenient**: a missing file is "nothing recorded yet" (empty), never
//! an error, so callers never need a try/catch around a fresh install.

mod embed;
mod error;
pub mod flush;
pub mod index;
pub mod watch;

pub use embed::{Embedder, NoopEmbedder, OpenAiEmbedder};
pub use error::{MemoryError, Result};
pub use flush::{FlushLedger, FlushRecord};
pub use index::{Fts5Index, HybridIndex, MemoryIndex, ScoredChunk};

use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Where a chunk came from (RFC 0006 §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemorySource {
    /// The curated `MEMORY.md`.
    Curated,
    /// A dated daily log.
    Daily { date: NaiveDate },
}

/// Where a `memory_write` lands (RFC 0006 §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteTarget {
    /// Append to today's daily log — the agent's free, frequent write.
    Daily,
    /// Append to the curated `MEMORY.md` — conservative; only durable facts.
    Curated,
}

/// Which memory file a [`MemoryFile`] is. Mirrors [`MemorySource`] but is a plain,
/// flat enum for the read-only Settings browse surface (M5.1e).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryFileKind {
    /// The curated `MEMORY.md`.
    Curated,
    /// A dated daily log under `daily/`.
    Daily,
}

/// A memory file as the Settings pane sees it (RFC 0006 §8, #131): name, root-
/// relative path, kind, size, and mtime. Read-only; no contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryFile {
    /// File name, e.g. `MEMORY.md` or `2026-06-18.md`.
    pub name: String,
    /// Path relative to the memory root, e.g. `MEMORY.md` or `daily/2026-06-18.md`.
    pub rel_path: String,
    pub kind: MemoryFileKind,
    pub size_bytes: u64,
    /// Last-modified time in epoch milliseconds, or 0 if unavailable.
    pub modified_ms: i64,
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
///
/// Note (M5.3.x persistence): `rename_all = "camelCase"` is safe today because
/// this is only ever built via `Default` and never deserialized from disk. When
/// on-disk persistence lands, keep the file camelCase too (or add an explicit
/// migration) so a snake_case settings file isn't silently reintroduced.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct MemoryConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Byte budget for the curated section of the ambient block (a pragmatic
    /// stand-in for a token budget). The curated file is truncated to a
    /// line-boundary head past this, with a pointer to `memory_search`.
    #[serde(default = "default_budget")]
    #[ts(type = "number")]
    pub injection_budget_bytes: usize,
    /// Hybrid (semantic) recall settings (M5.3). Off by default: recall stays
    /// pure FTS5/BM25 until a user opts in and an embedder is wired.
    #[serde(default)]
    pub embeddings: EmbeddingsConfig,
}

/// Hybrid recall configuration (RFC 0006, M5.3). When `enabled = false`
/// (the default) recall is byte-identical to the FTS5/BM25 path.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct EmbeddingsConfig {
    /// Opt in to semantic recall. Off keeps the FTS-only behaviour.
    pub enabled: bool,
    /// Which embedder backs semantic recall when `enabled`.
    pub provider: EmbeddingProvider,
}

/// Where chunk embeddings come from (M5.3). `Local` is the default on-device
/// path; `Cloud` is reserved for the M5.3.2 hosted embedder.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum EmbeddingProvider {
    /// On-device embedder (default).
    #[default]
    Local,
    /// Hosted embedder (reserved for M5.3.2).
    Cloud,
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
            embeddings: EmbeddingsConfig::default(),
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

    /// The derived FTS5 index database, `~/.flowforge/memory/index.db`.
    pub fn index_path(&self) -> PathBuf {
        self.root.join("index.db")
    }

    /// The memory root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether memory is enabled (RFC 0006 §8). Recall tools no-op when `false`.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Read a memory file, optionally sliced to a 1-based inclusive line range.
    /// Backs `memory_get`: a missing file is empty (never an error), and a `path`
    /// outside the memory root is rejected as empty so the tool can't read
    /// arbitrary files.
    pub fn get(&self, path: &Path, line_start: Option<u32>, line_end: Option<u32>) -> String {
        if !self.within_root(path) {
            return String::new();
        }
        let raw = read_lenient(path);
        match (line_start, line_end) {
            (None, None) => raw,
            _ => {
                let start = line_start.unwrap_or(1).max(1) as usize;
                let end = line_end.unwrap_or(u32::MAX) as usize;
                raw.lines()
                    .enumerate()
                    .filter(|(i, _)| {
                        let n = i + 1;
                        n >= start && n <= end
                    })
                    .map(|(_, l)| l)
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
    }

    /// Append `text` to the chosen target, creating the file/dirs if needed.
    /// Returns the file written so the caller can reindex just that path.
    pub fn write(&self, text: &str, target: WriteTarget) -> Result<PathBuf> {
        let path = match target {
            WriteTarget::Daily => self.daily_path(Local::now().date_naive()),
            WriteTarget::Curated => self.curated_path(),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| MemoryError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let existing = read_lenient(&path);
        let mut body = String::new();
        if !existing.is_empty() && !existing.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(text.trim_end());
        body.push('\n');
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| MemoryError::Io {
                path: path.clone(),
                source,
            })?;
        file.write_all(body.as_bytes())
            .map_err(|source| MemoryError::Io {
                path: path.clone(),
                source,
            })?;
        Ok(path)
    }

    /// Chunk every memory file (curated + all daily logs) — the input to a full
    /// [`MemoryIndex::reindex`].
    pub fn all_chunks(&self) -> Vec<MemoryChunk> {
        let mut out = Vec::new();
        let curated = self.curated_path();
        out.extend(chunk_markdown(
            &read_lenient(&curated),
            MemorySource::Curated,
            &curated,
        ));
        if let Ok(entries) = std::fs::read_dir(self.root.join("daily")) {
            let mut files: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
                .collect();
            files.sort();
            for path in files {
                let source = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                    .map(|date| MemorySource::Daily { date })
                    .unwrap_or(MemorySource::Curated);
                out.extend(chunk_markdown(&read_lenient(&path), source, &path));
            }
        }
        out
    }

    /// List every memory file for the read-only Settings browse surface (M5.1e,
    /// #131): the curated `MEMORY.md` first, then daily logs newest-first. Skips
    /// the derived `index.db` and any non-Markdown entry. Missing files/dirs yield
    /// an empty list (never an error).
    pub fn list_files(&self) -> Vec<MemoryFile> {
        let mut out = Vec::new();
        let curated = self.curated_path();
        if curated.is_file() {
            if let Some(f) = self.describe_file(&curated, MemoryFileKind::Curated) {
                out.push(f);
            }
        }
        if let Ok(entries) = std::fs::read_dir(self.root.join("daily")) {
            let mut daily: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("md"))
                .collect();
            // Newest-first: file names are `YYYY-MM-DD.md`, so a reverse lexical
            // sort is chronological.
            daily.sort();
            daily.reverse();
            for path in daily {
                if let Some(f) = self.describe_file(&path, MemoryFileKind::Daily) {
                    out.push(f);
                }
            }
        }
        out
    }

    /// Read a memory file by its root-relative path (as handed out by
    /// [`list_files`](Self::list_files)). Returns `None` when the path escapes the
    /// root (reuses the hardened [`within_root`](Self::within_root) from #176);
    /// a missing file within the root reads as `Some(String::new())`.
    pub fn read_file(&self, rel_path: &str) -> Option<String> {
        let path = self.root.join(rel_path);
        if !self.within_root(&path) {
            return None;
        }
        Some(read_lenient(&path))
    }

    /// Build a [`MemoryFile`] from a path on disk; `None` if metadata is
    /// unreadable. `rel_path` strips the root prefix for display.
    fn describe_file(&self, path: &Path, kind: MemoryFileKind) -> Option<MemoryFile> {
        let meta = std::fs::metadata(path).ok()?;
        let modified_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        // Forward slashes so the wire contract is platform-stable; `read_file`
        // joins it back onto the root regardless of OS separator.
        let rel_path = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        Some(MemoryFile {
            name: path.file_name()?.to_string_lossy().into_owned(),
            rel_path,
            kind,
            size_bytes: meta.len(),
            modified_ms,
        })
    }

    /// Containment check. Paths may not exist yet (so no `canonicalize`); reject
    /// any `..` component outright — legit memory paths are flat (MEMORY.md,
    /// daily/YYYY-MM-DD.md) — so `<root>/../../etc/passwd` can't slip past the
    /// component-wise `starts_with`.
    fn within_root(&self, path: &Path) -> bool {
        use std::path::Component;
        if path.components().any(|c| c == Component::ParentDir) {
            return false;
        }
        path.starts_with(&self.root)
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
                recent.push_str(&format!("\n#### {label} ({date})\n{log}\n"));
            }
        }
        if recent.is_empty() {
            None
        } else {
            Some(format!("\n### Recent daily log\n{recent}"))
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

/// Target byte size for a memory chunk, and the overlap carried between windows
/// when a section must be split (RFC 0006 §11.4). Heading-anchored sections are
/// the primary boundary; only a section whose joined text exceeds the target is
/// broken into overlapping line-windows, so today's small memory files stay a
/// single chunk (byte-identical BM25 behaviour) and only genuinely large sections
/// get windowed for focused embeddings. ~2 KB approximates 512 tokens; ~15%
/// overlap preserves context across a split.
const CHUNK_TARGET_BYTES: usize = 2048;
const CHUNK_OVERLAP_BYTES: usize = 307;

/// Greedy line-windows over `lines`: each window's joined byte length stays within
/// `target` where it can (a single oversized line still forms its own window), and
/// successive windows overlap by about `overlap` bytes. Returns half-open
/// `[start, end)` index ranges that cover every line and always advance, so a
/// pathological section can never loop. Splitting on line boundaries keeps every
/// chunk's reported line span exact for `memory_get`.
fn window_line_ranges(lines: &[&str], target: usize, overlap: usize) -> Vec<(usize, usize)> {
    if lines.is_empty() {
        return Vec::new();
    }
    // +1 approximates the `\n` join separator that sits between two lines.
    let len_of = |i: usize| lines[i].len() + 1;
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        let mut end = start;
        let mut acc = 0;
        while end < lines.len() && (end == start || acc + len_of(end) <= target) {
            acc += len_of(end);
            end += 1;
        }
        ranges.push((start, end));
        if end >= lines.len() {
            break;
        }
        // Carry ~`overlap` bytes into the next window, but never step back to or
        // past `start`, so the loop always makes forward progress.
        let mut back = end;
        let mut carried = 0;
        while back > start + 1 && carried < overlap {
            back -= 1;
            carried += len_of(back);
        }
        start = back;
    }
    ranges
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
        let joined = body.join("\n");
        if joined.trim().is_empty() {
            return;
        }
        // Small sections (the common case) stay a single chunk -> byte-identical
        // BM25 behaviour. Only a section past the target is split into overlapping
        // line-windows so its embeddings stay focused (RFC 0006 sec 11.4).
        if joined.len() <= CHUNK_TARGET_BYTES {
            let id = chunks.len() as i64;
            chunks.push(MemoryChunk {
                id,
                source: source.clone(),
                path: path.to_path_buf(),
                heading: heading.clone(),
                text: joined,
                line_start: start,
                line_end: end,
                embedding: None,
            });
            return;
        }
        for (s, e) in window_line_ranges(body, CHUNK_TARGET_BYTES, CHUNK_OVERLAP_BYTES) {
            let text = body[s..e].join("\n");
            if text.trim().is_empty() {
                continue;
            }
            let id = chunks.len() as i64;
            chunks.push(MemoryChunk {
                id,
                source: source.clone(),
                path: path.to_path_buf(),
                heading: heading.clone(),
                text,
                line_start: start + s as u32,
                line_end: start + e as u32 - 1,
                embedding: None,
            });
        }
    };

    let total = text.lines().count() as u32;
    // A `#` line inside a fenced code block (``` or ~~~) is code, not a heading —
    // a shell/python comment pasted into a daily log must not split a chunk.
    let mut in_fence = false;
    for (i, raw) in text.lines().enumerate() {
        let lineno = (i + 1) as u32;
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            body.push(raw);
        } else if !in_fence && trimmed.starts_with('#') {
            flush(
                &mut chunks,
                &heading,
                start_line,
                lineno.saturating_sub(1),
                &body,
            );
            heading = Some(trimmed.trim_start_matches('#').trim().to_string());
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

    #[test]
    fn get_rejects_path_traversal_and_absolute_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("memory");
        std::fs::create_dir_all(&root).unwrap();
        // A secret sibling outside the memory root.
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, "TOP-SECRET").unwrap();
        let m = mem(&root);
        // Relative traversal out of the root must not read the sibling.
        assert_eq!(m.get(&root.join("../secret.txt"), None, None), "");
        // An absolute path outside the root is likewise rejected.
        assert_eq!(m.get(&secret, None, None), "");
    }

    #[test]
    fn list_files_orders_curated_then_daily_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("MEMORY.md"), "# curated\nhello").unwrap();
        std::fs::create_dir_all(root.join("daily")).unwrap();
        std::fs::write(root.join("daily/2026-06-16.md"), "older").unwrap();
        std::fs::write(root.join("daily/2026-06-18.md"), "newer").unwrap();
        // A non-Markdown sibling and the derived index must be ignored.
        std::fs::write(root.join("index.db"), "binary").unwrap();
        std::fs::write(root.join("daily/notes.txt"), "x").unwrap();

        let files = mem(root).list_files();
        let names: Vec<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
        assert_eq!(
            names,
            vec!["MEMORY.md", "daily/2026-06-18.md", "daily/2026-06-16.md"]
        );
        assert_eq!(files[0].kind, MemoryFileKind::Curated);
        assert_eq!(files[1].kind, MemoryFileKind::Daily);
        assert!(files[0].size_bytes > 0);
        assert!(files.iter().all(|f| f.modified_ms >= 0));
    }

    #[test]
    fn list_files_empty_when_nothing_recorded() {
        let dir = tempfile::tempdir().unwrap();
        assert!(mem(dir.path()).list_files().is_empty());
    }

    #[test]
    fn read_file_round_trips_and_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("memory");
        std::fs::create_dir_all(root.join("daily")).unwrap();
        std::fs::write(root.join("MEMORY.md"), "curated body").unwrap();
        std::fs::write(root.join("daily/2026-06-18.md"), "daily body").unwrap();
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, "TOP-SECRET").unwrap();
        let m = mem(&root);

        assert_eq!(m.read_file("MEMORY.md").as_deref(), Some("curated body"));
        assert_eq!(
            m.read_file("daily/2026-06-18.md").as_deref(),
            Some("daily body")
        );
        // Missing-but-in-root reads as empty, never an error.
        assert_eq!(m.read_file("daily/2099-01-01.md").as_deref(), Some(""));
        // Traversal escapes are rejected outright.
        assert_eq!(m.read_file("../secret.txt"), None);
        assert_eq!(m.read_file("daily/../../secret.txt"), None);
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
                ..Default::default()
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

    #[test]
    fn small_section_is_a_single_chunk_unchanged() {
        // A section well under the target stays one chunk with the heading-anchored
        // line span -> byte-identical to the pre-windowing behaviour.
        let md = "# h\nshort body line one\nshort body line two";
        let chunks = chunk_markdown(md, MemorySource::Curated, Path::new("x.md"));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].line_start, 1);
        assert_eq!(chunks[0].line_end, 3);
        assert_eq!(chunks[0].text, md);
    }

    #[test]
    fn oversized_section_splits_into_overlapping_windows_with_exact_line_spans() {
        // Build one heading section far larger than CHUNK_TARGET_BYTES.
        let mut md = String::from("# big\n");
        for i in 0..200 {
            md.push_str(&format!(
                "line {i:03} with enough text to add real bytes here\n"
            ));
        }
        let chunks = chunk_markdown(&md, MemorySource::Curated, Path::new("x.md"));
        assert!(chunks.len() > 1, "oversized section should window");
        // Every sub-chunk inherits the heading and stays within the target (each
        // window is allowed to overshoot only by its first line).
        for c in &chunks {
            assert_eq!(c.heading.as_deref(), Some("big"));
            assert!(c.line_start >= 1 && c.line_end >= c.line_start);
        }
        // Windows are contiguous-with-overlap: each starts at or before the
        // previous window's end (the carried-over context), and the last window
        // reaches the final line of the section.
        let total_lines = md.lines().count() as u32;
        for pair in chunks.windows(2) {
            assert!(pair[1].line_start <= pair[0].line_end + 1);
            assert!(pair[1].line_start > pair[0].line_start);
        }
        assert_eq!(chunks.last().unwrap().line_end, total_lines);
        // Sub-chunk text matches its reported line span exactly.
        let lines: Vec<&str> = md.lines().collect();
        for c in &chunks {
            let expected =
                lines[(c.line_start - 1) as usize..=(c.line_end - 1) as usize].join("\n");
            assert_eq!(c.text, expected);
        }
    }

    #[test]
    fn window_line_ranges_covers_all_lines_and_advances() {
        let lines: Vec<&str> = vec!["aaaa"; 50];
        let ranges = window_line_ranges(&lines, 20, 6);
        assert!(ranges.len() > 1);
        assert_eq!(ranges.first().unwrap().0, 0);
        assert_eq!(ranges.last().unwrap().1, lines.len());
        for pair in ranges.windows(2) {
            assert!(pair[1].0 > pair[0].0, "must advance");
            assert!(pair[1].0 < pair[0].1, "must overlap");
        }
    }

    #[test]
    fn memory_config_default_keeps_embeddings_off() {
        let cfg = MemoryConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.injection_budget_bytes, 4096);
        assert!(!cfg.embeddings.enabled);
        assert_eq!(cfg.embeddings.provider, EmbeddingProvider::Local);
    }
}
