//! Durable-memory recall tools (RFC 0006 §6–§7): `memory_search`, `memory_get`,
//! and `memory_write`. They operate on the user's memory store at
//! `~/.flowforge/memory` — *outside* the per-session workspace jail — so they
//! deliberately ignore the `root` argument every other tool is confined to.
//!
//! Search and get are [`Safety::ReadOnly`]; write is [`Safety::Write`] (the host
//! approval gate prompts before the model edits durable memory). All three no-op
//! gracefully when memory is disabled so a turn never fails on a recall call.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use ff_memory::{chunk_markdown, Memory, MemoryIndex, MemorySource, ScoredChunk, WriteTarget};
use serde_json::Value;

use crate::registry::{Safety, Tool, ToolOutcome};

const DEFAULT_SEARCH_LIMIT: usize = 5;
const MAX_SEARCH_LIMIT: usize = 20;

/// Display a chunk's path relative to the memory root (e.g. `MEMORY.md`,
/// `daily/2026-06-18.md`) so the model gets back a value it can feed to
/// `memory_get`.
fn rel_path(memory: &Memory, path: &Path) -> String {
    path.strip_prefix(memory.root())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Resolve a model-supplied path against the memory root. `memory_search` only
/// hands back root-relative paths, so all input is treated as relative;
/// `Memory::get` rejects anything that escapes the root (incl. `..` traversal).
fn resolve(memory: &Memory, raw: &str) -> PathBuf {
    memory.root().join(raw)
}

fn format_hits(memory: &Memory, hits: &[ScoredChunk]) -> String {
    let mut out = String::new();
    for (i, ScoredChunk { chunk, .. }) in hits.iter().enumerate() {
        let path = rel_path(memory, &chunk.path);
        let heading = chunk
            .heading
            .as_deref()
            .map(|h| format!(" \u{203a} {h}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "[{}] {path}{heading} (lines {}-{})\n{}\n\n",
            i + 1,
            chunk.line_start,
            chunk.line_end,
            chunk.text.trim()
        ));
    }
    out.trim_end().to_string()
}

/// `memory_search` — ranked BM25 recall over durable memory.
pub struct MemorySearchTool {
    memory: Arc<Memory>,
    index: Arc<dyn MemoryIndex>,
}

impl MemorySearchTool {
    pub fn new(memory: Arc<Memory>, index: Arc<dyn MemoryIndex>) -> Self {
        Self { memory, index }
    }
}

#[async_trait]
impl Tool for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn description(&self) -> &str {
        "Search your durable memory (notes you've kept across sessions about the user, \
         their projects, and decisions) for text relevant to a query. Returns ranked \
         snippets with their file path and line range; follow up with `memory_get` to \
         read more around a hit."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "What to recall — keywords or a short phrase."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results to return (default 5, max 20).",
                    "minimum": 1
                }
            },
            "required": ["query"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::ReadOnly
    }

    async fn run(&self, args: Value, _root: &Path) -> ToolOutcome {
        if !self.memory.is_enabled() {
            return ToolOutcome::ok("(memory is disabled)");
        }
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolOutcome::error("missing required argument: query (a string)");
        };
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| (n as usize).clamp(1, MAX_SEARCH_LIMIT))
            .unwrap_or(DEFAULT_SEARCH_LIMIT);

        match self.index.search(query, limit) {
            Ok(hits) if hits.is_empty() => ToolOutcome::ok("No matching memory."),
            Ok(hits) => ToolOutcome::ok(format_hits(&self.memory, &hits)),
            Err(e) => ToolOutcome::error(format!("memory search failed: {e}")),
        }
    }
}

/// `memory_get` — read a memory file (optionally a line range).
pub struct MemoryGetTool {
    memory: Arc<Memory>,
}

