//! SQLite FTS5 recall index (RFC 0006 §6). The **default and floor** backend:
//! local, fast, no model, no cloud. The index is a **derived artifact** — it can
//! be deleted and rebuilt from the Markdown at any time (RFC 0006 §4).
//!
//! Layout is the standard FTS5 *external-content* pattern: a plain `chunks` table
//! holds the full rows (so search can return line spans and the reserved
//! `embedding` column lives here for the M5.3 hybrid backend), and a contentless
//! `chunks_fts` virtual table indexes only `text`, kept in sync by triggers.
//! Search joins the BM25 hit back to its row.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::NaiveDate;
use rusqlite::{params, Connection};

use crate::error::Result;
use crate::{MemoryChunk, MemorySource};

/// A search hit: the chunk plus a relevance score (higher = more relevant; it is
/// the negated BM25 distance, so callers can sort descending intuitively).
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredChunk {
    pub chunk: MemoryChunk,
    pub score: f32,
}

/// Recall backend (RFC 0006 §6). Swappable so M5.3 can add a hybrid vector index
/// behind the same seam; v1 ships [`Fts5Index`].
pub trait MemoryIndex: Send + Sync {
    /// Replace the entire index with `chunks` (full rebuild).
    fn reindex(&self, chunks: &[MemoryChunk]) -> Result<()>;
    /// Replace just the chunks for one file (used after a targeted write so a
    /// same-turn search sees the change without a full rebuild).
    fn reindex_path(&self, path: &Path, chunks: &[MemoryChunk]) -> Result<()>;
    /// Drop all chunks for a file that no longer exists.
    fn remove_path(&self, path: &Path) -> Result<()>;
    /// Ranked BM25 search. An empty/whitespace query yields no hits.
    fn search(&self, query: &str, k: usize) -> Result<Vec<ScoredChunk>>;
}

/// FTS5/BM25 index over a SQLite database. `open` on a path persists to disk;
/// [`open_in_memory`](Self::open_in_memory) is for tests.
pub struct Fts5Index {
    conn: Mutex<Connection>,
}

impl Fts5Index {
    /// Open (creating if absent) the index database at `path`, ensuring the schema.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).map_err(|source| crate::error::MemoryError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let conn = Connection::open(path)?;
        Self::from_conn(conn)
    }

    /// An ephemeral in-memory index (tests).
    pub fn open_in_memory() -> Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS chunks (
                 id         INTEGER PRIMARY KEY,
                 source     TEXT NOT NULL,
                 path       TEXT NOT NULL,
                 heading    TEXT,
                 text       TEXT NOT NULL,
                 line_start INTEGER NOT NULL,
                 line_end   INTEGER NOT NULL,
                 embedding  BLOB
             );
             CREATE INDEX IF NOT EXISTS idx_chunks_path ON chunks(path);
             CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts
                 USING fts5(text, content='chunks', content_rowid='id');
             CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
                 INSERT INTO chunks_fts(rowid, text) VALUES (new.id, new.text);
             END;
             CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
                 INSERT INTO chunks_fts(chunks_fts, rowid, text)
                     VALUES('delete', old.id, old.text);
             END;
             CREATE TRIGGER IF NOT EXISTS chunks_au AFTER UPDATE ON chunks BEGIN
                 INSERT INTO chunks_fts(chunks_fts, rowid, text)
                     VALUES('delete', old.id, old.text);
                 INSERT INTO chunks_fts(rowid, text) VALUES (new.id, new.text);
             END;",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn insert_chunks(conn: &Connection, chunks: &[MemoryChunk]) -> Result<()> {
        let mut stmt = conn.prepare(
            "INSERT INTO chunks (source, path, heading, text, line_start, line_end)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for c in chunks {
            stmt.execute(params![
                source_to_str(&c.source),
                c.path.to_string_lossy(),
                c.heading,
                c.text,
                c.line_start,
                c.line_end,
            ])?;
        }
        Ok(())
    }
}

impl MemoryIndex for Fts5Index {
    fn reindex(&self, chunks: &[MemoryChunk]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM chunks", [])?;
        Self::insert_chunks(&tx, chunks)?;
        tx.commit()?;
        Ok(())
    }

