//! Session and message persistence.
//!
//! Backed by SQLite (mirroring the `FlushLedger` pattern in `ff-memory`).
//! [`SessionStore::new`] opens an in-memory database, so the ephemeral CLI and
//! every test keep working with zero behavior change; [`SessionStore::open`]
//! backs the store with a file on disk so conversations survive a restart
//! (RFC 0012). The public API is unchanged — callers see the same infallible
//! methods regardless of backend.
//!
//! (Durable user memory -- facts, daily logs, recall -- is a separate concern,
//! owned by the `ff-memory` crate per RFC 0006.)

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use ff_core::{
    auto_title, Attachment, Format, McpServerConfig, Message, Mode, ModelSelection, Role, Session,
    SessionStatus, StopReason, ToolCall,
};
use rusqlite::{params, Connection, OptionalExtension};

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Construct a fresh `Session` (unpersisted). Both the immediate-write
/// [`create_session`](SessionStore::create_session) and the deferred
/// [`create_draft_session`](SessionStore::create_draft_session) build from here.
fn new_session(goal: Option<String>) -> Session {
    let ts = now_ms();
    Session {
        id: new_id(),
        goal,
        title: None,
        summary: None,
        status: SessionStatus::Active,
        created_at: ts,
        updated_at: ts,
        phenotype: None,
        mode: None,
        workspace: None,
        model: None,
        mcp_servers: None,
        parent_session_id: None,
        fork_point_seq: None,
    }
}

/// Write a session row. The single INSERT used by both immediate creation and the
/// deferred-draft flush ([`SessionStore::flush_pending`]).
fn insert_session(conn: &Connection, session: &Session) {
    let inserted = conn.execute(
        "INSERT INTO sessions
             (id, goal, title, summary, status, created_at, updated_at, phenotype, mode, workspace, model, mcp_servers,
              parent_session_id, fork_point_seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            session.id,
            session.goal,
            session.title,
            session.summary,
            enum_to_text(&session.status),
            session.created_at,
            session.updated_at,
            session.phenotype,
            session.mode.as_ref().map(enum_to_text),
            session.workspace,
            session.model.as_ref().and_then(|m| serde_json::to_string(m).ok()),
            session
                .mcp_servers
                .as_ref()
                .and_then(|m| serde_json::to_string(m).ok()),
            session.parent_session_id,
            session.fork_point_seq,
        ],
    );
    if let Err(error) = &inserted {
        tracing::error!(%error, "session write failed");
    }
    inserted.expect("insert session");
}

/// Serialize a small `rename_all` enum (Role/SessionStatus/Mode) to its string
/// form for storage, so the on-disk text always tracks the serde representation.
fn enum_to_text<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Inverse of [`enum_to_text`]; `None` if `text` is not a known variant.
fn text_to_enum<T: serde::de::DeserializeOwned>(text: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(text.to_owned())).ok()
}

/// Escape a user query for safe use as an FTS5 MATCH expression. Wraps each
/// whitespace-separated token in double quotes so special characters (`*`, `"`,
/// `NEAR`, etc.) are treated as literals.
fn fts5_escape(query: &str) -> String {
    query
        .split_whitespace()
        .map(|tok| format!("\"{}\"", tok.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Why an [`SessionStore::edit_user_message`] call was rejected. Kept as a small
/// typed error so the command layer can map each case to a clear message; the
/// transcript is never mutated on any of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditMessageError {
    /// No message with that id exists in the given session.
    UnknownMessage,
    /// The message exists but is not a user message (only user turns are editable).
    NotUserMessage,
    /// A database error occurred while applying the edit. The transaction is never
    /// committed on this path, so the transcript is left intact.
    Storage(String),
}

impl std::fmt::Display for EditMessageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownMessage => write!(f, "no such message in this session"),
            Self::NotUserMessage => write!(f, "only user messages can be edited"),
            Self::Storage(msg) => write!(f, "storage error while editing message: {msg}"),
        }
    }
}

impl std::error::Error for EditMessageError {}

/// A full-text search hit returned by [`SessionStore::search_messages`] and
/// [`SessionStore::search_in_session`] (#679). This is an IPC return type, so
/// its TypeScript binding is emitted to the desktop `bindings/` dir; the
/// `#[serde(rename_all)]` drives the camelCase field casing (ts-rs serde-compat
/// carries it into the binding), matching the ff-core convention.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct SearchHit {
    pub session_id: String,
    pub session_title: Option<String>,
    pub message_id: String,
    pub role: String,
    pub snippet: String,
    /// Unix epoch milliseconds. `#[ts(type = "number")]` matches the `Message`
    /// / `Session` convention so the binding is `number`, not ts-rs's default
    /// `bigint` for `i64` — the documented FE contract is `createdAt: number`.
    #[ts(type = "number")]
    pub created_at: i64,
}

/// One turn's preheat attribution, as persisted by the v13 migration (#1107).
/// No TS binding: nothing on the frontend reads this yet -- it backs
/// cross-session analysis, and the live per-turn numbers already reach the UI on
/// `ContextBreakdown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnPreheat {
    pub preheated_count: usize,
    pub preheated_used: usize,
    pub preheated_bytes: usize,
}

