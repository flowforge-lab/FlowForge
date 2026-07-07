//! Per-session flush ledger (RFC 0006 §7.2).
//!
//! Records when a session was last flushed to durable memory so the pre-compaction
//! flush fires at most once per context-pressure cycle. This is *not* part of the
//! rebuildable recall index ([`crate::Fts5Index`]) — it is durable bookkeeping the
//! memory subsystem owns, kept in a sidecar `flush.db` so it survives a restart and
//! an `index.db` rebuild without touching the frontend IPC contract.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

use crate::Result;

/// The last recorded flush for a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushRecord {
    /// Message count at the time of the flush — the cycle marker. A new flush is due
    /// only once the transcript has grown past this.
    pub message_count: u64,
    /// Wall-clock time of the flush, Unix epoch milliseconds.
    pub flushed_at_ms: i64,
}

/// SQLite-backed durable record of the last flush per session. `open` persists to
/// disk; [`open_in_memory`](Self::open_in_memory) is for tests.
pub struct FlushLedger {
    conn: Mutex<Connection>,
}

impl FlushLedger {
    /// Open (creating if absent) the ledger at `path`, ensuring the schema.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).map_err(|source| crate::error::MemoryError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        Self::from_conn(Connection::open(path)?)
    }

    /// An ephemeral in-memory ledger (tests).
    pub fn open_in_memory() -> Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS flush_state (
                 session_id    TEXT PRIMARY KEY,
                 message_count INTEGER NOT NULL,
                 flushed_at_ms INTEGER NOT NULL
             );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// The last flush recorded for `session_id`, or `None` if it was never flushed.
    pub fn last_flush(&self, session_id: &str) -> Result<Option<FlushRecord>> {
        let conn = self.conn.lock().unwrap();
        let rec = conn
            .query_row(
                "SELECT message_count, flushed_at_ms FROM flush_state WHERE session_id = ?1",
                params![session_id],
                |row| {
                    Ok(FlushRecord {
                        message_count: row.get::<_, i64>(0)? as u64,
                        flushed_at_ms: row.get(1)?,
                    })
                },
            )
            .optional()?;
        Ok(rec)
    }

    /// Record a flush for `session_id` at the given transcript `message_count` and
    /// time. Overwrites any prior record (one row per session).
    pub fn record_flush(
        &self,
        session_id: &str,
        message_count: u64,
        flushed_at_ms: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO flush_state (session_id, message_count, flushed_at_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id) DO UPDATE SET
                 message_count = excluded.message_count,
                 flushed_at_ms = excluded.flushed_at_ms",
            params![session_id, message_count as i64, flushed_at_ms],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
