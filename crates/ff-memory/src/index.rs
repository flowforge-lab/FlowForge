//! SQLite FTS5 recall index (RFC 0006 §6). The **default and floor** backend:
//! local, fast, no model, no cloud. The index is a **derived artifact** — it can
//! be deleted and rebuilt from the Markdown at any time (RFC 0006 §4).
//!
//! Layout is the standard FTS5 *external-content* pattern: a plain `chunks` table
//! holds the full rows (so search can return line spans and the reserved
//! `embedding` column lives here for the M5.3 hybrid backend), and a contentless
//! `chunks_fts` virtual table indexes only `text`, kept in sync by triggers.
//! Search joins the BM25 hit back to its row.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{NaiveDate, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use crate::consolidate::chunk_key;
use crate::embed::Embedder;

use crate::error::Result;
use crate::{DecayConfig, MemoryChunk, MemorySource};

/// Milliseconds in a day, for converting `last_accessed` deltas to fractional
/// decay days (RFC 0007 §2).
const ONE_DAY_MS: f32 = 86_400_000.0;

/// A search hit: the chunk plus a relevance score (higher = more relevant; it is
/// the negated BM25 distance, so callers can sort descending intuitively).
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredChunk {
    pub chunk: MemoryChunk,
    pub score: f32,
    /// Effective (lazily-decayed) usage weight at search time (RFC 0007 §3).
    /// `1.0` when decay is disabled or the chunk has no `chunk_stats` row, so a
    /// fresh / never-recalled chunk is never dormant.
    pub weight: f32,
    /// Epoch-ms of the last recorded access, if any — drives the dormant age
    /// tag in `memory_search`. `None` when decay is disabled or no row exists.
    pub last_accessed_ms: Option<i64>,
}

/// Read-time usage stats for a chunk: `weight` lazily decayed to the query
/// instant (RFC 0007 §3 — dormancy is a *derived* predicate, computed not
/// stored), plus the raw `last_accessed` for age display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectiveStat {
    pub weight: f32,
    pub last_accessed_ms: i64,
}

/// Full read-time usage snapshot for one chunk, backing the M6.2 "Salience"
/// surface (#293). Unlike [`EffectiveStat`] this is emitted for *every* keyed
/// row regardless of the decay flag, and carries the raw `access_count` and
/// `pinned` flag plus a server-computed `dormant` predicate — so the frontend
/// never re-derives the threshold (RFC 0007 §7). `weight` is pin-aware: a
/// pinned chunk reads `1.0` (decay skipped), matching [`effective_stats`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkStatSnapshot {
    /// Effective (pin-aware, lazily decayed) weight at the query instant.
    pub weight: f32,
    pub last_accessed_ms: i64,
    pub access_count: u32,
    pub pinned: bool,
    /// `decay.enabled && weight < dormant_threshold && !pinned` (RFC 0007 §3).
    pub dormant: bool,
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
    /// Reinforce usage stats for the surfaced top-k `hits` (RFC 0007 §2). The
    /// default is a no-op so backends without a `chunk_stats` table (e.g. a null
    /// index) need no change; [`Fts5Index`] overrides it.
    fn reinforce(&self, _hits: &[ScoredChunk]) -> Result<()> {
        Ok(())
    }
    /// Weak reinforcement for chunks that were *ambient-injected* and the turn
    /// produced a reply (RFC 0007 §10.1). Bumps each key's `weight` toward `1.0`
    /// by the smaller `decay.ambient_gain` (vs `reinforce_gain` for a recall) and
    /// refreshes `last_accessed`, so a still-shown chunk ages more slowly. A
    /// complete no-op unless decay is enabled *and* `ambient_gain > 0`, and it
    /// never creates a row for a never-recalled chunk — so the RFC §3
    /// never-recalled-stays-`1.0` invariant is preserved. The default is a no-op
    /// so backends without a `chunk_stats` table need no change.
    fn reinforce_ambient(&self, _keys: &[String]) -> Result<()> {
        Ok(())
    }
    /// Read-time effective (decayed) usage stats for `keys`, computed against
    /// `now_ms` without persisting (RFC 0007 §3). Keys with no `chunk_stats`
    /// row — and *all* keys when decay is disabled — are omitted from the map;
    /// callers treat an absent key as weight `1.0` (never dormant). The default
    /// is empty so backends without a stats table need no change.
    fn effective_stats(
        &self,
        _keys: &[String],
        _now_ms: i64,
    ) -> Result<HashMap<String, EffectiveStat>> {
        Ok(HashMap::new())
    }
    /// Full per-chunk usage snapshot for the M6.2 Salience surface (#293):
    /// effective (pin-aware) weight, `last_accessed_ms`, `access_count`, `pinned`,
    /// and the server-computed `dormant` predicate, for every key that has a
    /// `chunk_stats` row — emitted regardless of the decay flag (unlike
    /// [`effective_stats`](Self::effective_stats), which is empty when decay is
    /// off). Keys with no row are omitted; callers treat an absent key as a
    /// never-recalled chunk (weight `1.0`, never dormant, not pinned). The
    /// default is empty so backends without a stats table need no change.
    fn chunk_stats_snapshot(
        &self,
        _keys: &[String],
        _now_ms: i64,
    ) -> Result<HashMap<String, ChunkStatSnapshot>> {
        Ok(HashMap::new())
    }
    /// Reset (wake) a chunk: restore `weight` to `1.0` and stamp `last_accessed`
    /// to now, creating the row if absent (RFC 0007 §7). The default is a no-op
    /// so backends without a stats table need no change.
    fn reset_chunk(&self, _key: &str) -> Result<()> {
        Ok(())
    }
    /// Set a chunk's `pinned` flag, creating the row if absent. A pinned chunk
    /// reads effective weight `1.0` (decay skipped) and is never dormant (#293).
    /// The default is a no-op so backends without a stats table need no change.
    fn set_chunk_pinned(&self, _key: &str, _pinned: bool) -> Result<()> {
        Ok(())
    }
}