pub struct SessionStore {
    conn: Mutex<Connection>,
    /// Sessions created but not yet persisted (#671 item 2a): a bare `＋` in the
    /// desktop app makes an in-memory draft that stays off disk (and out of
    /// `list_sessions`) until its first message — or first config write — flushes
    /// it. Keeps empty "New session" rows from accumulating on every `＋` click.
    pending: Mutex<HashMap<String, Session>>,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStore {
    /// An in-memory store. Infallible — `:memory:` cannot fail to open — so the
    /// ephemeral CLI and tests keep the same construction they always had.
    pub fn new() -> Self {
        Self::open_in_memory().expect("in-memory sqlite session store")
    }

    /// A file-backed store at `path` (created if absent), durable across restarts.
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            // Best-effort: if the dir cannot be created, the open below surfaces it.
            let _ = std::fs::create_dir_all(parent);
        }
        Self::from_conn(Connection::open(path)?)
    }

    /// An ephemeral in-memory store (tests, and the backing for [`new`](Self::new)).
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> rusqlite::Result<Self> {
        // Pin a 5s busy_timeout explicitly. WAL allows concurrent readers but
        // only one writer at a time; without a wait, a contending write returns
        // SQLITE_BUSY instantly and `insert_session`'s `.expect("insert
        // session")` would panic the losing process. Now that the CLI shares
        // the GUI's db file (#1080), this is the default path (`ff chat` with
        // the desktop app open). rusqlite 0.32 happens to ship a 5s default
        // itself, but relying on an undocumented library default for a
        // cross-process durability invariant is fragile — set it here so the
        // contract is self-documenting and version-proof. Set BEFORE any
        // operation that could contend (the `journal_mode = WAL` PRAGMA below
        // is itself a write).
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Per-connection: foreign keys are off by default in SQLite.
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )?;
        migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            pending: Mutex::new(HashMap::new()),
        })
    }

    pub fn create_session(&self, goal: Option<String>) -> Session {
        let session = new_session(goal);
        let conn = self.conn.lock().unwrap();
        insert_session(&conn, &session);
        session
    }

    /// Create a session **without persisting it** (#671 item 2a). The bare `＋` in
    /// the desktop app makes an in-memory draft that stays off disk — and out of
    /// [`list_sessions`](Self::list_sessions) — until its first message (or first
    /// config write) flushes it via [`flush_pending`](Self::flush_pending). An
    /// untouched draft leaves no row, so clicking `＋` never accrues empty "New
    /// session" rows. Restarting before the first write simply drops the draft.
    pub fn create_draft_session(&self) -> Session {
        let session = new_session(None);
        self.pending
            .lock()
            .unwrap()
            .insert(session.id.clone(), session.clone());
        session
    }

    /// Flush a deferred draft on its first write (#671 item 2a): if `session_id`
    /// is a not-yet-persisted draft, INSERT it now so the following message or
    /// config `UPDATE` lands on a real row (the FK requires the session to exist).
    /// No-op for an already-persisted session. The caller holds the `conn` lock;
    /// the lock order is always `conn` then `pending` to stay deadlock-free.
    fn flush_pending(&self, conn: &Connection, session_id: &str) {
        if let Some(session) = self.pending.lock().unwrap().remove(session_id) {
            insert_session(conn, &session);
        }
    }

    pub fn list_sessions(&self) -> Vec<Session> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, goal, title, summary, status, created_at, updated_at, phenotype, mode, workspace, model, mcp_servers,
                        parent_session_id, fork_point_seq
                 FROM sessions
                 ORDER BY updated_at DESC",
            )
            .expect("prepare list_sessions");
        let rows = stmt
            .query_map([], row_to_session)
            .expect("query list_sessions");
        rows.filter_map(Result::ok).collect()
    }

    /// Full-text search across all sessions (#679). Returns up to `limit` hits
    /// ranked by FTS5 BM25 relevance, with a snippet (highlighted with `<mark>`
    /// tags) for each match. Empty/whitespace queries return no results.
    pub fn search_messages(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        let conn = self.conn.lock().unwrap();
        // Escape FTS5 special characters by wrapping each token in double quotes.
        let safe_query = fts5_escape(query);
        let mut stmt = conn
            .prepare(
                "SELECT f.message_id, f.session_id, m.role, m.created_at, s.title,
                        CASE
                            WHEN snippet(messages_fts, 2, '<mark>', '</mark>', '...', 32) LIKE '%<mark>%'
                                THEN snippet(messages_fts, 2, '<mark>', '</mark>', '...', 32)
                            ELSE snippet(messages_fts, 3, '<mark>', '</mark>', '...', 32)
                        END AS snip
                 FROM messages_fts f
                 JOIN messages m ON m.id = f.message_id
                 JOIN sessions s ON s.id = f.session_id
                 WHERE messages_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .expect("prepare search_messages");
        let rows = stmt
            .query_map(params![safe_query, limit as i64], |row| {
                Ok(SearchHit {
                    message_id: row.get(0)?,
                    session_id: row.get(1)?,
                    role: row.get::<_, String>(2)?,
                    created_at: row.get(3)?,
                    session_title: row.get(4)?,
                    snippet: row.get(5)?,
                })
            })
            .expect("query search_messages");
        rows.filter_map(Result::ok).collect()
    }

    /// Full-text search within a single session (#679). Returns all matches
    /// in message order (by seq), for in-thread find navigation.
    pub fn search_in_session(&self, session_id: &str, query: &str) -> Vec<SearchHit> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        let conn = self.conn.lock().unwrap();
        let safe_query = fts5_escape(query);
        let mut stmt = conn
            .prepare(
                "SELECT f.message_id, f.session_id, m.role, m.created_at, s.title,
                        CASE
                            WHEN snippet(messages_fts, 2, '<mark>', '</mark>', '...', 32) LIKE '%<mark>%'
                                THEN snippet(messages_fts, 2, '<mark>', '</mark>', '...', 32)
                            ELSE snippet(messages_fts, 3, '<mark>', '</mark>', '...', 32)
                        END AS snip
                 FROM messages_fts f
                 JOIN messages m ON m.id = f.message_id
                 JOIN sessions s ON s.id = f.session_id
                 WHERE messages_fts MATCH ?1 AND f.session_id = ?2
                 ORDER BY m.seq
                 LIMIT 200",
            )
            .expect("prepare search_in_session");
        let rows = stmt
            .query_map(params![safe_query, session_id], |row| {
                Ok(SearchHit {
                    message_id: row.get(0)?,
                    session_id: row.get(1)?,
                    role: row.get::<_, String>(2)?,
                    created_at: row.get(3)?,
                    session_title: row.get(4)?,
                    snippet: row.get(5)?,
                })
            })
            .expect("query search_in_session");
        rows.filter_map(Result::ok).collect()
    }

    pub fn get_messages(&self, session_id: &str) -> Vec<Message> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, role, content, tool_calls, tool_call_id, attachments, reasoning, stop_reason, author_name, created_at
                 FROM messages
                 WHERE session_id = ?1
                 ORDER BY seq",
            )
            .expect("prepare get_messages");
        let rows = stmt
            .query_map(params![session_id], row_to_message)
            .expect("query get_messages");
        rows.filter_map(Result::ok).collect()
    }

    /// Like [`get_messages`](Self::get_messages) but returns only the most recent
    /// `limit` messages in chronological order (#1142 P1).
    pub fn get_messages_tail(&self, session_id: &str, limit: usize) -> Vec<Message> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, role, content, tool_calls, tool_call_id, attachments, reasoning, stop_reason, author_name, created_at
                 FROM messages
                 WHERE session_id = ?1
                 ORDER BY seq DESC
                 LIMIT ?2",
            )
            .expect("prepare get_messages_tail");
        let rows = stmt
            .query_map(params![session_id, limit], row_to_message)
            .expect("query get_messages_tail");
        let mut msgs: Vec<Message> = rows.filter_map(Result::ok).collect();
        msgs.reverse();
        msgs
    }

    pub fn add_message(&self, session_id: &str, role: Role, content: String) -> Message {
        self.push_message(Message {
            id: new_id(),
            session_id: session_id.to_string(),
            role,
            content,
            tool_calls: None,
            tool_call_id: None,
            attachments: None,
            reasoning: None,
            stop_reason: None,
            author_name: None,
            created_at: now_ms(),
        })
    }

    /// Append a user/assistant message that carries attachments (multimodal, #332).
    pub fn add_message_with_attachments(
        &self,
        session_id: &str,
        role: Role,
        content: String,
        attachments: Vec<Attachment>,
    ) -> Message {
        self.push_message(Message {
            id: new_id(),
            session_id: session_id.to_string(),
            role,
            content,
            tool_calls: None,
            tool_call_id: None,
            attachments: (!attachments.is_empty()).then_some(attachments),
            reasoning: None,
            stop_reason: None,
            author_name: None,
            created_at: now_ms(),
        })
    }

    /// Persist the result of a tool call, bound to its request id.
    pub fn add_tool_result_message(
        &self,
        session_id: &str,
        tool_call_id: String,
        content: String,
    ) -> Message {
        self.push_message(Message {
            id: new_id(),
            session_id: session_id.to_string(),
            role: Role::Tool,
            content,
            tool_calls: None,
            tool_call_id: Some(tool_call_id),
            attachments: None,
            reasoning: None,
            stop_reason: None,
            author_name: None,
            created_at: now_ms(),
        })
    }

    /// Persist the verbatim original of a compacted tool result, keyed by the
    /// content hash carried in its `[compacted; retrieve key=...]` marker
    /// (M7.1a, RFC 0016 Tier 1). Idempotent: an identical original (same key)
    /// is written once -- repeated tool outputs share one row.
    pub fn put_compaction_original(
        &self,
        session_id: &str,
        message_id: &str,
        key: &str,
        content: &str,
    ) {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO compaction_originals
                 (key, session_id, message_id, content, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![key, session_id, message_id, content, now_ms()],
        )
        .ok();
    }

    /// Re-assign a compacted tool result's verbatim original to a different
    /// session so it survives that session's deletion (#469). When a sub-agent's
    /// summary keeps a `[compacted; retrieve key=...]` marker, the backing
    /// original must be re-homed to the parent session *before* the ephemeral
    /// child session -- and its `ON DELETE CASCADE` -- tears the row down, or the
    /// marker dangles and `compaction_retrieve` has nothing to return. No-op when
    /// `key` is unknown.
    pub fn rehome_compaction_original(&self, key: &str, new_session_id: &str) {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE compaction_originals SET session_id = ?2, message_id = NULL
             WHERE key = ?1",
            params![key, new_session_id],
        )
        .ok();
    }

    /// Look up a compacted tool result's verbatim original by its retrieve key.
    /// Returns `None` when no original is stored for the key (e.g. it was never
    /// compacted, or its session was deleted).
    #[must_use]
    pub fn compaction_original(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT content FROM compaction_originals WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Persist one turn's preheat attribution (#1107). No-op when the turn
    /// preheated nothing: a zero row would both pad the table -- most turns
    /// declare no preheat -- and inflate the denominator of any later hit-rate
    /// read with turns that never placed a bet. Idempotent per message.
    pub fn put_turn_preheat(
        &self,
        session_id: &str,
        message_id: &str,
        preheated_count: usize,
        preheated_used: usize,
        preheated_bytes: usize,
    ) {
        if preheated_count == 0 {
            return;
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO turn_preheat
                 (message_id, session_id, preheated_count, preheated_used,
                  preheated_bytes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                message_id,
                session_id,
                preheated_count as i64,
                preheated_used as i64,
                preheated_bytes as i64,
                now_ms()
            ],
        )
        .ok();
    }

    /// Read back one turn's preheat attribution. `None` when the turn preheated
    /// nothing, predates the v13 migration, or its session was deleted.
    #[must_use]
    pub fn turn_preheat(&self, message_id: &str) -> Option<TurnPreheat> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT preheated_count, preheated_used, preheated_bytes
             FROM turn_preheat WHERE message_id = ?1",
            params![message_id],
            |row| {
                Ok(TurnPreheat {
                    preheated_count: row.get::<_, i64>(0)? as usize,
                    preheated_used: row.get::<_, i64>(1)? as usize,
                    preheated_bytes: row.get::<_, i64>(2)? as usize,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    fn push_message(&self, mut msg: Message) -> Message {
        let conn = self.conn.lock().unwrap();
        // Persist a deferred draft (#671 item 2a) before its first message: the
        // session row must exist for the message FK and the first-user-msg
        // auto-title below. No-op once the session is already on disk.
        self.flush_pending(&conn, &msg.session_id);
        // Stamp the authoring phenotype on assistant rows (#657) so a reloaded
        // thread renders the true historical author instead of the currently
        // active phenotype. The row is reserved at turn start, so the session
        // binding read here is the phenotype that produced the turn. NULL when the
        // session is unbound (default phenotype) -- the UI falls back to live
        // resolution, matching pre-existing rows.
        if msg.role == Role::Assistant && msg.author_name.is_none() {
            msg.author_name = conn
                .query_row(
                    "SELECT phenotype FROM sessions WHERE id = ?1",
                    params![msg.session_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()
                .ok()
                .flatten()
                .flatten();
        }
        let seq: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(seq), -1) + 1 FROM messages WHERE session_id = ?1",
                params![msg.session_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        // First user message in an untitled session seeds the auto-title, so a
        // title exists without a manual rename (covers background sessions too).
        let is_first_user_msg = msg.role == Role::User
            && !conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM messages WHERE session_id = ?1 AND role = 'user')",
                    params![msg.session_id],
                    |row| row.get::<_, bool>(0),
                )
                .unwrap_or(false);
        let tool_calls = msg
            .tool_calls
            .as_ref()
            .map(|c| serde_json::to_string(c).expect("serialize tool_calls"));
        // Store NULL when there are no attachments so text-only rows stay compact.
        let attachments = msg
            .attachments
            .as_ref()
            .filter(|a| !a.is_empty())
            .map(|a| serde_json::to_string(a).expect("serialize attachments"));
        let inserted = conn.execute(
            "INSERT INTO messages
                 (id, session_id, seq, role, content, tool_calls, tool_call_id, attachments, reasoning, stop_reason, author_name, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                msg.id,
                msg.session_id,
                seq,
                enum_to_text(&msg.role),
                msg.content,
                tool_calls,
                msg.tool_call_id,
                attachments,
                msg.reasoning,
                msg.stop_reason.map(|r| r.as_wire()),
                msg.author_name,
                msg.created_at,
            ],
        );
        if let Err(error) = &inserted {
            tracing::error!(%error, "session write failed");
        }
        inserted.expect("insert message");
        conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![msg.created_at, msg.session_id],
        )
        .ok();
        if is_first_user_msg {
            conn.execute(
                "UPDATE sessions SET title = ?1 WHERE id = ?2 AND title IS NULL",
                params![auto_title(&msg.content), msg.session_id],
            )
            .ok();
        }
        msg
    }

    /// Set a session's display title (the `rename_session` ipc). A manual title
    /// always wins over the auto-derived one. No-op for an unknown session.
    pub fn set_title(&self, session_id: &str, title: String) {
        let conn = self.conn.lock().unwrap();
        self.flush_pending(&conn, session_id);
        conn.execute(
            "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now_ms(), session_id],
        )
        .ok();
    }

    /// Bind this session to a phenotype by name, or clear the binding with `None`
    /// so it inherits the global active phenotype again (#246). No-op for an
    /// unknown session. The name is *not* validated here — the store is dumb
    /// persistence; the app layer validates against the phenotype registry before
    /// calling, and resolution falls back to global on an unknown name anyway.
    pub fn set_session_phenotype(&self, session_id: &str, phenotype: Option<String>) {
        let conn = self.conn.lock().unwrap();
        self.flush_pending(&conn, session_id);
        conn.execute(
            "UPDATE sessions SET phenotype = ?1, updated_at = ?2 WHERE id = ?3",
            params![phenotype, now_ms(), session_id],
        )
        .ok();
    }

    /// The session's bound phenotype name, or `None` if it inherits the global
    /// active one (or the session is unknown).
    pub fn session_phenotype(&self, session_id: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT phenotype FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
    }

    /// Bind (or clear, with `None`) this session's autonomy mode (#265). Mirrors
    /// [`set_session_phenotype`](Self::set_session_phenotype).
    pub fn set_session_mode(&self, session_id: &str, mode: Option<Mode>) {
        let conn = self.conn.lock().unwrap();
        self.flush_pending(&conn, session_id);
        conn.execute(
            "UPDATE sessions SET mode = ?1, updated_at = ?2 WHERE id = ?3",
            params![mode.as_ref().map(enum_to_text), now_ms(), session_id],
        )
        .ok();
    }

    /// The session's bound mode, or `None` if it inherits the global `defaultMode`
    /// preference (or the session is unknown).
    pub fn session_mode(&self, session_id: &str) -> Option<Mode> {
        let conn = self.conn.lock().unwrap();
        let text: Option<String> = conn
            .query_row(
                "SELECT mode FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten();
        text.as_deref().and_then(text_to_enum)
    }

    /// Set (or clear, with `None`) this session's working directory (#279). The
    /// path is stored verbatim; the app layer validates it exists before calling.
    /// No-op for an unknown session. Mirrors [`set_session_phenotype`](Self::set_session_phenotype).
    pub fn set_session_workspace(&self, session_id: &str, workspace: Option<String>) {
        let conn = self.conn.lock().unwrap();
        self.flush_pending(&conn, session_id);
        conn.execute(
            "UPDATE sessions SET workspace = ?1, updated_at = ?2 WHERE id = ?3",
            params![workspace, now_ms(), session_id],
        )
        .ok();
    }

    /// The session's persisted working directory, or `None` if it inherits the
    /// global default (or the session is unknown).
    pub fn session_workspace(&self, session_id: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT workspace FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
    }

    /// Bind (or clear, with `None`) this session's model selection (#499). The
    /// selection is stored verbatim as JSON; the app layer validates the
    /// connection id exists before calling. No-op for an unknown session. Mirrors
    /// [`set_session_phenotype`](Self::set_session_phenotype).
    pub fn set_session_model(&self, session_id: &str, model: Option<ModelSelection>) {
        let conn = self.conn.lock().unwrap();
        self.flush_pending(&conn, session_id);
        let json = model.as_ref().and_then(|m| serde_json::to_string(m).ok());
        conn.execute(
            "UPDATE sessions SET model = ?1, updated_at = ?2 WHERE id = ?3",
            params![json, now_ms(), session_id],
        )
        .ok();
    }

    /// The session's bound model selection, or `None` if it inherits the
    /// phenotype's model (or the session is unknown / the stored JSON is corrupt).
    pub fn session_model(&self, session_id: &str) -> Option<ModelSelection> {
        let conn = self.conn.lock().unwrap();
        let json: Option<String> = conn
            .query_row(
                "SELECT model FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten();
        json.and_then(|s| serde_json::from_str(&s).ok())
    }

    /// Bind (or clear, with `None`) this session's MCP server overrides (RFC 0018
    /// C3, the session tier). Stored verbatim as a JSON array; the resolver overlays
    /// them by id over the phenotype + global tiers. No-op for an unknown session.
    /// Mirrors [`set_session_model`](Self::set_session_model).
    pub fn set_session_mcp_servers(&self, session_id: &str, servers: Option<Vec<McpServerConfig>>) {
        let conn = self.conn.lock().unwrap();
        self.flush_pending(&conn, session_id);
        let json = servers.as_ref().and_then(|s| serde_json::to_string(s).ok());
        conn.execute(
            "UPDATE sessions SET mcp_servers = ?1, updated_at = ?2 WHERE id = ?3",
            params![json, now_ms(), session_id],
        )
        .ok();
    }

    /// The session's bound MCP server overrides, or `None` if it inherits the
    /// phenotype + global resolution (or the session is unknown / the stored JSON is
    /// corrupt). Mirrors [`session_model`](Self::session_model).
    pub fn session_mcp_servers(&self, session_id: &str) -> Option<Vec<McpServerConfig>> {
        let conn = self.conn.lock().unwrap();
        let json: Option<String> = conn
            .query_row(
                "SELECT mcp_servers FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten();
        json.and_then(|s| serde_json::from_str(&s).ok())
    }

    pub fn set_message_content(
        &self,
        message_id: &str,
        session_id: &str,
        content: String,
    ) -> Message {
        let ts = now_ms();
        let conn = self.conn.lock().unwrap();
        let updated = conn
            .execute(
                "UPDATE messages SET content = ?1, created_at = ?2
                 WHERE id = ?3 AND session_id = ?4",
                params![content, ts, message_id, session_id],
            )
            .unwrap_or(0);
        if updated > 0 {
            if let Some(msg) = conn
                .query_row(
                    "SELECT id, session_id, role, content, tool_calls, tool_call_id, attachments, reasoning, stop_reason, author_name, created_at
                     FROM messages WHERE id = ?1",
                    params![message_id],
                    row_to_message,
                )
                .optional()
                .ok()
                .flatten()
            {
                return msg;
            }
        }
        Message {
            id: message_id.to_string(),
            session_id: session_id.to_string(),
            role: Role::Assistant,
            content,
            tool_calls: None,
            tool_call_id: None,
            attachments: None,
            reasoning: None,
            stop_reason: None,
            author_name: None,
            created_at: ts,
        }
    }

    /// Relabel orphaned empty assistant rows with `notice`, returning how many
    /// were changed. An orphan is an assistant row with empty content, no tool
    /// calls, and no reasoning -- the row the agent loop reserves up front for
    /// token routing but never finalized because the turn was interrupted by a
    /// hard kill (SIGKILL / panic=abort), which runs no `Drop` guard (#646).
    ///
    /// Call this only when no turn is live for the session (checked by the caller),
    /// since a live turn's reserved tail row is a legitimate transient orphan.
    pub fn reconcile_orphaned_assistant_rows(&self, session_id: &str, notice: &str) -> usize {
        let ts = now_ms();
        let conn = self.conn.lock().unwrap();
        // This bulk sweep has no message id, so it can't call `set_message_stop_reason`
        // per row (that's the `Drop` guard's job). Stamp the structured reason inline
        // in the same UPDATE so the reconciled rows classify structurally on the FE,
        // matching the notice text. The wire string stays single-sourced via `as_wire`.
        conn.execute(
            "UPDATE messages SET content = ?1, created_at = ?2, stop_reason = ?3
             WHERE session_id = ?4 AND role = 'assistant'
               AND content = '' AND tool_calls IS NULL AND reasoning IS NULL",
            params![notice, ts, StopReason::Interrupted.as_wire(), session_id],
        )
        .unwrap_or(0)
    }

    /// Attach tool calls to an already-reserved assistant message (the one whose
    /// id was handed out for token routing).
    pub fn attach_tool_calls(&self, message_id: &str, session_id: &str, tool_calls: Vec<ToolCall>) {
        let json = serde_json::to_string(&tool_calls).expect("serialize tool_calls");
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE messages SET tool_calls = ?1 WHERE id = ?2 AND session_id = ?3",
            params![json, message_id, session_id],
        )
        .ok();
    }

    /// Persist the model's reasoning/CoT onto an already-reserved assistant
    /// message, so a later turn can round-trip it to reasoning-capable providers
    /// (#375). Stored verbatim; the caller skips empty reasoning so non-reasoning
    /// turns keep a NULL column. No-op for an unknown message.
    pub fn set_message_reasoning(&self, message_id: &str, session_id: &str, reasoning: &str) {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE messages SET reasoning = ?1 WHERE id = ?2 AND session_id = ?3",
            params![reasoning, message_id, session_id],
        )
        .ok();
    }

    /// Persist why a turn stopped without a usable answer (#658) onto its finalized
    /// notice message, so the frontend renders the stop structurally rather than
    /// string-matching the marker. Stored as the reason's stable wire string; NULL
    /// for a normal turn. No-op for an unknown message.
    pub fn set_message_stop_reason(&self, message_id: &str, session_id: &str, reason: StopReason) {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE messages SET stop_reason = ?1 WHERE id = ?2 AND session_id = ?3",
            params![reason.as_wire(), message_id, session_id],
        )
        .ok();
    }

    pub fn set_status(&self, session_id: &str, status: SessionStatus) {
        let conn = self.conn.lock().unwrap();
        self.flush_pending(&conn, session_id);
        conn.execute(
            "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![enum_to_text(&status), now_ms(), session_id],
        )
        .ok();
    }

    /// Permanently remove a session and its transcript. Returns whether the
    /// session existed. Idempotent: deleting an unknown id is a no-op. Messages
    /// are removed by the `ON DELETE CASCADE` foreign key.
    pub fn delete_session(&self, session_id: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        let removed = conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])
            .unwrap_or(0);
        // Also drop an un-persisted draft (#671 item 2a) so an abandoned `＋` never
        // leaks in the pending map.
        let drafted = self.pending.lock().unwrap().remove(session_id).is_some();
        removed > 0 || drafted
    }

    /// Edit a prior **user** message in place and truncate the transcript after
    /// it, in support of the ChatGPT/Claude-style "edit + re-run" flow (#464).
    /// Replaces the message's content (and attachments) at its original `seq`,
    /// then deletes every message that followed it -- the old assistant response
    /// and anything after. The caller (the `edit_message` command) re-runs the
    /// turn from the now-final edited prompt.
    ///
    /// Rejects an unknown id, an id belonging to another session, or a non-user
    /// message, so the FE can surface the reason rather than silently corrupt the
    /// transcript. The edit + truncation run in one transaction so a relaunch
    /// never observes a half-truncated history.
    pub fn edit_user_message(
        &self,
        session_id: &str,
        message_id: &str,
        content: String,
        attachments: Option<Vec<Attachment>>,
    ) -> Result<String, EditMessageError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn
            .transaction()
            .map_err(|e| EditMessageError::Storage(e.to_string()))?;

        // Look up the target message's position and role, scoped to the session.
        let row = tx
            .query_row(
                "SELECT seq, role FROM messages WHERE id = ?1 AND session_id = ?2",
                params![message_id, session_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|e| EditMessageError::Storage(e.to_string()))?;
        let (seq, role_text) = row.ok_or(EditMessageError::UnknownMessage)?;
        if text_to_enum::<Role>(&role_text) != Some(Role::User) {
            return Err(EditMessageError::NotUserMessage);
        }

        // Replace content + attachments in place (NULL when empty, matching
        // `push_message` so text-only rows stay compact).
        let attachments_json = attachments
            .as_ref()
            .filter(|a| !a.is_empty())
            .map(|a| serde_json::to_string(a).expect("serialize attachments"));
        tx.execute(
            "UPDATE messages SET content = ?1, attachments = ?2 WHERE id = ?3",
            params![content, attachments_json, message_id],
        )
        .map_err(|e| EditMessageError::Storage(e.to_string()))?;

        // Drop reversible-compaction blobs backing the messages we are about to
        // truncate. They are keyed by content hash (not a message FK), so the
        // `ON DELETE CASCADE` on `compaction_originals` only fires on a full
        // session delete -- a partial truncate would otherwise orphan them.
        tx.execute(
            "DELETE FROM compaction_originals
             WHERE session_id = ?1
               AND message_id IN (
                   SELECT id FROM messages WHERE session_id = ?1 AND seq > ?2
               )",
            params![session_id, seq],
        )
        .ok();

        // Truncate: the old response and everything after the edited message.
        tx.execute(
            "DELETE FROM messages WHERE session_id = ?1 AND seq > ?2",
            params![session_id, seq],
        )
        .map_err(|e| EditMessageError::Storage(e.to_string()))?;

        tx.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![now_ms(), session_id],
        )
        .ok();

        tx.commit()
            .map_err(|e| EditMessageError::Storage(e.to_string()))?;
        Ok(message_id.to_string())
    }

    /// Clone a session and its full transcript into a new session. The copy gets
    /// fresh ids and timestamps; messages are re-keyed to the new session id and
    /// a titled source becomes "<title> (copy)". Returns `None` for an unknown id.
    ///
    /// Records lineage (#1074): the copy points at its source via
    /// [`parent_session_id`](Session::parent_session_id), and
    /// [`fork_point_seq`](Session::fork_point_seq) marks the last parent `seq`
    /// copied. Because `seq` is preserved verbatim rather than reallocated, that
    /// point is a coordinate valid in both sessions -- which is what lets
    /// confluence bound the shared prefix instead of guessing it (RFC 0023 §4).
    pub fn fork_session(&self, session_id: &str) -> Option<Session> {
        let ts = now_ms();
        let conn = self.conn.lock().unwrap();
        let source = conn
            .query_row(
                "SELECT id, goal, title, summary, status, created_at, updated_at, phenotype, mode, workspace, model, mcp_servers,
                        parent_session_id, fork_point_seq
                 FROM sessions WHERE id = ?1",
                params![session_id],
                row_to_session,
            )
            .optional()
            .ok()
            .flatten()?;
        let mut forked = Session {
            id: new_id(),
            goal: source.goal.clone(),
            title: source.title.as_ref().map(|t| format!("{t} (copy)")),
            summary: source.summary.clone(),
            status: source.status,
            created_at: ts,
            updated_at: ts,
            // A fork keeps the parent's phenotype binding so the copy runs as the
            // same Pheno (#246).
            phenotype: source.phenotype.clone(),
            // ...and its mode binding, so the copy runs at the same autonomy (#265).
            mode: source.mode,
            // ...and its workspace, so the copy runs in the same cwd (#279).
            workspace: source.workspace.clone(),
            // ...and its model selection, so the copy runs on the same model (#499).
            model: source.model.clone(),
            // ...and its MCP server overrides (RFC 0018 session tier).
            mcp_servers: source.mcp_servers.clone(),
            // Lineage: this fork's parent is the session it was copied from, not
            // the parent's own parent -- the chain is walked, not flattened.
            parent_session_id: Some(source.id.clone()),
            // Not known until the transcript is copied below.
            fork_point_seq: None,
        };
        let tx = match conn.unchecked_transaction() {
            Ok(tx) => tx,
            Err(error) => {
                tracing::error!(%error, "session write failed");
                panic!("start fork transaction: {error}");
            }
        };
        // Shares the one INSERT with normal creation so the column list has a
        // single definition -- a second copy here silently dropped any newly
        // added column (such as lineage) from forks only.
        insert_session(&tx, &forked);

        // Re-key the transcript to the new session, preserving `seq` order.
        // The highest `seq` written is the fork point; deriving it from the rows
        // actually copied keeps it consistent by construction, with no second
        // query to race against a concurrent append.
        let mut fork_point_seq: Option<i64> = None;
        {
            let mut stmt = tx
                .prepare(
                    "SELECT seq, role, content, tool_calls, tool_call_id, attachments, reasoning, stop_reason, author_name, created_at
                     FROM messages WHERE session_id = ?1
                     ORDER BY seq",
                )
                .expect("prepare forked messages");
            let rows = stmt
                .query_map(params![session_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, i64>(9)?,
                    ))
                })
                .expect("query forked messages");

            for row in rows {
                let (
                    seq,
                    role,
                    content,
                    tool_calls,
                    tool_call_id,
                    attachments,
                    reasoning,
                    stop_reason,
                    author_name,
                    created_at,
                ) = row.expect("read forked message");
                fork_point_seq = Some(seq);
                let inserted = tx.execute(
                    "INSERT INTO messages
                         (id, session_id, seq, role, content, tool_calls, tool_call_id, attachments, reasoning, stop_reason, author_name, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        new_id(),
                        forked.id,
                        seq,
                        role,
                        content,
                        tool_calls,
                        tool_call_id,
                        attachments,
                        reasoning,
                        stop_reason,
                        author_name,
                        created_at,
                    ],
                );
                if let Err(error) = &inserted {
                    tracing::error!(%error, "session write failed");
                }
                inserted.expect("clone forked message");
            }
        }
        // A parent that was empty at fork time leaves this NULL: an empty shared
        // prefix, distinguished from "lineage root" by `parent_session_id` being set.
        if let Some(seq) = fork_point_seq {
            let updated = tx.execute(
                "UPDATE sessions SET fork_point_seq = ?1 WHERE id = ?2",
                params![seq, forked.id],
            );
            if let Err(error) = &updated {
                tracing::error!(%error, "session write failed");
            }
            updated.expect("record fork point");
            forked.fork_point_seq = Some(seq);
        }
        let committed = tx.commit();
        if let Err(error) = &committed {
            tracing::error!(%error, "session write failed");
        }
        committed.expect("commit forked session");
        Some(forked)
    }

    /// Fetch a single session by id, or `None` if it does not exist.
    pub fn get_session(&self, session_id: &str) -> Option<Session> {
        let conn = self.conn.lock().unwrap();
        let persisted = conn
            .query_row(
                "SELECT id, goal, title, summary, status, created_at, updated_at, phenotype, mode, workspace, model, mcp_servers,
                        parent_session_id, fork_point_seq
                 FROM sessions WHERE id = ?1",
                params![session_id],
                row_to_session,
            )
            .optional()
            .ok()
            .flatten();
        // An un-persisted draft (#671 item 2a) is not on disk yet; surface it from
        // the pending map so its pane and config resolve before the first message.
        persisted.or_else(|| self.pending.lock().unwrap().get(session_id).cloned())
    }

    /// Render a session and its full transcript as JSON or Markdown (RFC 0012,
    /// #278). Returns `None` for an unknown session id.
    ///
    /// JSON is a lossless `{ session, messages }` envelope that deserializes back
    /// to the same values. Markdown is a folded transcript meant to be read in a
    /// Markdown viewer.
    pub fn export_session(&self, session_id: &str, format: Format) -> Option<String> {
        let session = self.get_session(session_id)?;
        let messages = self.get_messages(session_id);
        Some(match format {
            Format::Json => export_json(&session, &messages),
            Format::Markdown => export_markdown(&session, &messages),
        })
    }
}