    fn reindex_path(&self, path: &Path, chunks: &[MemoryChunk]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM chunks WHERE path = ?1",
            params![path.to_string_lossy()],
        )?;
        Self::insert_chunks(&tx, chunks)?;
        tx.commit()?;
        Ok(())
    }

    fn remove_path(&self, path: &Path) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM chunks WHERE path = ?1",
            params![path.to_string_lossy()],
        )?;
        Ok(())
    }

    fn search(&self, query: &str, k: usize) -> Result<Vec<ScoredChunk>> {
        let Some(match_query) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.source, c.path, c.heading, c.text, c.line_start, c.line_end,
                    bm25(chunks_fts) AS score
             FROM chunks_fts
             JOIN chunks c ON c.id = chunks_fts.rowid
             WHERE chunks_fts MATCH ?1
             ORDER BY score
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![match_query, k as i64], |row| {
            let source: String = row.get(1)?;
            let path: String = row.get(2)?;
            let bm25: f64 = row.get(7)?;
            Ok(ScoredChunk {
                chunk: MemoryChunk {
                    id: row.get(0)?,
                    source: source_from_str(&source),
                    path: PathBuf::from(path),
                    heading: row.get(3)?,
                    text: row.get(4)?,
                    line_start: row.get(5)?,
                    line_end: row.get(6)?,
                    embedding: None,
                },
                // bm25() is a distance (lower = better, typically negative); negate
                // so a larger score means more relevant.
                score: -bm25 as f32,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

fn source_to_str(source: &MemorySource) -> String {
    match source {
        MemorySource::Curated => "curated".to_string(),
        MemorySource::Daily { date } => format!("daily:{}", date.format("%Y-%m-%d")),
    }
}

fn source_from_str(s: &str) -> MemorySource {
    match s.strip_prefix("daily:") {
        Some(date) => NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .map(|date| MemorySource::Daily { date })
            .unwrap_or(MemorySource::Curated),
        None => MemorySource::Curated,
    }
}

/// Turn arbitrary user text into a safe FTS5 MATCH expression: each whitespace
/// token becomes a quoted string (so `:`/`-`/`*` etc. can't trigger a syntax
/// error or an unintended operator), joined by implicit AND. `None` if empty.
fn fts_query(raw: &str) -> Option<String> {
    // Keep only alphanumeric runs as quoted terms (implicit AND). Dropping all
    // punctuation means FTS5 operators (`:` `-` `*` `"` `(`) in arbitrary user
    // text can never form a malformed MATCH or an empty phrase.
    let tokens: Vec<String> = raw
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\""))
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk_markdown;
    use std::path::Path;

    fn chunks(md: &str, path: &str) -> Vec<MemoryChunk> {
        chunk_markdown(md, MemorySource::Curated, Path::new(path))
    }

    #[test]
    fn search_ranks_and_returns_chunks() {
        let idx = Fts5Index::open_in_memory().unwrap();
        idx.reindex(&chunks(
            "## Prefs\nuser prefers rust over python\n\n## Tools\nuse sqlite for storage",
            "MEMORY.md",
        ))
        .unwrap();
        let hits = idx.search("rust", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].chunk.text.contains("rust"));
        assert_eq!(hits[0].chunk.heading.as_deref(), Some("Prefs"));
    }

    #[test]
    fn empty_query_returns_nothing() {
        let idx = Fts5Index::open_in_memory().unwrap();
        idx.reindex(&chunks("## H\nbody", "MEMORY.md")).unwrap();
        assert!(idx.search("   ", 10).unwrap().is_empty());
    }

    #[test]
    fn special_chars_do_not_error() {
        let idx = Fts5Index::open_in_memory().unwrap();
        idx.reindex(&chunks("## H\norigin address id join key", "MEMORY.md"))
            .unwrap();
        // Colons, hyphens, quotes — all would be FTS5 syntax without sanitizing.
        let hits = idx.search("origin: \"address\" - id", 10).unwrap();
        assert!(!hits.is_empty());
    }

    #[test]
    fn reindex_is_a_full_replace() {
        let idx = Fts5Index::open_in_memory().unwrap();
        idx.reindex(&chunks("## A\nalpha content", "MEMORY.md"))
            .unwrap();
        idx.reindex(&chunks("## B\nbeta content", "MEMORY.md"))
            .unwrap();
        assert!(idx.search("alpha", 10).unwrap().is_empty());
        assert_eq!(idx.search("beta", 10).unwrap().len(), 1);
    }

    #[test]
    fn reindex_path_replaces_only_that_file() {
        let idx = Fts5Index::open_in_memory().unwrap();
        idx.reindex_path(Path::new("a.md"), &chunks("## A\napple", "a.md"))
            .unwrap();
        idx.reindex_path(Path::new("b.md"), &chunks("## B\nbanana", "b.md"))
            .unwrap();
        // Rewriting a.md must not touch b.md.
        idx.reindex_path(Path::new("a.md"), &chunks("## A\navocado", "a.md"))
            .unwrap();
        assert!(idx.search("apple", 10).unwrap().is_empty());
        assert_eq!(idx.search("avocado", 10).unwrap().len(), 1);
        assert_eq!(idx.search("banana", 10).unwrap().len(), 1);
    }

    #[test]
    fn remove_path_drops_a_files_chunks() {
        let idx = Fts5Index::open_in_memory().unwrap();
        idx.reindex_path(Path::new("a.md"), &chunks("## A\napple", "a.md"))
            .unwrap();
        idx.remove_path(Path::new("a.md")).unwrap();
        assert!(idx.search("apple", 10).unwrap().is_empty());
    }

    #[test]
    fn daily_source_round_trips_through_index() {
        let idx = Fts5Index::open_in_memory().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 6, 18).unwrap();
        let cs = chunk_markdown(
            "## Log\nshipped m5.1",
            MemorySource::Daily { date },
            Path::new("daily/2026-06-18.md"),
        );
        idx.reindex(&cs).unwrap();
        let hits = idx.search("shipped", 10).unwrap();
        assert_eq!(hits[0].chunk.source, MemorySource::Daily { date });
    }

    #[test]
    fn persists_to_disk_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("index.db");
        {
            let idx = Fts5Index::open(&db).unwrap();
            idx.reindex(&chunks("## H\npersistent body", "MEMORY.md"))
                .unwrap();
        }
        let idx = Fts5Index::open(&db).unwrap();
        assert_eq!(idx.search("persistent", 10).unwrap().len(), 1);
    }
}
