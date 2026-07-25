//! `flowforge memory` — direct, human-facing memory commands over the same
//! store + FTS5 index the agent's `memory_*` tools use (RFC 0006, issue #1081).
//!
//! The CLI already stands up the full durable-memory stack on startup
//! ([`build_memory_store`]); this module exposes it to a human at the terminal
//! without spending an agent turn. Three thin subcommands:
//!
//! ```
//! flowforge memory search "rust preferences"        # ranked FTS5 hits
//! flowforge memory get MEMORY.md --lines 1:20       # read a file / slice
//! flowforge memory write "shipped m5.1"             # append to today's log
//! flowforge memory write "L5 SDE" --curated --stratum identity
//! ```
//!
//! The `search` / `get` / `write` handlers mirror [`MemorySearchTool`] /
//! [`MemoryGetTool`] / [`MemoryWriteTool`] verbatim — same disabled-memory
//! short-circuit, same path jail, same stratum/target resolution, same
//! `Wrote to {rel_path}` shape — so the CLI and the agent surfaces stay in
//! lockstep. The only deliberate divergence: a CLI `search` does **not**
//! [`MemoryIndex::reinforce`] the surfaced hits (a human inspection must not
//! perturb the decay/dormancy model the agent recalls under, RFC 0007 §2).

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Subcommand;
use ff_memory::{chunk_markdown, Memory, MemoryIndex, MemorySource, Stratum, WriteTarget};

use crate::build_memory_store;

/// Mirrors `ff_tools::memory::DEFAULT_SEARCH_LIMIT` / `MAX_SEARCH_LIMIT` so the
/// CLI surface matches the agent tool without cross-crate coupling on two
/// integers. Re-bump here if the tool's bounds change.
const DEFAULT_SEARCH_LIMIT: usize = 5;
const MAX_SEARCH_LIMIT: usize = 20;

/// `flowforge memory <SUBCOMMAND>` (issue #1081). Each variant's fields double
/// as that subcommand's clap args, mirroring [`crate::Command`].
#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    /// Search durable memory (RFC 0006 §6): ranked BM25 recall over the
    /// curated `MEMORY.md` plus every daily log. Prints each hit's path,
    /// heading, and line range; follow up with `memory get` to read more.
    Search {
        /// Keywords or a short phrase to recall.
        query: String,
        /// Max results to return (default 5, max 20).
        #[arg(long, default_value_t = DEFAULT_SEARCH_LIMIT)]
        limit: usize,
    },
    /// Read a memory file, optionally sliced to a 1-based inclusive line range.
    /// Use the path and line numbers from a `memory search` hit to read the
    /// surrounding context. A missing file is "(no such memory file or empty)";
    /// a path outside the memory root is rejected as empty (never an error).
    Get {
        /// Memory file path, e.g. `MEMORY.md` or `daily/2026-06-18.md` (root-relative).
        path: String,
        /// Line range `A:B` (1-based, inclusive). `A` alone = line `A`; `A:` =
        /// from `A` to end; `:B` = start through `B`; omitted = whole file.
        #[arg(long, value_name = "A:B")]
        lines: Option<String>,
    },
    /// Append a note to durable memory and reindex so the new text is
    /// searchable in the same `ff memory` invocation. `--daily` (the default)
    /// writes to today's `daily/YYYY-MM-DD.md`; `--curated` writes to the
    /// long-lived `MEMORY.md`. For curated facts, `--stratum` files the note
    /// under the right section: `identity` (who), `patterns` (how), `focus`
    /// (what). Write Markdown; a `## Heading` makes the note easier to recall.
    Write {
        /// The Markdown note to append (quote multi-word notes).
        text: String,
        /// Append to today's daily log (the default). Mutually exclusive with
        /// `--curated` and `--stratum`.
        #[arg(long, conflicts_with_all = ["curated", "stratum"])]
        daily: bool,
        /// Append to the curated `MEMORY.md`. Mutually exclusive with `--daily`.
        #[arg(long, conflicts_with = "daily")]
        curated: bool,
        /// Curated section to file the note under (implies `--curated`).
        /// Mutually exclusive with `--daily`.
        #[arg(long, value_enum, conflicts_with = "daily")]
        stratum: Option<StratumArg>,
    },
}