impl MemoryGetTool {
    pub fn new(memory: Arc<Memory>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl Tool for MemoryGetTool {
    fn name(&self) -> &str {
        "memory_get"
    }

    fn description(&self) -> &str {
        "Read a durable-memory file, optionally limited to a 1-based inclusive line \
         range. Use the path and line numbers from a `memory_search` hit to read the \
         surrounding context."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Memory file path, e.g. `MEMORY.md` or `daily/2026-06-18.md`."
                },
                "line_start": {
                    "type": "integer",
                    "description": "First line to read (1-based, inclusive).",
                    "minimum": 1
                },
                "line_end": {
                    "type": "integer",
                    "description": "Last line to read (1-based, inclusive).",
                    "minimum": 1
                }
            },
            "required": ["path"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::ReadOnly
    }

    async fn run(&self, args: Value, _root: &Path) -> ToolOutcome {
        if !self.memory.is_enabled() {
            return ToolOutcome::ok("(memory is disabled)");
        }
        let Some(raw) = args.get("path").and_then(Value::as_str) else {
            return ToolOutcome::error("missing required argument: path (a string)");
        };
        let line_start = args
            .get("line_start")
            .and_then(Value::as_u64)
            .map(|n| n as u32);
        let line_end = args
            .get("line_end")
            .and_then(Value::as_u64)
            .map(|n| n as u32);

        let path = resolve(&self.memory, raw);
        let content = self.memory.get(&path, line_start, line_end);
        if content.is_empty() {
            ToolOutcome::ok("(no such memory file or empty)")
        } else {
            ToolOutcome::ok(content)
        }
    }
}

/// `memory_write` — append to durable memory and reindex so the new text is
/// searchable within the same turn.
pub struct MemoryWriteTool {
    memory: Arc<Memory>,
    index: Arc<dyn MemoryIndex>,
}

impl MemoryWriteTool {
    pub fn new(memory: Arc<Memory>, index: Arc<dyn MemoryIndex>) -> Self {
        Self { memory, index }
    }
}

#[async_trait]
impl Tool for MemoryWriteTool {
    fn name(&self) -> &str {
        "memory_write"
    }

    fn description(&self) -> &str {
        "Append a note to your durable memory so you remember it in future sessions. \
         `target` is `daily` (today's log — for time-stamped observations, the default) \
         or `curated` (the long-lived MEMORY.md — for stable facts about the user and \
         their projects). Write Markdown; a `## Heading` makes the note easier to recall."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The Markdown note to append."
                },
                "target": {
                    "type": "string",
                    "enum": ["daily", "curated"],
                    "description": "Where to write (default `daily`)."
                }
            },
            "required": ["text"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::Write
    }

    async fn run(&self, args: Value, _root: &Path) -> ToolOutcome {
        if !self.memory.is_enabled() {
            return ToolOutcome::ok("(memory is disabled)");
        }
        let Some(text) = args.get("text").and_then(Value::as_str) else {
            return ToolOutcome::error("missing required argument: text (a string)");
        };
        if text.trim().is_empty() {
            return ToolOutcome::error("nothing to write: text is empty");
        }
        let target = match args.get("target").and_then(Value::as_str) {
            None | Some("daily") => WriteTarget::Daily,
            Some("curated") => WriteTarget::Curated,
            Some(other) => {
                return ToolOutcome::error(format!(
                    "invalid target `{other}` (expected `daily` or `curated`)"
                ));
            }
        };

        let path = match self.memory.write(text, target) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::error(format!("memory write failed: {e}")),
        };