/// FTS5/BM25 index over a SQLite database. `open` on a path persists to disk;
/// [`open_in_memory`](Self::open_in_memory) is for tests.
pub struct Fts5Index {
    conn: Mutex<Connection>,
    decay: DecayConfig,
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
             END;
             CREATE TABLE IF NOT EXISTS chunk_stats (
                 chunk_key     TEXT PRIMARY KEY,
                 weight        REAL    NOT NULL DEFAULT 1.0,
                 last_accessed INTEGER NOT NULL,
                 access_count  INTEGER NOT NULL DEFAULT 0,
                 pinned        INTEGER NOT NULL DEFAULT 0
             );",
        )?;
        Self::ensure_embedding_column(&conn)?;
        Self::ensure_pinned_column(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            decay: DecayConfig::default(),
        })
    }

    /// Attach decay configuration (RFC 0007 §5). Builder so existing call sites
    /// and tests keep the disabled-by-default behaviour.
    pub fn with_decay(mut self, decay: DecayConfig) -> Self {
        self.decay = decay;
        self
    }

    /// Back-fill the `embedding` column on indexes created before M5.3.0 (#196).
    /// The M5.1 schema (#176) had no `embedding` column, and the `CREATE TABLE IF
    /// NOT EXISTS` above is a no-op against that pre-existing table — so an old
    /// on-disk `index.db` would lack the column and every `reindex` insert would
    /// fail, silently freezing recall. The index is a derived cache, but adding
    /// the column in place is cheaper than a full rebuild and keeps the FTS data.
    /// FTS5 only indexes `text`, so the new column is invisible to `chunks_fts`.
    fn ensure_embedding_column(conn: &Connection) -> Result<()> {
        let has_embedding = conn
            .prepare("PRAGMA table_info(chunks)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<String>, _>>()?
            .iter()
            .any(|name| name == "embedding");
        if !has_embedding {
            conn.execute("ALTER TABLE chunks ADD COLUMN embedding BLOB", [])?;
        }
        Ok(())
    }

    /// Back-fill the `pinned` column on `chunk_stats` tables created before M6.2
    /// (#293). The M6.0 schema had no `pinned` column, and the `CREATE TABLE IF
    /// NOT EXISTS` above is a no-op against a pre-existing table — so an old
    /// on-disk index would lack the column and every pin write would fail. Mirror
    /// of [`ensure_embedding_column`] for the stats side table.
    fn ensure_pinned_column(conn: &Connection) -> Result<()> {
        let has_pinned = conn
            .prepare("PRAGMA table_info(chunk_stats)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<String>, _>>()?
            .iter()
            .any(|name| name == "pinned");
        if !has_pinned {
            conn.execute(
                "ALTER TABLE chunk_stats ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        Ok(())
    }

    fn insert_chunks(conn: &Connection, chunks: &[MemoryChunk]) -> Result<()> {
        let mut stmt = conn.prepare(
            "INSERT INTO chunks (source, path, heading, text, line_start, line_end, embedding)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for c in chunks {
            stmt.execute(params![
                source_to_str(&c.source),
                c.path.to_string_lossy(),
                c.heading,
                c.text,
                c.line_start,
                c.line_end,
                c.embedding.as_deref().map(vec_to_blob),
            ])?;
        }
        Ok(())
    }

    /// Stored embeddings for the given chunk ids (NULL/absent entries omitted).
    /// Used by [`HybridIndex`] to fuse vector similarity over a BM25 candidate
    /// set; with the default [`NoopEmbedder`](crate::embed::NoopEmbedder) the
    /// `embedding` column is always NULL so this returns empty.
    fn embeddings_for(&self, ids: &[i64]) -> Result<HashMap<i64, Vec<f32>>> {
        let mut out = HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT embedding FROM chunks WHERE id = ?1")?;
        for &id in ids {
            let blob: Option<Vec<u8>> = stmt.query_row(params![id], |row| row.get(0))?;
            if let Some(bytes) = blob {
                out.insert(id, blob_to_vec(&bytes));
            }
        }
        Ok(out)
    }

    /// Reinforce `hits` against `now_ms` (RFC 0007 §2). Split from the trait
    /// method so tests can inject a deterministic clock. For each hit, looks up
    /// the `chunk_stats` row by stable [`chunk_key`]:
    /// - **new key**: insert at `weight = 1.0`, `access_count = 1`.
    /// - **existing, decay enabled**: lazily decay from `last_accessed`, then
    ///   reinforce, bump `access_count`, stamp `now_ms`.
    /// - **existing, decay disabled**: record the access (count + timestamp) but
    ///   leave `weight` untouched -- behaviour is byte-identical to M5.
    pub(crate) fn reinforce_at(&self, hits: &[ScoredChunk], now_ms: i64) -> Result<()> {
        if hits.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut sel = tx.prepare(
                "SELECT weight, last_accessed, access_count
                 FROM chunk_stats WHERE chunk_key = ?1",
            )?;
            let mut up = tx.prepare(
                "INSERT INTO chunk_stats (chunk_key, weight, last_accessed, access_count)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(chunk_key) DO UPDATE SET
                     weight = ?2, last_accessed = ?3, access_count = ?4",
            )?;
            for hit in hits {
                let key = chunk_key(&hit.chunk);
                let existing: Option<(f64, i64, i64)> = sel
                    .query_row(params![key], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })
                    .optional()?;
                let (weight, count) = match existing {
                    None => (1.0_f32, 1_i64),
                    Some((w, last, c)) => {
                        let w = w as f32;
                        let new_w = if self.decay.enabled {
                            reinforced_weight(
                                decayed_weight(w, last, now_ms, self.decay.factor),
                                self.decay.reinforce_gain,
                            )
                        } else {
                            w
                        };
                        (new_w, c + 1)
                    }
                };
                up.execute(params![key, weight as f64, now_ms, count])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Time-injectable core of [`reinforce_ambient`](MemoryIndex::reinforce_ambient).
    /// Gated: no-op unless decay is enabled and `ambient_gain > 0`. Only updates
    /// keys that already have a `chunk_stats` row (a never-recalled chunk has no
    /// row and is left untouched, preserving RFC §3).
    pub(crate) fn reinforce_ambient_at(&self, keys: &[String], now_ms: i64) -> Result<()> {
        if keys.is_empty() || !self.decay.enabled || self.decay.ambient_gain <= 0.0 {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut sel = tx.prepare(
                "SELECT weight, last_accessed, access_count
                 FROM chunk_stats WHERE chunk_key = ?1",
            )?;
            let mut up = tx.prepare(
                "UPDATE chunk_stats
                 SET weight = ?2, last_accessed = ?3, access_count = ?4
                 WHERE chunk_key = ?1",
            )?;
            for key in keys {
                let existing: Option<(f64, i64, i64)> = sel
                    .query_row(params![key], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })
                    .optional()?;
                // Never-recalled chunks (no row) are intentionally skipped: ambient
                // injection does not start the age clock (RFC §3).
                if let Some((w, last, c)) = existing {
                    let new_w = reinforced_weight(
                        decayed_weight(w as f32, last, now_ms, self.decay.factor),
                        self.decay.ambient_gain,
                    );
                    up.execute(params![key, new_w as f64, now_ms, c + 1])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Time-injectable core of [`reset_chunk`](MemoryIndex::reset_chunk) (#293).
    /// Restores `weight` to `1.0` and stamps `last_accessed = now_ms`, creating
    /// the row if absent (a never-recalled chunk has no row). `access_count` and
    /// `pinned` are preserved on an existing row; a fresh row starts at count `0`,
    /// unpinned.
    pub(crate) fn reset_chunk_at(&self, key: &str, now_ms: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO chunk_stats (chunk_key, weight, last_accessed, access_count, pinned)
             VALUES (?1, 1.0, ?2, 0, 0)
             ON CONFLICT(chunk_key) DO UPDATE SET weight = 1.0, last_accessed = ?2",
            params![key, now_ms],
        )?;
        Ok(())
    }

    /// Time-injectable core of
    /// [`set_chunk_pinned`](MemoryIndex::set_chunk_pinned) (#293). Sets the
    /// `pinned` flag, creating the row if absent. A new row starts at weight
    /// `1.0` with `last_accessed = now_ms` (so pinning a never-recalled chunk
    /// does not fabricate an epoch-0 timestamp); an existing row keeps its
    /// stored weight/timestamp — the pin overrides at read time anyway.
    pub(crate) fn set_chunk_pinned_at(&self, key: &str, pinned: bool, now_ms: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO chunk_stats (chunk_key, weight, last_accessed, access_count, pinned)
             VALUES (?1, 1.0, ?2, 0, ?3)
             ON CONFLICT(chunk_key) DO UPDATE SET pinned = ?3",
            params![key, now_ms, pinned as i64],
        )?;
        Ok(())
    }
}

impl MemoryIndex for Fts5Index {
    fn reindex(&self, chunks: &[MemoryChunk]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM chunks", [])?;
        Self::insert_chunks(&tx, chunks)?;
        // Orphan sweep (RFC 0007 §4): a full rebuild knows the complete keyset, so
        // drop stats whose chunk no longer exists. Stats for surviving keys are
        // kept untouched -- chunk_key is stable across reindex, so the row re-joins
        // by key with no work. (reindex_path is partial and cannot sweep globally;
        // the next full reindex reconciles.)
        let keys: Vec<String> = chunks.iter().map(chunk_key).collect();
        if keys.is_empty() {
            tx.execute("DELETE FROM chunk_stats", [])?;
        } else {
            tx.execute("CREATE TEMP TABLE valid_keys (k TEXT PRIMARY KEY)", [])?;
            {
                let mut stmt = tx.prepare("INSERT OR IGNORE INTO valid_keys (k) VALUES (?1)")?;
                for k in &keys {
                    stmt.execute(params![k])?;
                }
            }
            tx.execute(
                "DELETE FROM chunk_stats WHERE chunk_key NOT IN (SELECT k FROM valid_keys)",
                [],
            )?;
            tx.execute("DROP TABLE valid_keys", [])?;
        }
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
        // Scope the connection lock so it is released before `effective_stats`
        // re-locks it below (the Mutex is not reentrant).
        let mut out = {
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
                    // Annotated below from chunk_stats; defaults keep a fresh chunk
                    // non-dormant.
                    weight: 1.0,
                    last_accessed_ms: None,
                })
            })?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        };

        // Annotate each hit with its read-time effective weight + last access so
        // `memory_search` can tag dormant chunks (RFC 0007 §3). No-op when decay
        // is disabled (`effective_stats` returns empty), keeping output identical
        // to M5.
        if !out.is_empty() {
            let keys: Vec<String> = out.iter().map(|s| chunk_key(&s.chunk)).collect();
            let stats = self.effective_stats(&keys, Utc::now().timestamp_millis())?;
            for (s, key) in out.iter_mut().zip(&keys) {
                if let Some(es) = stats.get(key) {
                    s.weight = es.weight;
                    s.last_accessed_ms = Some(es.last_accessed_ms);
                }
            }
        }
        Ok(out)
    }

    fn reinforce(&self, hits: &[ScoredChunk]) -> Result<()> {
        self.reinforce_at(hits, Utc::now().timestamp_millis())
    }

    fn reinforce_ambient(&self, keys: &[String]) -> Result<()> {
        self.reinforce_ambient_at(keys, Utc::now().timestamp_millis())
    }

    fn effective_stats(
        &self,
        keys: &[String],
        now_ms: i64,
    ) -> Result<HashMap<String, EffectiveStat>> {
        let mut out = HashMap::new();
        if !self.decay.enabled || keys.is_empty() {
            return Ok(out);
        }
        let conn = self.conn.lock().unwrap();
        let mut sel = conn.prepare(
            "SELECT weight, last_accessed, pinned FROM chunk_stats WHERE chunk_key = ?1",
        )?;
        for key in keys {
            let row: Option<(f64, i64, i64)> = sel
                .query_row(params![key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()?;
            if let Some((w, last, pinned)) = row {
                // A pinned chunk holds weight 1.0 (decay skipped), so the
                // ambient-injection skip path (`curated_filter`) keeps it live —
                // pinned facts never decay (RFC 0007 §7).
                let weight = if pinned != 0 {
                    1.0
                } else {
                    decayed_weight(w as f32, last, now_ms, self.decay.factor)
                };
                out.insert(
                    key.clone(),
                    EffectiveStat {
                        weight,
                        last_accessed_ms: last,
                    },
                );
            }
        }
        Ok(out)
    }

    fn chunk_stats_snapshot(
        &self,
        keys: &[String],
        now_ms: i64,
    ) -> Result<HashMap<String, ChunkStatSnapshot>> {
        let mut out = HashMap::new();
        if keys.is_empty() {
            return Ok(out);
        }
        let threshold = self.decay.dormant_threshold;
        let conn = self.conn.lock().unwrap();
        let mut sel = conn.prepare(
            "SELECT weight, last_accessed, access_count, pinned
             FROM chunk_stats WHERE chunk_key = ?1",
        )?;
        for key in keys {
            let row: Option<(f64, i64, i64, i64)> = sel
                .query_row(params![key], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .optional()?;
            if let Some((w, last, count, pinned)) = row {
                let pinned = pinned != 0;
                // Effective weight is pin-aware, and decay applies only when the
                // flag is on — mirrors `effective_stats`, but emitted regardless
                // of the decay flag so the Salience panel is not all-or-nothing.
                let weight = if pinned {
                    1.0
                } else if self.decay.enabled {
                    decayed_weight(w as f32, last, now_ms, self.decay.factor)
                } else {
                    w as f32
                };
                let dormant = self.decay.enabled && !pinned && weight < threshold;
                out.insert(
                    key.clone(),
                    ChunkStatSnapshot {
                        weight,
                        last_accessed_ms: last,
                        access_count: count.max(0) as u32,
                        pinned,
                        dormant,
                    },
                );
            }
        }
        Ok(out)
    }

    fn reset_chunk(&self, key: &str) -> Result<()> {
        self.reset_chunk_at(key, Utc::now().timestamp_millis())
    }

    fn set_chunk_pinned(&self, key: &str, pinned: bool) -> Result<()> {
        self.set_chunk_pinned_at(key, pinned, Utc::now().timestamp_millis())
    }
}

/// Lazy exponential decay (RFC 0007 §2): `weight * factor^days`, where `days` is
/// the fractional idle days since `last_ms`. Path-independent -- one lazy
/// application over N days equals N daily applications (`factor^(a+b) ==
/// factor^a * factor^b`), so no per-day cron is needed.
fn decayed_weight(weight: f32, last_ms: i64, now_ms: i64, factor: f32) -> f32 {
    let days = ((now_ms - last_ms) as f32 / ONE_DAY_MS).max(0.0);
    weight * factor.powf(days)
}

/// Hebbian reinforcement (RFC 0007 §2): bump `weight` toward 1.0, clamped.
fn reinforced_weight(weight: f32, gain: f32) -> f32 {
    (weight + gain * (1.0 - weight)).min(1.0)
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

/// Hybrid recall (RFC 0006 §6): FTS5/BM25 fused with vector similarity from an
/// [`Embedder`]. The fusion only engages when the embedder yields a query vector;
/// otherwise `search` returns the inner [`Fts5Index`] result verbatim, so with
/// the default [`NoopEmbedder`](crate::embed::NoopEmbedder) it is byte-identical
/// to a bare FTS5 index — the "never a hard failure" floor.
///
/// Fusion is Reciprocal Rank Fusion (RRF) over the BM25 candidate pool: a chunk's
/// final score sums `1/(C + rank)` from its BM25 rank and its cosine-similarity
/// rank. RRF is scale-independent, so exact ID/symbol hits (BM25's strength) and
/// semantic neighbours (vectors' strength) both surface. M5.3.1 supplies a real
/// local embedder; M5.3.0 only opens this seam.
pub struct HybridIndex<E: Embedder> {
    inner: Fts5Index,
    embedder: E,
}

/// RRF damping constant (the standard default). Larger = flatter rank weighting.
const RRF_C: f32 = 60.0;
/// How many BM25 candidates to over-fetch (relative to `k`) before fusing, so
/// vector re-ranking has room to promote a semantically-close low-BM25 hit.
const FUSION_POOL_FACTOR: usize = 4;

impl<E: Embedder> HybridIndex<E> {
    /// Wrap an [`Fts5Index`] with an [`Embedder`]. The index keeps full BM25
    /// behaviour; the embedder adds optional vector fusion.
    pub fn new(inner: Fts5Index, embedder: E) -> Self {
        Self { inner, embedder }
    }

    /// Attach a chunk embedding (when the embedder produces one) before indexing,
    /// leaving any pre-set embedding untouched. With [`NoopEmbedder`] this is a
    /// clone with every embedding left `None`.
    fn with_embeddings(&self, chunks: &[MemoryChunk]) -> Result<Vec<MemoryChunk>> {
        let mut out = Vec::with_capacity(chunks.len());
        for c in chunks {
            let mut c = c.clone();
            if c.embedding.is_none() {
                c.embedding = self.embedder.embed_chunk(&c.text)?;
            }
            out.push(c);
        }
        Ok(out)
    }
}

impl<E: Embedder> MemoryIndex for HybridIndex<E> {
    fn reindex(&self, chunks: &[MemoryChunk]) -> Result<()> {
        self.inner.reindex(&self.with_embeddings(chunks)?)
    }

    fn reindex_path(&self, path: &Path, chunks: &[MemoryChunk]) -> Result<()> {
        self.inner
            .reindex_path(path, &self.with_embeddings(chunks)?)
    }

    fn remove_path(&self, path: &Path) -> Result<()> {
        self.inner.remove_path(path)
    }

    fn reinforce(&self, hits: &[ScoredChunk]) -> Result<()> {
        self.inner.reinforce(hits)
    }

    fn reinforce_ambient(&self, keys: &[String]) -> Result<()> {
        self.inner.reinforce_ambient(keys)
    }

    fn effective_stats(
        &self,
        keys: &[String],
        now_ms: i64,
    ) -> Result<HashMap<String, EffectiveStat>> {
        self.inner.effective_stats(keys, now_ms)
    }

    fn chunk_stats_snapshot(
        &self,
        keys: &[String],
        now_ms: i64,
    ) -> Result<HashMap<String, ChunkStatSnapshot>> {
        self.inner.chunk_stats_snapshot(keys, now_ms)
    }

    fn reset_chunk(&self, key: &str) -> Result<()> {
        self.inner.reset_chunk(key)
    }

    fn set_chunk_pinned(&self, key: &str, pinned: bool) -> Result<()> {
        self.inner.set_chunk_pinned(key, pinned)
    }

    fn search(&self, query: &str, k: usize) -> Result<Vec<ScoredChunk>> {
        // No query vector (embeddings off / unavailable) -> pure BM25, identical
        // to Fts5Index. This is the fallback guarantee and the common path.
        let Some(qvec) = self.embedder.embed_query(query)? else {
            return self.inner.search(query, k);
        };

        // Over-fetch BM25, then fuse. The pool is the recall set; vectors only
        // re-rank within it (full vector recall is a later slice).
        let pool_k = k.saturating_mul(FUSION_POOL_FACTOR).max(k);
        let bm25 = self.inner.search(query, pool_k)?;
        if bm25.is_empty() {
            return Ok(bm25);
        }

        let ids: Vec<i64> = bm25.iter().map(|s| s.chunk.id).collect();
        let embs = self.inner.embeddings_for(&ids)?;

        // Vector rank: candidates with a stored embedding and a positive cosine,
        // ordered by cosine desc. Orthogonal/negative chunks earn no vector
        // credit just for being in the BM25 pool, so a true semantic neighbour
        // is not diluted into a tie with an unrelated high-BM25 hit.
        let mut by_cosine: Vec<(usize, f32)> = bm25
            .iter()
            .enumerate()
            .filter_map(|(i, s)| embs.get(&s.chunk.id).map(|e| (i, cosine(&qvec, e))))
            .filter(|(_, c)| *c > 0.0)
            .collect();
        by_cosine.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut vec_rank = vec![usize::MAX; bm25.len()];
        for (rank, (cand_idx, _)) in by_cosine.iter().enumerate() {
            vec_rank[*cand_idx] = rank;
        }

        // RRF: BM25 rank is the candidate's position (already best-first); the
        // vector term is omitted for candidates with no embedding.
        let mut fused: Vec<(usize, f32)> = (0..bm25.len())
            .map(|i| {
                let bm = 1.0 / (RRF_C + (i as f32 + 1.0));
                let vc = if vec_rank[i] == usize::MAX {
                    0.0
                } else {
                    1.0 / (RRF_C + (vec_rank[i] as f32 + 1.0))
                };
                (i, bm + vc)
            })
            .collect();
        fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(fused
            .into_iter()
            .take(k)
            .map(|(i, score)| ScoredChunk {
                chunk: bm25[i].chunk.clone(),
                score,
                // Usage stats are populated by the inner BM25 search; carry them
                // through the fusion re-rank unchanged.
                weight: bm25[i].weight,
                last_accessed_ms: bm25[i].last_accessed_ms,
            })
            .collect())
    }
}

/// Cosine similarity in `[-1, 1]`; `0.0` for mismatched-length or zero vectors.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Serialize an embedding to a little-endian `f32` BLOB.
fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Inverse of [`vec_to_blob`]; trailing partial bytes (never produced by us) are
/// ignored.
fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
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
    use crate::embed::{Embedder, NoopEmbedder};
    use std::path::Path;

    /// 3-axis [rust, python, sqlite] keyword vector — deterministic stand-in for
    /// a real embedder so fusion is unit-testable without a model.
    fn vectorize(text: &str) -> Vec<f32> {
        let l = text.to_lowercase();
        vec![
            f32::from(l.contains("rust")),
            f32::from(l.contains("python")),
            f32::from(l.contains("sqlite")),
        ]
    }

    /// Embeds chunks by keyword and returns a fixed query vector, so a test can
    /// aim the query at a chosen chunk regardless of the BM25 ordering.
    struct FakeEmbedder {
        query: Vec<f32>,
    }
    impl Embedder for FakeEmbedder {
        fn embed_query(&self, _query: &str) -> Result<Option<Vec<f32>>> {
            Ok(Some(self.query.clone()))
        }
        fn embed_chunk(&self, text: &str) -> Result<Option<Vec<f32>>> {
            Ok(Some(vectorize(text)))
        }
    }

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

    #[test]
    fn blob_round_trips_an_embedding() {
        let v = vec![0.5_f32, -1.25, 3.0, 0.0];
        assert_eq!(blob_to_vec(&vec_to_blob(&v)), v);
    }

    #[test]
    fn cosine_is_one_for_parallel_zero_for_orthogonal() {
        assert!((cosine(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
        assert_eq!(cosine(&[1.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn hybrid_with_noop_is_byte_identical_to_fts5() {
        let md = "## Prefs\nuser prefers rust over python\n\n## Tools\nuse sqlite for storage";
        let bare = Fts5Index::open_in_memory().unwrap();
        bare.reindex(&chunks(md, "MEMORY.md")).unwrap();

        let hybrid = HybridIndex::new(Fts5Index::open_in_memory().unwrap(), NoopEmbedder);
        hybrid.reindex(&chunks(md, "MEMORY.md")).unwrap();

        for q in ["rust", "sqlite", "python prefers", "storage", "   "] {
            assert_eq!(
                hybrid.search(q, 10).unwrap(),
                bare.search(q, 10).unwrap(),
                "hybrid+noop must match bare BM25 for query {q:?}"
            );
        }
    }

    #[test]
    fn fusion_promotes_the_semantic_match_over_bm25_order() {
        // Three chunks share the term "topic" so a "topic" query returns all
        // three with (near-)tied BM25 — order falls to insert/id order, putting
        // the python chunk first.
        let md = "## P\npython topic\n\n## R\nrust topic\n\n## S\nsqlite topic";

        let bm25_only = Fts5Index::open_in_memory().unwrap();
        bm25_only.reindex(&chunks(md, "MEMORY.md")).unwrap();
        let baseline = bm25_only.search("topic", 10).unwrap();
        assert!(baseline.len() >= 3);
        assert!(
            baseline[0].chunk.text.contains("python"),
            "baseline BM25 should lead with the python chunk, got {:?}",
            baseline[0].chunk.text
        );

        // Aim the query vector at the rust axis: fusion must promote the rust
        // chunk to the top even though BM25 alone ranks it lower.
        let hybrid = HybridIndex::new(
            Fts5Index::open_in_memory().unwrap(),
            FakeEmbedder {
                query: vec![1.0, 0.0, 0.0],
            },
        );
        hybrid.reindex(&chunks(md, "MEMORY.md")).unwrap();
        let fused = hybrid.search("topic", 10).unwrap();
        assert!(
            fused[0].chunk.text.contains("rust"),
            "fusion should surface the rust chunk first, got {:?}",
            fused[0].chunk.text
        );
    }

    #[test]
    fn fusion_falls_back_to_bm25_when_query_vector_is_absent() {
        // FakeEmbedder always yields a vector; NoopEmbedder yields none. With no
        // query vector the hybrid path must be identical to BM25.
        let md = "## A\nrust topic\n\n## B\npython topic";
        let hybrid = HybridIndex::new(Fts5Index::open_in_memory().unwrap(), NoopEmbedder);
        hybrid.reindex(&chunks(md, "MEMORY.md")).unwrap();
        let bare = Fts5Index::open_in_memory().unwrap();
        bare.reindex(&chunks(md, "MEMORY.md")).unwrap();
        assert_eq!(
            hybrid.search("topic", 10).unwrap(),
            bare.search("topic", 10).unwrap()
        );
    }

    #[test]
    fn open_migrates_pre_m530_schema_missing_embedding_column() {
        // Simulate an M5.1 (#176) on-disk index whose `chunks` table predates the
        // `embedding` column. `from_conn`'s `CREATE TABLE IF NOT EXISTS` is a
        // no-op against it, so the migration must back-fill the column or every
        // reindex insert fails (the bug the #196 review caught — unit tests missed
        // it because they all start from a fresh `open_in_memory`).
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE chunks (
                 id         INTEGER PRIMARY KEY,
                 source     TEXT NOT NULL,
                 path       TEXT NOT NULL,
                 heading    TEXT,
                 text       TEXT NOT NULL,
                 line_start INTEGER NOT NULL,
                 line_end   INTEGER NOT NULL
             );
             CREATE VIRTUAL TABLE chunks_fts
                 USING fts5(text, content='chunks', content_rowid='id');",
        )
        .unwrap();

        let idx = Fts5Index::from_conn(conn).unwrap();
        idx.reindex(&chunks("## Prefs\nuser prefers rust", "MEMORY.md"))
            .unwrap();
        let hits = idx.search("rust", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].chunk.text.contains("rust"));
    }

    // ----- M6.0 chunk_stats foundation (RFC 0007) --------------------------

    use crate::DecayConfig;

    /// Read a `chunk_stats` row by key: `(weight, last_accessed, access_count)`.
    fn read_stat(idx: &Fts5Index, key: &str) -> Option<(f32, i64, i64)> {
        let conn = idx.conn.lock().unwrap();
        conn.query_row(
            "SELECT weight, last_accessed, access_count FROM chunk_stats WHERE chunk_key = ?1",
            params![key],
            |r| Ok((r.get::<_, f64>(0)? as f32, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .unwrap()
    }

    fn scored(chunks: &[MemoryChunk]) -> Vec<ScoredChunk> {
        chunks
            .iter()
            .map(|c| ScoredChunk {
                chunk: c.clone(),
                score: 0.0,
                weight: 1.0,
                last_accessed_ms: None,
            })
            .collect()
    }

    fn enabled_decay() -> DecayConfig {
        DecayConfig {
            enabled: true,
            ..DecayConfig::default()
        }
    }

    fn disabled_decay() -> DecayConfig {
        DecayConfig {
            enabled: false,
            ..DecayConfig::default()
        }
    }

    fn ambient_decay(gain: f32) -> DecayConfig {
        DecayConfig {
            enabled: true,
            ambient_gain: gain,
            ..DecayConfig::default()
        }
    }

    #[test]
    fn decay_lazy_equals_repeated_daily() {
        let f = 0.98_f32;
        let day = ONE_DAY_MS as i64;
        let lazy = decayed_weight(1.0, 0, day * 10, f);
        let mut step = 1.0_f32;
        for _ in 0..10 {
            step = decayed_weight(step, 0, day, f);
        }
        assert!((lazy - step).abs() < 1e-4, "lazy {lazy} vs daily {step}");
    }

    #[test]
    fn reinforcement_clamps_at_one() {
        let mut w = 0.2_f32;
        for _ in 0..100 {
            w = reinforced_weight(w, 0.3);
            assert!(w <= 1.0 + f32::EPSILON, "weight escaped 1.0: {w}");
        }
        assert!((w - 1.0).abs() < 1e-3);
        // A fresh fully-salient chunk stays put.
        assert_eq!(reinforced_weight(1.0, 0.3), 1.0);
    }

    #[test]
    fn reinforce_records_new_chunk_at_full_weight() {
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(enabled_decay());
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        idx.reinforce(&scored(&cs)).unwrap();
        let key = chunk_key(&cs[0]);
        let (w, _, count) = read_stat(&idx, &key).expect("stat row created");
        assert_eq!(w, 1.0);
        assert_eq!(count, 1);
    }

    #[test]
    fn reinforce_decays_then_lifts_existing_weight() {
        let decay = DecayConfig {
            enabled: true,
            factor: 0.5,
            reinforce_gain: 0.3,
            ..DecayConfig::default()
        };
        let idx = Fts5Index::open_in_memory().unwrap().with_decay(decay);
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        let hits = scored(&cs);
        idx.reinforce_at(&hits, 0).unwrap();
        // Two idle days later: decay 1.0 -> 0.25, reinforce -> 0.25 + 0.3*0.75 = 0.475.
        let day = ONE_DAY_MS as i64;
        idx.reinforce_at(&hits, day * 2).unwrap();
        let key = chunk_key(&cs[0]);
        let (w, last, count) = read_stat(&idx, &key).unwrap();
        assert!((w - 0.475).abs() < 1e-4, "weight {w}");
        assert_eq!(last, day * 2);
        assert_eq!(count, 2);
    }

    #[test]
    fn disabled_records_stats_but_never_decays() {
        // Decay explicitly disabled (the M5 rollback path): access is recorded but
        // weight is frozen.
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(disabled_decay());
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        let hits = scored(&cs);
        idx.reinforce_at(&hits, 0).unwrap();
        let day = ONE_DAY_MS as i64;
        idx.reinforce_at(&hits, day * 10).unwrap();
        let key = chunk_key(&cs[0]);
        let (w, last, count) = read_stat(&idx, &key).unwrap();
        assert_eq!(w, 1.0, "weight must not decay when disabled");
        assert_eq!(last, day * 10, "last_accessed is still recorded");
        assert_eq!(count, 2, "access_count is still recorded");
    }

    #[test]
    fn stats_survive_edit_and_reindex_under_stable_key() {
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(enabled_decay());
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        idx.reinforce(&scored(&cs)).unwrap();
        let key = chunk_key(&cs[0]);
        assert_eq!(read_stat(&idx, &key).unwrap().2, 1);

        // Re-author the same fact with whitespace / line shifts: chunk_key is
        // stable (consolidate.rs), so the row must survive the orphan sweep.
        let edited = chunks("\n\n## H\nalpha body   \n\n", "MEMORY.md");
        assert_eq!(
            chunk_key(&edited[0]),
            key,
            "key must be stable across edits"
        );
        idx.reindex(&edited).unwrap();
        idx.reinforce(&scored(&edited)).unwrap();
        let (_, _, count) = read_stat(&idx, &key).expect("stat row survived reindex");
        assert_eq!(count, 2, "row persisted, so access_count accumulated");
    }

    #[test]
    fn reindex_sweeps_orphaned_stats() {
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(enabled_decay());
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        idx.reinforce(&scored(&cs)).unwrap();
        let stale_key = chunk_key(&cs[0]);
        assert!(read_stat(&idx, &stale_key).is_some());

        // Replace with genuinely new content: the old key is orphaned and swept.
        let fresh = chunks("## H\ncompletely different content", "MEMORY.md");
        idx.reindex(&fresh).unwrap();
        assert!(
            read_stat(&idx, &stale_key).is_none(),
            "orphan must be swept"
        );
    }

    #[test]
    fn empty_reindex_sweeps_all_stats() {
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(enabled_decay());
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        idx.reinforce(&scored(&cs)).unwrap();
        idx.reindex(&[]).unwrap();
        assert!(read_stat(&idx, &chunk_key(&cs[0])).is_none());
    }

    // ----- M6.1 dormancy reads (RFC 0007 §3) -------------------------------

    #[test]
    fn search_then_reinforce_lands_on_indexed_key() {
        // Guards the production invariant (PR #367 review): a chunk reconstructed
        // by `search` must hash to the same `chunk_key` as the indexed chunk, so
        // search-driven reinforcement updates the row the orphan sweep keeps. A
        // future change to search's SELECT or to chunk_key's inputs would break
        // this silently with every other test still green.
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(enabled_decay());
        let cs = chunks("## Prefs\nuser prefers rust", "MEMORY.md");
        idx.reindex(&cs).unwrap();

        let hits = idx.search("rust", 10).unwrap();
        assert_eq!(hits.len(), 1);
        idx.reinforce(&hits).unwrap();

        let indexed_key = chunk_key(&cs[0]);
        let (_, _, count) = read_stat(&idx, &indexed_key)
            .expect("reinforce(search hits) must update the indexed chunk stats row");
        assert_eq!(count, 1);
        assert_eq!(
            chunk_key(&hits[0].chunk),
            indexed_key,
            "search-reconstructed key must match the indexed key"
        );
    }

    #[test]
    fn effective_stats_decays_at_read_time_without_persisting() {
        let decay = DecayConfig {
            enabled: true,
            factor: 0.5,
            ..DecayConfig::default()
        };
        let idx = Fts5Index::open_in_memory().unwrap().with_decay(decay);
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        idx.reinforce_at(&scored(&cs), 0).unwrap();
        let key = chunk_key(&cs[0]);

        // Two idle days later (factor 0.5): effective weight = 1.0 * 0.5^2 = 0.25.
        let day = ONE_DAY_MS as i64;
        let stats = idx
            .effective_stats(std::slice::from_ref(&key), day * 2)
            .unwrap();
        let es = stats.get(&key).expect("row present");
        assert!((es.weight - 0.25).abs() < 1e-4, "weight {}", es.weight);
        assert_eq!(es.last_accessed_ms, 0);
        // Read-only: the persisted weight is untouched.
        assert_eq!(read_stat(&idx, &key).unwrap().0, 1.0);
    }

    #[test]
    fn effective_stats_omits_unknown_keys() {
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(enabled_decay());
        let stats = idx.effective_stats(&["nope".to_string()], 0).unwrap();
        assert!(
            stats.is_empty(),
            "unknown key absent -> caller treats as 1.0"
        );
    }

    #[test]
    fn effective_stats_empty_when_decay_disabled() {
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(disabled_decay());
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        idx.reinforce_at(&scored(&cs), 0).unwrap();
        let stats = idx
            .effective_stats(&[chunk_key(&cs[0])], ONE_DAY_MS as i64 * 100)
            .unwrap();
        assert!(
            stats.is_empty(),
            "disabled -> nothing dormant, identical to M5"
        );
    }

    #[test]
    fn search_annotates_decayed_weight() {
        let decay = DecayConfig {
            enabled: true,
            factor: 0.5,
            ..DecayConfig::default()
        };
        let idx = Fts5Index::open_in_memory().unwrap().with_decay(decay);
        let cs = chunks("## Prefs\nuser prefers rust", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        // Last access at the epoch: read-time decay to "now" collapses the weight.
        idx.reinforce_at(&scored(&cs), 0).unwrap();

        let hits = idx.search("rust", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].weight < 0.25, "decayed weight {}", hits[0].weight);
        assert_eq!(hits[0].last_accessed_ms, Some(0));
    }

    #[test]
    fn search_weight_stays_one_when_decay_disabled() {
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(disabled_decay());
        let cs = chunks("## Prefs\nuser prefers rust", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        idx.reinforce_at(&scored(&cs), 0).unwrap();
        let hits = idx.search("rust", 10).unwrap();
        assert_eq!(hits[0].weight, 1.0, "disabled decay -> weight neutral");
        assert_eq!(hits[0].last_accessed_ms, None);
    }
    const DAY_MS_I: i64 = ONE_DAY_MS as i64;

    #[test]
    fn reinforce_ambient_bumps_existing_row_weaker_than_recall() {
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(ambient_decay(0.1));
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        idx.reinforce_at(&scored(&cs), 0).unwrap(); // recall: weight 1.0 @ t=0

        let later = DAY_MS_I * 100;
        idx.reinforce_ambient_at(&[chunk_key(&cs[0])], later)
            .unwrap();

        let decayed = decayed_weight(1.0, 0, later, 0.98);
        let recall_bump = reinforced_weight(decayed, 0.3); // reinforce_gain
        let (w, last, count) = read_stat(&idx, &chunk_key(&cs[0])).unwrap();
        assert!(w > decayed, "ambient gain must bump above pure decay");
        assert!(
            w < recall_bump,
            "ambient bump must be weaker than a recall bump"
        );
        assert_eq!(last, later, "ambient touch refreshes last_accessed");
        assert_eq!(count, 2, "ambient touch counts as an access");
    }

    #[test]
    fn reinforce_ambient_skips_never_recalled_chunk() {
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(ambient_decay(0.1));
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        // Never recalled -> no chunk_stats row. Ambient must NOT create one (RFC §3).
        idx.reinforce_ambient_at(&[chunk_key(&cs[0])], DAY_MS_I)
            .unwrap();
        assert!(
            read_stat(&idx, &chunk_key(&cs[0])).is_none(),
            "ambient injection must not start the age clock for a never-recalled chunk"
        );
    }

    #[test]
    fn reinforce_ambient_noop_when_gain_zero() {
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(ambient_decay(0.0));
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        idx.reinforce_at(&scored(&cs), 0).unwrap();
        idx.reinforce_ambient_at(&[chunk_key(&cs[0])], DAY_MS_I * 100)
            .unwrap();
        let (w, last, count) = read_stat(&idx, &chunk_key(&cs[0])).unwrap();
        assert_eq!((w, last, count), (1.0, 0, 1), "gain 0 => complete no-op");
    }

    #[test]
    fn reinforce_ambient_noop_when_decay_disabled() {
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(disabled_decay());
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        idx.reinforce_at(&scored(&cs), 0).unwrap();
        idx.reinforce_ambient_at(&[chunk_key(&cs[0])], DAY_MS_I * 100)
            .unwrap();
        let (w, last, count) = read_stat(&idx, &chunk_key(&cs[0])).unwrap();
        assert_eq!((w, last, count), (1.0, 0, 1), "decay off => complete no-op");
    }

    // ----- M6.2 snapshot + reset + pin (RFC 0007 §7, #293) -----------------

    fn read_pinned(idx: &Fts5Index, key: &str) -> Option<bool> {
        let conn = idx.conn.lock().unwrap();
        conn.query_row(
            "SELECT pinned FROM chunk_stats WHERE chunk_key = ?1",
            params![key],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .unwrap()
        .map(|p| p != 0)
    }

    #[test]
    fn pin_holds_weight_at_one_across_a_decay_pass() {
        // factor 0.5 would collapse an unpinned chunk to ~0 after 10 idle days;
        // a pinned chunk reads 1.0 from BOTH the snapshot and effective_stats, so
        // the ambient-injection skip path keeps it live (pinned facts never decay).
        let decay = DecayConfig {
            enabled: true,
            factor: 0.5,
            ..DecayConfig::default()
        };
        let idx = Fts5Index::open_in_memory().unwrap().with_decay(decay);
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        idx.reinforce_at(&scored(&cs), 0).unwrap();
        let key = chunk_key(&cs[0]);
        idx.set_chunk_pinned_at(&key, true, 0).unwrap();

        let later = DAY_MS_I * 10;
        let snap = idx
            .chunk_stats_snapshot(std::slice::from_ref(&key), later)
            .unwrap();
        let s = snap.get(&key).expect("row present");
        assert_eq!(s.weight, 1.0, "pinned weight held at 1.0");
        assert!(!s.dormant, "pinned chunk is never dormant");
        assert!(s.pinned);

        let es = idx
            .effective_stats(std::slice::from_ref(&key), later)
            .unwrap();
        assert_eq!(
            es.get(&key).unwrap().weight,
            1.0,
            "effective_stats is pin-aware so curated_filter keeps the chunk live"
        );
    }

    #[test]
    fn unpinned_chunk_below_threshold_is_dormant_in_snapshot() {
        let decay = DecayConfig {
            enabled: true,
            factor: 0.5,
            ..DecayConfig::default()
        };
        let idx = Fts5Index::open_in_memory().unwrap().with_decay(decay);
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        idx.reinforce_at(&scored(&cs), 0).unwrap();
        let key = chunk_key(&cs[0]);

        // 3 idle days: 1.0 * 0.5^3 = 0.125 < dormant_threshold (0.25).
        let snap = idx
            .chunk_stats_snapshot(std::slice::from_ref(&key), DAY_MS_I * 3)
            .unwrap();
        let s = snap.get(&key).unwrap();
        assert!((s.weight - 0.125).abs() < 1e-4, "weight {}", s.weight);
        assert!(s.dormant, "below threshold and unpinned => dormant");
        assert_eq!(s.access_count, 1);
        assert!(!s.pinned);
    }

    #[test]
    fn snapshot_emitted_even_when_decay_disabled() {
        // Unlike effective_stats (empty when decay off), the snapshot still
        // reports stored weight/access_count/pinned so the Salience panel is not
        // all-or-nothing on the decay flag; nothing is ever dormant though.
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(disabled_decay());
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        idx.reinforce_at(&scored(&cs), 0).unwrap();
        let key = chunk_key(&cs[0]);
        let snap = idx
            .chunk_stats_snapshot(std::slice::from_ref(&key), DAY_MS_I * 100)
            .unwrap();
        let s = snap.get(&key).expect("row present despite decay off");
        assert_eq!(s.weight, 1.0, "decay off => stored weight, no decay");
        assert!(!s.dormant, "decay off => never dormant");
        assert_eq!(s.access_count, 1);
    }

    #[test]
    fn snapshot_omits_unknown_keys() {
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(enabled_decay());
        let snap = idx.chunk_stats_snapshot(&["nope".to_string()], 0).unwrap();
        assert!(
            snap.is_empty(),
            "no row => caller treats as never-recalled 1.0"
        );
    }

    #[test]
    fn reset_restores_weight_and_creates_row_if_absent() {
        let decay = DecayConfig {
            enabled: true,
            factor: 0.5,
            ..DecayConfig::default()
        };
        let idx = Fts5Index::open_in_memory().unwrap().with_decay(decay);
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        let key = chunk_key(&cs[0]);

        // Never recalled => no row. Reset must create it (wake) at weight 1.0.
        assert!(read_stat(&idx, &key).is_none());
        idx.reset_chunk_at(&key, 5_000).unwrap();
        let (w, last, count) = read_stat(&idx, &key).expect("reset created the row");
        assert_eq!((w, last, count), (1.0, 5_000, 0));

        // Decay it, then reset again: weight back to 1.0, timestamp refreshed.
        idx.reinforce_at(&scored(&cs), 0).unwrap(); // count -> 1, weight path
        idx.reset_chunk_at(&key, DAY_MS_I).unwrap();
        let (w, last, _) = read_stat(&idx, &key).unwrap();
        assert_eq!(w, 1.0, "reset restores neutral weight");
        assert_eq!(last, DAY_MS_I, "reset stamps last_accessed");
    }

    #[test]
    fn set_pinned_round_trips_and_creates_row_if_absent() {
        let idx = Fts5Index::open_in_memory()
            .unwrap()
            .with_decay(enabled_decay());
        let cs = chunks("## H\nalpha body", "MEMORY.md");
        idx.reindex(&cs).unwrap();
        let key = chunk_key(&cs[0]);

        assert!(read_stat(&idx, &key).is_none());
        idx.set_chunk_pinned_at(&key, true, 7_000).unwrap();
        assert_eq!(read_pinned(&idx, &key), Some(true), "pin creates the row");
        let (w, last, count) = read_stat(&idx, &key).unwrap();
        assert_eq!(
            (w, last, count),
            (1.0, 7_000, 0),
            "fresh pinned row defaults"
        );

        idx.set_chunk_pinned_at(&key, false, 9_000).unwrap();
        assert_eq!(read_pinned(&idx, &key), Some(false), "unpin round-trips");
    }

    #[test]
    fn ensure_pinned_column_upgrades_a_columnless_db() {
        // Simulate an M6.0 on-disk index: chunk_stats without the pinned column.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE chunk_stats (
                 chunk_key     TEXT PRIMARY KEY,
                 weight        REAL    NOT NULL DEFAULT 1.0,
                 last_accessed INTEGER NOT NULL,
                 access_count  INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO chunk_stats (chunk_key, weight, last_accessed, access_count)
                 VALUES ('old:key', 0.5, 0, 3);",
        )
        .unwrap();

        // from_conn must ALTER-in-place so pin writes succeed on the upgraded DB.
        let idx = Fts5Index::from_conn(conn).unwrap();
        assert_eq!(
            read_pinned(&idx, "old:key"),
            Some(false),
            "back-filled column defaults to 0 on the pre-existing row"
        );
        idx.set_chunk_pinned_at("old:key", true, 1_000).unwrap();
        assert_eq!(read_pinned(&idx, "old:key"), Some(true));
    }
}