/// JSON export envelope. Private: the public surface is the serialized string, but
/// tests deserialize back into this to prove the round trip is lossless.
#[derive(serde::Serialize, serde::Deserialize)]
struct ExportEnvelope {
    session: Session,
    messages: Vec<Message>,
}

fn export_json(session: &Session, messages: &[Message]) -> String {
    let envelope = ExportEnvelope {
        session: session.clone(),
        messages: messages.to_vec(),
    };
    serde_json::to_string_pretty(&envelope).expect("serialize export envelope")
}

/// Tool output longer than this many characters is folded into a collapsed block
/// so a transcript stays scannable; the full text is preserved, just collapsed.
const TOOL_OUTPUT_FOLD_THRESHOLD: usize = 600;

fn export_markdown(session: &Session, messages: &[Message]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let title = session.title.as_deref().unwrap_or("Untitled session");
    let _ = writeln!(out, "# {title}\n");
    let _ = writeln!(out, "- **Created:** {}", fmt_ts(session.created_at));
    let _ = writeln!(out, "- **Updated:** {}", fmt_ts(session.updated_at));
    if let Some(goal) = &session.goal {
        let _ = writeln!(out, "- **Goal:** {goal}");
    }
    if let Some(phenotype) = &session.phenotype {
        let _ = writeln!(out, "- **Phenotype:** {phenotype}");
    }
    if let Some(mode) = session.mode {
        let _ = writeln!(out, "- **Mode:** {}", mode_label(mode));
    }
    out.push('\n');

    // Messages are stored in `seq` order, so a tool result already follows the
    // assistant message that requested it -- the tool_call_id binding is honored
    // by that ordering.
    for msg in messages {
        let heading = match msg.role {
            Role::System => continue,
            Role::User => "## You",
            Role::Assistant => "## Assistant",
            Role::Tool => "## Tool",
        };
        let _ = writeln!(out, "{heading}\n");

        // Persisted chain-of-thought (#375/#549) for an assistant turn, folded so
        // the export stays readable. Emitted before the answer it produced.
        if msg.role == Role::Assistant {
            if let Some(reasoning) = msg
                .reasoning
                .as_deref()
                .map(str::trim)
                .filter(|r| !r.is_empty())
            {
                let _ = writeln!(out, "<details><summary>Thought</summary>\n");
                let _ = writeln!(out, "{reasoning}\n");
                let _ = writeln!(out, "</details>\n");
            }
        }

        let content = msg.content.trim_end();
        if !content.is_empty() {
            if msg.role == Role::Tool && content.chars().count() > TOOL_OUTPUT_FOLD_THRESHOLD {
                let len = content.chars().count();
                let _ = writeln!(
                    out,
                    "<details><summary>Tool output ({len} chars)</summary>\n"
                );
                let _ = writeln!(out, "```\n{content}\n```\n");
                let _ = writeln!(out, "</details>\n");
            } else {
                let _ = writeln!(out, "{content}\n");
            }
        }

        if let Some(calls) = &msg.tool_calls {
            for call in calls {
                let _ = writeln!(out, "**Tool call:** {}({})\n", call.name, call.arguments);
            }
        }
    }
    out
}