/// CLI surface for a curated-memory stratum (RFC 0008 §4). Maps to
/// [`ff_memory::Stratum`] the same way [`crate::ModeArg`] maps to
/// [`ff_core::Mode`]: clap's `ValueEnum` derives a kebab-case wire name; the
/// `From` impl hands the parsed value to the memory store.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum StratumArg {
    Identity,
    Patterns,
    Focus,
}

impl From<StratumArg> for Stratum {
    fn from(arg: StratumArg) -> Self {
        match arg {
            StratumArg::Identity => Stratum::Identity,
            StratumArg::Patterns => Stratum::Patterns,
            StratumArg::Focus => Stratum::Focus,
        }
    }
}

/// Entry point dispatched from [`crate::main`]. Builds the store + index the
/// same way the agent tools do, then runs the requested subcommand. A disabled
/// store short-circuits to `"(memory is disabled)"` (exit 0); an index open
/// failure makes `search`/`write` report unavailability (exit non-zero) while
/// `get` still works against the files on disk.
pub async fn run(command: MemoryCommand) -> ExitCode {
    let (memory, memory_index) = build_memory_store();
    let result = match command {
        MemoryCommand::Search { query, limit } => {
            search(memory.as_ref(), memory_index.clone(), query, limit).await
        }
        MemoryCommand::Get { path, lines } => get(memory.as_ref(), &path, lines.as_deref()).await,
        MemoryCommand::Write {
            text,
            daily: _,
            curated,
            stratum,
        } => {
            let stratum = stratum.map(Stratum::from);
            let target = resolve_target(curated, stratum.is_some());
            write(memory.as_ref(), memory_index.clone(), text, target, stratum).await
        }
    };
    match result {
        Ok(msg) => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// Resolve the `--curated` / `--stratum` flags to a write target, mirroring
/// `MemoryWriteTool::run`'s resolution (`crates/ff-tools/src/memory.rs`).
/// `--stratum` implies curated; `--curated` overrides to curated; otherwise the
/// target is daily (the default — `--daily` is the explicit no-op form).
fn resolve_target(curated: bool, has_stratum: bool) -> WriteTarget {
    if has_stratum || curated {
        WriteTarget::Curated
    } else {
        WriteTarget::Daily
    }
}

/// `memory search` — ranked FTS5 recall. Mirrors [`MemorySearchTool::run`]
/// without the [`MemoryIndex::reinforce`] side-effect (a human inspection must
/// not perturb the decay model).
async fn search(
    memory: &Memory,
    index: Option<Arc<dyn MemoryIndex>>,
    query: String,
    limit: usize,
) -> Result<String, String> {
    if !memory.is_enabled() {
        return Ok("(memory is disabled)".to_string());
    }
    let Some(index) = index else {
        return Err("memory index unavailable".to_string());
    };
    if query.trim().is_empty() {
        return Ok("No matching memory.".to_string());
    }
    let limit = limit.clamp(1, MAX_SEARCH_LIMIT);
    // The hybrid backend may make a blocking embedding HTTP call inside
    // `search`; run it off the async worker so a slow/unreachable embedder never
    // parks a runtime thread (mirrors `MemorySearchTool::run`). It still
    // degrades to BM25 internally on embed failure.
    let join = tokio::task::spawn_blocking(move || index.search(&query, limit))
        .await
        .map_err(|e| format!("memory search task failed: {e}"))?;
    let hits = join.map_err(|e| format!("memory search failed: {e}"))?;
    if hits.is_empty() {
        return Ok("No matching memory.".to_string());
    }
    Ok(ff_tools::memory::format_hits(memory, &hits))
}

/// `memory get` — read a memory file (optionally a line range). Mirrors
/// [`MemoryGetTool::run`]: lenient (missing file → friendly empty), jailed
/// (paths outside the root read as empty so the CLI can't read arbitrary files).
async fn get(memory: &Memory, raw: &str, lines: Option<&str>) -> Result<String, String> {
    if !memory.is_enabled() {
        return Ok("(memory is disabled)".to_string());
    }
    let (line_start, line_end) = parse_lines(lines)?;
    // `Memory::get` jails the path itself (rejects `..` traversal, requires the
    // path to live under the memory root). A root-relative input is treated as
    // relative, exactly like the tool.
    let path = memory.root().join(raw);
    let content = memory.get(&path, line_start, line_end);
    if content.is_empty() {
        Ok("(no such memory file or empty)".to_string())
    } else {
        Ok(content)
    }
}

/// `memory write` — append a note and reindex so the new text is searchable in
/// the same `ff memory` invocation. Mirrors [`MemoryWriteTool::run`] verbatim:
/// same empty-text guard, same stratum/target dispatch, same `chunk_markdown` +
/// [`MemoryIndex::reindex_path`] post-write, same non-fatal reindex warning
/// (the Markdown is the source of truth; a failed reindex does not undo the write).
async fn write(
    memory: &Memory,
    index: Option<Arc<dyn MemoryIndex>>,
    text: String,
    target: WriteTarget,
    stratum: Option<Stratum>,
) -> Result<String, String> {
    if !memory.is_enabled() {
        return Ok("(memory is disabled)".to_string());
    }
    if text.trim().is_empty() {
        return Err("nothing to write: text is empty".to_string());
    }
    let write_result = match stratum {
        Some(st) => memory.write_curated_stratum(&text, st),
        None => memory.write(&text, target),
    };
    let path = write_result.map_err(|e| format!("memory write failed: {e}"))?;
    let rel = rel_path(memory, &path);

    // Reindex just the written file so a same-process `memory search` sees the
    // new note (mirrors `MemoryWriteTool::run`). A missing index is non-fatal:
    // the file is on disk and will be picked up by the next full reindex.
    let Some(index) = index else {
        return Ok(format!(
            "Wrote to {rel} (warning: index unavailable, note will be indexed on next start)"
        ));
    };
    let source = match target {
        WriteTarget::Daily => MemorySource::Daily {
            date: chrono::Local::now().date_naive(),
        },
        WriteTarget::Curated => MemorySource::Curated,
    };
    let full = memory.get(&path, None, None);
    let chunks = chunk_markdown(&full, source, &path);
    // The hybrid index may make a blocking embedding HTTP call inside
    // `reindex_path`; run it off the async worker (mirrors `MemoryWriteTool::run`).
    let join = tokio::task::spawn_blocking(move || index.reindex_path(&path, &chunks))
        .await
        .map_err(|e| format!("memory reindex task failed: {e}"))?;
    match join {
        Ok(()) => Ok(format!("Wrote to {rel}")),
        Err(e) => Ok(format!("Wrote to {rel} (warning: reindex failed: {e})")),
    }
}

/// Parse a `--lines A:B` spec into `(line_start, line_end)` for [`Memory::get`].
///
/// - `A:B` → `(Some(A), Some(B))`
/// - `A:`  → `(Some(A), None)` (from A to end)
/// - `:B`  → `(None, Some(B))` (start through B)
/// - `A`   → `(Some(A), Some(A))` (single line)
/// - ``    → `(None, None)` (whole file)
fn parse_lines(spec: Option<&str>) -> Result<(Option<u32>, Option<u32>), String> {
    let Some(spec) = spec else {
        return Ok((None, None));
    };
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok((None, None));
    }
    let parse = |s: &str, field: &str| -> Result<u32, String> {
        s.parse::<u32>()
            .map_err(|_| format!("invalid --lines {field} `{s}` (expected a positive integer)"))
    };
    match spec.split_once(':') {
        Some((lhs, rhs)) => {
            let start = if lhs.is_empty() {
                None
            } else {
                Some(parse(lhs, "start")?)
            };
            let end = if rhs.is_empty() {
                None
            } else {
                Some(parse(rhs, "end")?)
            };
            Ok((start, end))
        }
        None => {
            let n = parse(spec, "value")?;
            Ok((Some(n), Some(n)))
        }
    }
}

/// Display a chunk's path relative to the memory root (e.g. `MEMORY.md`,
/// `daily/2026-06-18.md`). Mirrors the private `rel_path` in
/// `crates/ff-tools/src/memory.rs` so the CLI prints the same shape the agent
/// tool emits.
fn rel_path(memory: &Memory, path: &Path) -> String {
    path.strip_prefix(memory.root())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ff_memory::{Fts5Index, MemoryConfig};
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

    #[test]
    fn parse_lines_handles_all_shapes() {
        assert_eq!(parse_lines(None).unwrap(), (None, None));
        assert_eq!(parse_lines(Some("")).unwrap(), (None, None));
        assert_eq!(parse_lines(Some("3")).unwrap(), (Some(3), Some(3)));
        assert_eq!(parse_lines(Some("3:8")).unwrap(), (Some(3), Some(8)));
        assert_eq!(parse_lines(Some("3:")).unwrap(), (Some(3), None));
        assert_eq!(parse_lines(Some(":8")).unwrap(), (None, Some(8)));
    }

    #[test]
    fn parse_lines_rejects_garbage() {
        assert!(parse_lines(Some("x")).is_err());
        assert!(parse_lines(Some("1:x")).is_err());
        assert!(parse_lines(Some("x:2")).is_err());
        assert!(parse_lines(Some("0:-1")).is_err());
    }

    #[test]
    fn resolve_target_defaults_to_daily() {
        assert_eq!(resolve_target(false, false), WriteTarget::Daily);
        assert_eq!(resolve_target(true, false), WriteTarget::Curated);
        assert_eq!(resolve_target(false, true), WriteTarget::Curated);
        assert_eq!(resolve_target(true, true), WriteTarget::Curated);
    }

    #[tokio::test]
    async fn search_returns_known_hit() {
        let (_dir, memory, index) = setup();
        write(
            memory.as_ref(),
            Some(index.clone()),
            "## Join key\nThe origin address id is the donor join key.".to_string(),
            WriteTarget::Curated,
            None,
        )
        .await
        .unwrap();

        let out = search(
            memory.as_ref(),
            Some(index),
            "origin address join key".to_string(),
            5,
        )
        .await
        .unwrap();
        assert!(out.contains("Join key"), "search miss: {out}");
        assert!(out.contains("MEMORY.md"), "{out}");
        // A manual CLI search must not reinforce the hit (RFC 0007 §2): the
        // weight stays at the FTS default. We assert on the formatted output
        // shape rather than stats; the no-reinforce contract is enforced by
        // the handler simply not calling `index.reinforce`.
        assert!(
            !out.contains("dormant"),
            "fresh hit must not be dormant: {out}"
        );
    }

    #[tokio::test]
    async fn search_empty_query_says_no_match() {
        let (_dir, memory, index) = setup();
        let out = search(memory.as_ref(), Some(index), "   ".to_string(), 5)
            .await
            .unwrap();
        assert_eq!(out, "No matching memory.");
    }

    #[tokio::test]
    async fn search_without_index_errors() {
        let (_dir, memory, _index) = setup();
        let err = search(memory.as_ref(), None, "anything".to_string(), 5)
            .await
            .unwrap_err();
        assert!(err.contains("index unavailable"), "{err}");
    }

    #[tokio::test]
    async fn write_then_search_then_get_round_trip() {
        let (_dir, memory, index) = setup();

        let wrote = write(
            memory.as_ref(),
            Some(index.clone()),
            "## Join key\nThe origin address id is the donor join key.".to_string(),
            WriteTarget::Curated,
            None,
        )
        .await
        .unwrap();
        assert!(wrote.contains("MEMORY.md"), "{}", wrote);

        let hit = search(
            memory.as_ref(),
            Some(index.clone()),
            "origin address join key".to_string(),
            5,
        )
        .await
        .unwrap();
        assert!(hit.contains("Join key"), "search miss: {hit}");

        let read = get(memory.as_ref(), "MEMORY.md", None).await.unwrap();
        assert!(read.contains("donor join key"), "{read}");
    }

    #[tokio::test]
    async fn write_with_stratum_files_under_heading() {
        let (_dir, memory, index) = setup();
        let out = write(
            memory.as_ref(),
            Some(index),
            "L5 SDE on Maps".to_string(),
            WriteTarget::Curated,
            Some(Stratum::Identity),
        )
        .await
        .unwrap();
        assert!(out.contains("MEMORY.md"), "{out}");
        let curated = memory.get(&memory.curated_path(), None, None);
        assert!(curated.contains("## Identity"), "{curated}");
        assert!(curated.contains("L5 SDE on Maps"), "{curated}");
    }

    #[tokio::test]
    async fn write_empty_text_is_error() {
        let (_dir, memory, index) = setup();
        let err = write(
            memory.as_ref(),
            Some(index),
            "   ".to_string(),
            WriteTarget::Daily,
            None,
        )
        .await
        .unwrap_err();
        assert!(err.contains("nothing to write"), "{err}");
    }

    #[tokio::test]
    async fn write_without_index_still_writes_with_warning() {
        let (_dir, memory, _index) = setup();
        let out = write(
            memory.as_ref(),
            None,
            "note without an index".to_string(),
            WriteTarget::Daily,
            None,
        )
        .await
        .unwrap();
        assert!(out.starts_with("Wrote to "), "{out}");
        assert!(out.contains("index unavailable"), "{out}");
        // The file is on disk — the source of truth.
        let files = memory.list_files();
        assert!(
            files.iter().any(|f| f.rel_path.starts_with("daily/")),
            "{files:?}"
        );
    }

    #[tokio::test]
    async fn get_missing_file_is_friendly_empty() {
        let (_dir, memory, _index) = setup();
        let out = get(memory.as_ref(), "MEMORY.md", None).await.unwrap();
        assert!(out.contains("no such memory file"), "{out}");
    }

    #[tokio::test]
    async fn get_rejects_path_traversal() {
        let dir = TempDir::new().unwrap();
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, "TOP-SECRET-KEY-12345").unwrap();
        let root = dir.path().join("memory");
        let memory = Memory::new(root, MemoryConfig::default());

        for p in [
            "../secret.txt",
            "../../secret.txt",
            "daily/../../secret.txt",
        ] {
            let out = get(&memory, p, None).await.unwrap();
            assert!(
                !out.contains("TOP-SECRET"),
                "path `{p}` leaked a file outside the memory root: {out}"
            );
        }
    }

    #[tokio::test]
    async fn get_with_line_range_slices_file() {
        let (_dir, memory, index) = setup();
        write(
            memory.as_ref(),
            Some(index),
            "## Heading\nline two\nline three\nline four".to_string(),
            WriteTarget::Curated,
            None,
        )
        .await
        .unwrap();

        let slice = get(memory.as_ref(), "MEMORY.md", Some("2:3"))
            .await
            .unwrap();
        assert!(slice.contains("line two"), "{slice}");
        assert!(slice.contains("line three"), "{slice}");
        assert!(!slice.contains("line four"), "{slice}");
        assert!(!slice.contains("## Heading"), "{slice}");
    }

    #[tokio::test]
    async fn disabled_memory_no_ops() {
        let dir = TempDir::new().unwrap();
        let memory = Memory::new(
            dir.path().to_path_buf(),
            MemoryConfig {
                enabled: false,
                ..MemoryConfig::default()
            },
        );
        let index: Arc<dyn MemoryIndex> = Arc::new(Fts5Index::open_in_memory().unwrap());

        // All three commands short-circuit to the disabled message, exit 0.
        assert_eq!(
            search(&memory, Some(index.clone()), "x".to_string(), 5)
                .await
                .unwrap(),
            "(memory is disabled)"
        );
        assert_eq!(
            get(&memory, "MEMORY.md", None).await.unwrap(),
            "(memory is disabled)"
        );
        assert_eq!(
            write(
                &memory,
                Some(index),
                "x".to_string(),
                WriteTarget::Daily,
                None,
            )
            .await
            .unwrap(),
            "(memory is disabled)"
        );
        // A disabled write must not create the file.
        assert!(!memory.curated_path().exists());
        assert!(memory.list_files().is_empty());
    }
}