        let source = match target {
            WriteTarget::Daily => MemorySource::Daily {
                date: chrono::Local::now().date_naive(),
            },
            WriteTarget::Curated => MemorySource::Curated,
        };
        let full = self.memory.get(&path, None, None);
        let chunks = chunk_markdown(&full, source, &path);
        if let Err(e) = self.index.reindex_path(&path, &chunks) {
            return ToolOutcome::ok(format!(
                "Wrote to {} (warning: reindex failed: {e})",
                rel_path(&self.memory, &path)
            ));
        }
        ToolOutcome::ok(format!("Wrote to {}", rel_path(&self.memory, &path)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ff_memory::{Fts5Index, MemoryConfig};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Arc<Memory>, Arc<dyn MemoryIndex>) {
        let dir = TempDir::new().unwrap();
        let memory = Arc::new(Memory::new(
            dir.path().to_path_buf(),
            MemoryConfig::default(),
        ));
        let index: Arc<dyn MemoryIndex> = Arc::new(Fts5Index::open_in_memory().unwrap());
        (dir, memory, index)
    }

    fn disabled() -> (TempDir, Arc<Memory>, Arc<dyn MemoryIndex>) {
        let dir = TempDir::new().unwrap();
        let memory = Arc::new(Memory::new(
            dir.path().to_path_buf(),
            MemoryConfig {
                enabled: false,
                ..MemoryConfig::default()
            },
        ));
        let index: Arc<dyn MemoryIndex> = Arc::new(Fts5Index::open_in_memory().unwrap());
        (dir, memory, index)
    }

    #[tokio::test]
    async fn write_then_search_then_get_round_trip() {
        let (_dir, memory, index) = setup();
        let write = MemoryWriteTool::new(memory.clone(), index.clone());
        let search = MemorySearchTool::new(memory.clone(), index.clone());
        let get = MemoryGetTool::new(memory.clone());

        let out = write
            .run(
                serde_json::json!({
                    "text": "## Join key\nThe origin address id is the donor join key.",
                    "target": "curated"
                }),
                Path::new("."),
            )
            .await;
        assert!(out.success, "{}", out.content);
        assert!(out.content.contains("MEMORY.md"), "{}", out.content);

        let hit = search
            .run(
                serde_json::json!({ "query": "origin address join key" }),
                Path::new("."),
            )
            .await;
        assert!(hit.success, "{}", hit.content);
        assert!(
            hit.content.contains("Join key"),
            "search miss: {}",
            hit.content
        );
        assert!(hit.content.contains("MEMORY.md"), "{}", hit.content);

        let read = get
            .run(serde_json::json!({ "path": "MEMORY.md" }), Path::new("."))
            .await;
        assert!(read.success);
        assert!(read.content.contains("donor join key"), "{}", read.content);
    }

    #[tokio::test]
    async fn search_empty_yields_no_match() {
        let (_dir, memory, index) = setup();
        let search = MemorySearchTool::new(memory, index);
        let out = search
            .run(
                serde_json::json!({ "query": "nonexistent" }),
                Path::new("."),
            )
            .await;
        assert!(out.success);
        assert_eq!(out.content, "No matching memory.");
    }

    #[tokio::test]
    async fn search_missing_query_is_error() {
        let (_dir, memory, index) = setup();
        let search = MemorySearchTool::new(memory, index);
        let out = search.run(serde_json::json!({}), Path::new(".")).await;
        assert!(!out.success);
        assert!(out.content.contains("missing required argument: query"));
    }

    #[tokio::test]
    async fn get_missing_file_is_ok_empty() {
        let (_dir, memory, _index) = setup();
        let get = MemoryGetTool::new(memory);
        let out = get
            .run(serde_json::json!({ "path": "MEMORY.md" }), Path::new("."))
            .await;
        assert!(out.success);
        assert!(out.content.contains("no such memory file"));
    }

    #[tokio::test]
    async fn get_rejects_path_traversal() {
        let dir = TempDir::new().unwrap();
        // A secret sibling outside the memory root.
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, "TOP-SECRET-KEY-12345").unwrap();
        let root = dir.path().join("memory");
        let memory = Arc::new(Memory::new(root, MemoryConfig::default()));
        let get = MemoryGetTool::new(memory);

        for p in [
            "../secret.txt",
            "../../secret.txt",
            "daily/../../secret.txt",
        ] {
            let out = get
                .run(serde_json::json!({ "path": p }), Path::new("."))
                .await;
            assert!(out.success, "{}", out.content);
            assert!(
                !out.content.contains("TOP-SECRET"),
                "path `{p}` leaked a file outside the memory root: {}",
                out.content
            );
        }
    }

    #[tokio::test]
    async fn write_empty_text_is_error() {
        let (_dir, memory, index) = setup();
        let write = MemoryWriteTool::new(memory, index);
        let out = write
            .run(serde_json::json!({ "text": "   " }), Path::new("."))
            .await;
        assert!(!out.success);
        assert!(out.content.contains("nothing to write"));
    }

    #[tokio::test]
    async fn write_invalid_target_is_error() {
        let (_dir, memory, index) = setup();
        let write = MemoryWriteTool::new(memory, index);
        let out = write
            .run(
                serde_json::json!({ "text": "x", "target": "weekly" }),
                Path::new("."),
            )
            .await;
        assert!(!out.success);
        assert!(out.content.contains("invalid target"));
    }

    #[tokio::test]
    async fn disabled_memory_tools_no_op() {
        let (_dir, memory, index) = disabled();
        let search = MemorySearchTool::new(memory.clone(), index.clone());
        let write = MemoryWriteTool::new(memory.clone(), index.clone());
        let get = MemoryGetTool::new(memory.clone());

        for out in [
            search
                .run(serde_json::json!({ "query": "x" }), Path::new("."))
                .await,
            write
                .run(serde_json::json!({ "text": "x" }), Path::new("."))
                .await,
            get.run(serde_json::json!({ "path": "MEMORY.md" }), Path::new("."))
                .await,
        ] {
            assert!(out.success);
            assert_eq!(out.content, "(memory is disabled)");
        }
        // Disabled write must not create the file.
        assert!(!memory.curated_path().exists());
    }

    #[tokio::test]
    async fn safety_classifications() {
        let (_dir, memory, index) = setup();
        let search = MemorySearchTool::new(memory.clone(), index.clone());
        let get = MemoryGetTool::new(memory.clone());
        let write = MemoryWriteTool::new(memory, index);
        let empty = serde_json::json!({});
        assert_eq!(search.safety(&empty), Safety::ReadOnly);
        assert_eq!(get.safety(&empty), Safety::ReadOnly);
        assert_eq!(write.safety(&empty), Safety::Write);
    }
}