fn fmt_ts(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| ms.to_string())
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Plan => "Plan",
        Mode::Act => "Act",
        Mode::Auto => "Auto",
    }
}

/// Version-gated migration runner. Bumps `PRAGMA user_version` as the schema
/// evolves; v1 is the initial sessions + messages schema (RFC 0012).
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 1 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                 id          TEXT PRIMARY KEY,
                 goal        TEXT,
                 title       TEXT,
                 summary     TEXT,
                 status      TEXT NOT NULL,
                 created_at  INTEGER NOT NULL,
                 updated_at  INTEGER NOT NULL,
                 phenotype   TEXT,
                 mode        TEXT
             );
             CREATE TABLE IF NOT EXISTS messages (
                 id           TEXT PRIMARY KEY,
                 session_id   TEXT NOT NULL,
                 seq          INTEGER NOT NULL,
                 role         TEXT NOT NULL,
                 content      TEXT NOT NULL,
                 tool_calls   TEXT,
                 tool_call_id TEXT,
                 created_at   INTEGER NOT NULL,
                 FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);",
        )?;
        conn.pragma_update(None, "user_version", 1)?;
    }
    if version < 2 {
        // P4 (#279): per-session workspace moves off `AppState`'s in-memory map into
        // the session row, so a chosen cwd survives a restart. Added via ALTER so
        // existing v1 databases gain the column without losing data.
        conn.execute_batch("ALTER TABLE sessions ADD COLUMN workspace TEXT;")?;
        conn.pragma_update(None, "user_version", 2)?;
    }
    if version < 3 {
        // BE-1 (#333): message attachments (multimodal, #332) persist as a JSON
        // array of `Attachment`, NULL when a message has none. Added via ALTER so
        // existing v2 databases gain the column without losing data.
        conn.execute_batch("ALTER TABLE messages ADD COLUMN attachments TEXT;")?;
        conn.pragma_update(None, "user_version", 3)?;
    }
    if version < 4 {
        // PR-1 (#375): persist the assistant reasoning/CoT so it can be
        // round-tripped to reasoning-capable providers on later tool-calling
        // turns. NULL for non-reasoning turns. Added via ALTER so existing v3
        // databases gain the column without losing data.
        conn.execute_batch("ALTER TABLE messages ADD COLUMN reasoning TEXT;")?;
        conn.pragma_update(None, "user_version", 4)?;
    }
    if version < 5 {
        // M7.1a (RFC 0016 Tier 1): reversible tool-result compaction. A compacted
        // tool result stores its compressed form in messages.content and the
        // verbatim original here, keyed by the content hash carried in the
        // [compacted; retrieve key=...] marker. The compaction_retrieve tool reads
        // this table on demand. ON DELETE CASCADE frees originals when a session is
        // wiped so the table cannot outlive the transcript it backs.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS compaction_originals (
                 key         TEXT PRIMARY KEY,
                 session_id  TEXT NOT NULL,
                 message_id  TEXT,
                 content     TEXT NOT NULL,
                 created_at  INTEGER NOT NULL,
                 FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_compaction_originals_session
                 ON compaction_originals(session_id);",
        )?;
        conn.pragma_update(None, "user_version", 5)?;
    }
    if version < 6 {
        // Phase D (RFC 0005 section 11, #499): per-session model selection. A
        // resolved connection+model pair stored as JSON, NULL when the session
        // inherits its phenotype's model. Added via ALTER so existing v5 databases
        // gain the column without losing data.
        conn.execute_batch("ALTER TABLE sessions ADD COLUMN model TEXT;")?;
        conn.pragma_update(None, "user_version", 6)?;
    }
    if version < 7 {
        // RFC 0018 C3 (#590): per-session MCP server overrides (the session tier). A
        // JSON array of `McpServerConfig`, NULL when the session inherits its
        // phenotype + global resolution. Added via ALTER so existing v6 databases
        // gain the column without losing data -- mirrors the v5->v6 `model` add.
        conn.execute_batch("ALTER TABLE sessions ADD COLUMN mcp_servers TEXT;")?;
        conn.pragma_update(None, "user_version", 7)?;
    }
    if version < 8 {
        // #657: persist the phenotype that authored each assistant message so a
        // reloaded thread shows the true historical author instead of relabeling
        // past turns with the currently active phenotype. NULL for user/tool/system
        // rows and for pre-existing rows (which fall back to live resolution in the
        // UI). Added via ALTER so existing v7 databases gain the column without
        // losing data -- mirrors the v3 `attachments` / v4 `reasoning` adds.
        conn.execute_batch("ALTER TABLE messages ADD COLUMN author_name TEXT;")?;
        conn.pragma_update(None, "user_version", 8)?;
    }
    if version < 9 {
        // #658: persist the structured stop reason for a turn that ended without a
        // usable answer, so the frontend classifies/renders the stop from this
        // column instead of string-matching the `[stopped…]` marker in `content`.
        // NULL for a normal turn. Added via ALTER so existing rows upgrade in place.
        conn.execute_batch("ALTER TABLE messages ADD COLUMN stop_reason TEXT;")?;
        conn.pragma_update(None, "user_version", 9)?;
    }
    if version < 10 {
        // #679: full-text search across session messages. A standalone FTS5 table
        // indexes message content for cross-session and in-thread search. Triggers
        // keep it in sync with the messages table on INSERT/UPDATE/DELETE.
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts
                 USING fts5(message_id UNINDEXED, session_id UNINDEXED, content);
             CREATE TRIGGER IF NOT EXISTS messages_fts_ai AFTER INSERT ON messages BEGIN
                 INSERT INTO messages_fts(message_id, session_id, content)
                 VALUES (new.id, new.session_id, new.content);
             END;
             CREATE TRIGGER IF NOT EXISTS messages_fts_au AFTER UPDATE OF content ON messages BEGIN
                 DELETE FROM messages_fts WHERE message_id = old.id;
                 INSERT INTO messages_fts(message_id, session_id, content)
                 VALUES (new.id, new.session_id, new.content);
             END;
             CREATE TRIGGER IF NOT EXISTS messages_fts_ad AFTER DELETE ON messages BEGIN
                 DELETE FROM messages_fts WHERE message_id = old.id;
             END;",
        )?;
        // Backfill existing messages into the FTS index.
        conn.execute_batch(
            "INSERT INTO messages_fts(message_id, session_id, content)
             SELECT id, session_id, content FROM messages;",
        )?;
        conn.pragma_update(None, "user_version", 10)?;
    }
    if version < 11 {
        // #679: also index tool-call *arguments*, not just message text and tool
        // result bodies. The v10 index covered only `content`, so searching for the
        // command/path/query of a tool call (e.g. "git rebase", a fetched URL) found
        // nothing. FTS5 columns can't be added in place, so recreate the table and
        // triggers with a `tool_calls` column and re-backfill. FTS5 MATCH spans all
        // indexed columns, so a hit in either `content` or `tool_calls` is returned;
        // the search snippet prefers whichever column actually matched.
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS messages_fts_ai;
             DROP TRIGGER IF EXISTS messages_fts_au;
             DROP TRIGGER IF EXISTS messages_fts_ad;
             DROP TABLE IF EXISTS messages_fts;
             CREATE VIRTUAL TABLE messages_fts
                 USING fts5(message_id UNINDEXED, session_id UNINDEXED, content, tool_calls);
             CREATE TRIGGER messages_fts_ai AFTER INSERT ON messages BEGIN
                 INSERT INTO messages_fts(message_id, session_id, content, tool_calls)
                 VALUES (new.id, new.session_id, new.content, COALESCE(new.tool_calls, ''));
             END;
             CREATE TRIGGER messages_fts_au AFTER UPDATE OF content, tool_calls ON messages BEGIN
                 DELETE FROM messages_fts WHERE message_id = old.id;
                 INSERT INTO messages_fts(message_id, session_id, content, tool_calls)
                 VALUES (new.id, new.session_id, new.content, COALESCE(new.tool_calls, ''));
             END;
             CREATE TRIGGER messages_fts_ad AFTER DELETE ON messages BEGIN
                 DELETE FROM messages_fts WHERE message_id = old.id;
             END;",
        )?;
        conn.execute_batch(
            "INSERT INTO messages_fts(message_id, session_id, content, tool_calls)
             SELECT id, session_id, content, COALESCE(tool_calls, '') FROM messages;",
        )?;
        conn.pragma_update(None, "user_version", 11)?;
    }
    if version < 12 {
        // #1074 (RFC 0023 §4): persist fork lineage so confluence can locate the
        // shared prefix precisely instead of guessing it from a content hash.
        // Both columns NULL means "lineage root" -- which is every pre-existing
        // session, since forked history cannot be back-filled with lineage.
        //
        // `ON DELETE SET NULL` (not CASCADE) is load-bearing: deleting a parent
        // must orphan the fork, never delete it -- the fork owns a full copy of
        // the transcript and is a session in its own right.
        //
        // The index serves two readers: the FK's own reverse lookup on every
        // session delete (without it, `ON DELETE SET NULL` forces a full scan of
        // `sessions`), and the "list a session's forks" query that merge-sessions
        // needs. Do not drop it as unused.
        conn.execute_batch(
            "ALTER TABLE sessions ADD COLUMN parent_session_id TEXT NULL
                 REFERENCES sessions(id) ON DELETE SET NULL;
             ALTER TABLE sessions ADD COLUMN fork_point_seq INTEGER NULL;
             CREATE INDEX IF NOT EXISTS idx_sessions_parent
                 ON sessions(parent_session_id);",
        )?;
        conn.pragma_update(None, "user_version", 12)?;
    }
    if version < 13 {
        // #1107 (RFC 0024 Phase 3 follow-up): persist preheat attribution. The
        // three counters ship on `ContextBreakdown`, but that rides a `turn:done`
        // event the UI overwrites next turn -- an oscilloscope, not a flight
        // recorder -- so nothing could answer whether the 2500 B preheat budget
        // was earning its keep across sessions.
        //
        // A table rather than columns on `messages`: this is a fact about a turn,
        // not message content, and most turns declare no preheat, so ALTERing
        // `messages` would add three columns that are NULL on nearly every row.
        // Keyed by `message_id` (the id `AgentEvent::Done` carries) following
        // `compaction_originals`, since there is no `turn_id` concept.
        //
        // ON DELETE CASCADE via session_id so attribution cannot outlive the
        // transcript it describes. The index serves the per-session hit-rate
        // rollup; without it that read degrades to a full scan as the table grows.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS turn_preheat (
                 message_id      TEXT PRIMARY KEY,
                 session_id      TEXT NOT NULL,
                 preheated_count INTEGER NOT NULL,
                 preheated_used  INTEGER NOT NULL,
                 preheated_bytes INTEGER NOT NULL,
                 created_at      INTEGER NOT NULL,
                 FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_turn_preheat_session
                 ON turn_preheat(session_id);",
        )?;
        conn.pragma_update(None, "user_version", 13)?;
    }
    Ok(())
}

fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<Session> {
    let status: String = row.get("status")?;
    let mode: Option<String> = row.get("mode")?;
    let model: Option<String> = row.get("model")?;
    let mcp_servers: Option<String> = row.get("mcp_servers")?;
    Ok(Session {
        id: row.get("id")?,
        goal: row.get("goal")?,
        title: row.get("title")?,
        summary: row.get("summary")?,
        status: text_to_enum(&status).unwrap_or(SessionStatus::Active),
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        phenotype: row.get("phenotype")?,
        mode: mode.as_deref().and_then(text_to_enum),
        workspace: row.get("workspace")?,
        model: model.and_then(|s| serde_json::from_str(&s).ok()),
        mcp_servers: mcp_servers.and_then(|s| serde_json::from_str(&s).ok()),
        parent_session_id: row.get("parent_session_id")?,
        fork_point_seq: row.get("fork_point_seq")?,
    })
}

fn row_to_message(row: &rusqlite::Row) -> rusqlite::Result<Message> {
    let role: String = row.get("role")?;
    let tool_calls: Option<String> = row.get("tool_calls")?;
    let attachments: Option<String> = row.get("attachments")?;
    Ok(Message {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        role: text_to_enum(&role).unwrap_or(Role::User),
        content: row.get("content")?,
        tool_calls: tool_calls.and_then(|s| serde_json::from_str(&s).ok()),
        tool_call_id: row.get("tool_call_id")?,
        attachments: attachments.and_then(|s| serde_json::from_str(&s).ok()),
        reasoning: row.get("reasoning")?,
        stop_reason: row
            .get::<_, Option<String>>("stop_reason")?
            .and_then(|s| StopReason::from_wire(&s)),
        author_name: row.get("author_name")?,
        created_at: row.get("created_at")?,
    })
}

#[cfg(test)]
mod tests;
