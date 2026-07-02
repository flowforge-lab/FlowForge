//! Session and message persistence.
//!
//! Backed by SQLite (mirroring the `FlushLedger` pattern in `ff-memory`).
//! [`SessionStore::new`] opens an in-memory database, so the ephemeral CLI and
//! every test keep working with zero behaviour change; [`SessionStore::open`]
//! backs the store with a file on disk so conversations survive a restart
//! (RFC 0012). The public API is unchanged — callers see the same infallible
//! methods regardless of backend.
//!
//! (Durable user memory -- facts, daily logs, recall -- is a separate concern,
//! owned by the `ff-memory` crate per RFC 0006.)

use std::path::Path;
use std::sync::Mutex;

use ff_core::{
    auto_title, Attachment, Format, McpServerConfig, Message, Mode, ModelSelection, Role, Session,
    SessionStatus, ToolCall,
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

pub struct SessionStore {
    conn: Mutex<Connection>,
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
        // Per-connection: foreign keys are off by default in SQLite.
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )?;
        migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn create_session(&self, goal: Option<String>) -> Session {
        let ts = now_ms();
        let session = Session {
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
        };
        let conn = self.conn.lock().unwrap();
        let inserted = conn.execute(
            "INSERT INTO sessions
                 (id, goal, title, summary, status, created_at, updated_at, phenotype, mode, workspace, model, mcp_servers)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
            ],
        );
        if let Err(error) = &inserted {
            tracing::error!(%error, "session write failed");
        }
        inserted.expect("insert session");
        session
    }

    pub fn list_sessions(&self) -> Vec<Session> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, goal, title, summary, status, created_at, updated_at, phenotype, mode, workspace, model, mcp_servers
                 FROM sessions
                 ORDER BY updated_at DESC",
            )
            .expect("prepare list_sessions");
        let rows = stmt
            .query_map([], row_to_session)
            .expect("query list_sessions");
        rows.filter_map(Result::ok).collect()
    }

    pub fn get_messages(&self, session_id: &str) -> Vec<Message> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, role, content, tool_calls, tool_call_id, attachments, reasoning, created_at
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

    fn push_message(&self, msg: Message) -> Message {
        let conn = self.conn.lock().unwrap();
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
                 (id, session_id, seq, role, content, tool_calls, tool_call_id, attachments, reasoning, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
                    "SELECT id, session_id, role, content, tool_calls, tool_call_id, attachments, reasoning, created_at
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
        conn.execute(
            "UPDATE messages SET content = ?1, created_at = ?2
             WHERE session_id = ?3 AND role = 'assistant'
               AND content = '' AND tool_calls IS NULL AND reasoning IS NULL",
            params![notice, ts, session_id],
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

    pub fn set_status(&self, session_id: &str, status: SessionStatus) {
        let conn = self.conn.lock().unwrap();
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
        removed > 0
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
    pub fn fork_session(&self, session_id: &str) -> Option<Session> {
        let ts = now_ms();
        let conn = self.conn.lock().unwrap();
        let source = conn
            .query_row(
                "SELECT id, goal, title, summary, status, created_at, updated_at, phenotype, mode, workspace, model, mcp_servers
                 FROM sessions WHERE id = ?1",
                params![session_id],
                row_to_session,
            )
            .optional()
            .ok()
            .flatten()?;
        let forked = Session {
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
        };
        let tx = match conn.unchecked_transaction() {
            Ok(tx) => tx,
            Err(error) => {
                tracing::error!(%error, "session write failed");
                panic!("start fork transaction: {error}");
            }
        };
        let inserted = tx.execute(
            "INSERT INTO sessions
                 (id, goal, title, summary, status, created_at, updated_at, phenotype, mode, workspace, model, mcp_servers)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                forked.id,
                forked.goal,
                forked.title,
                forked.summary,
                enum_to_text(&forked.status),
                forked.created_at,
                forked.updated_at,
                forked.phenotype,
                forked.mode.as_ref().map(enum_to_text),
                forked.workspace,
                forked.model.as_ref().and_then(|m| serde_json::to_string(m).ok()),
                forked
                    .mcp_servers
                    .as_ref()
                    .and_then(|m| serde_json::to_string(m).ok()),
            ],
        );
        if let Err(error) = &inserted {
            tracing::error!(%error, "session write failed");
        }
        inserted.expect("insert forked session");

        // Re-key the transcript to the new session, preserving `seq` order.
        {
            let mut stmt = tx
                .prepare(
                    "SELECT seq, role, content, tool_calls, tool_call_id, attachments, reasoning, created_at
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
                        row.get::<_, i64>(7)?,
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
                    created_at,
                ) = row.expect("read forked message");
                let inserted = tx.execute(
                    "INSERT INTO messages
                         (id, session_id, seq, role, content, tool_calls, tool_call_id, attachments, reasoning, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
                        created_at,
                    ],
                );
                if let Err(error) = &inserted {
                    tracing::error!(%error, "session write failed");
                }
                inserted.expect("clone forked message");
            }
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
        conn.query_row(
            "SELECT id, goal, title, summary, status, created_at, updated_at, phenotype, mode, workspace, model, mcp_servers
             FROM sessions WHERE id = ?1",
            params![session_id],
            row_to_session,
        )
        .optional()
        .ok()
        .flatten()
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
        created_at: row.get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_user_message_auto_titles_untitled_session() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        assert!(s.title.is_none());

        store.add_message(&s.id, Role::User, "fix the parser bug".into());
        let titled = store.list_sessions().into_iter().next().unwrap();
        assert_eq!(titled.title.as_deref(), Some("Fix the"));
    }

    #[test]
    fn second_user_message_does_not_retitle() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "fix the parser bug".into());
        store.add_message(&s.id, Role::User, "now ship it".into());
        let titled = store.list_sessions().into_iter().next().unwrap();
        assert_eq!(titled.title.as_deref(), Some("Fix the"));
    }

    #[test]
    fn assistant_first_message_does_not_title() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::Assistant, "hi there".into());
        let after = store.list_sessions().into_iter().next().unwrap();
        assert!(after.title.is_none());
    }

    #[test]
    fn set_title_overrides_and_persists() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "fix the parser bug".into());
        store.set_title(&s.id, "Custom name".into());
        let titled = store.list_sessions().into_iter().next().unwrap();
        assert_eq!(titled.title.as_deref(), Some("Custom name"));
    }

    #[test]
    fn set_title_unknown_session_is_noop() {
        let store = SessionStore::new();
        store.set_title("nope", "x".into());
        assert!(store.list_sessions().is_empty());
    }

    #[test]
    fn delete_session_removes_session_and_messages() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "hello".into());
        assert_eq!(store.list_sessions().len(), 1);

        assert!(store.delete_session(&s.id));
        assert!(store.list_sessions().is_empty());
        assert!(store.get_messages(&s.id).is_empty());
    }

    #[test]
    fn delete_session_unknown_is_noop() {
        let store = SessionStore::new();
        store.create_session(None);
        assert!(!store.delete_session("nope"));
        assert_eq!(store.list_sessions().len(), 1);
    }

    #[test]
    fn session_and_message_roundtrip() {
        let store = SessionStore::new();
        let s = store.create_session(Some("fix bug".into()));
        assert_eq!(store.list_sessions().len(), 1);

        store.add_message(&s.id, Role::User, "hello".into());
        store.add_message(&s.id, Role::Assistant, "hi".into());

        let msgs = store.get_messages(&s.id);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].content, "hi");
    }

    #[test]
    fn message_attachments_round_trip() {
        use ff_core::{AttachmentKind, AttachmentSource};
        let store = SessionStore::new();
        let s = store.create_session(None);
        let attachments = vec![
            Attachment {
                kind: AttachmentKind::Image,
                media_type: "image/png".into(),
                source: AttachmentSource::Path("/tmp/shot.png".into()),
                name: Some("shot.png".into()),
                bytes: 2048,
            },
            Attachment {
                kind: AttachmentKind::Document,
                media_type: "application/pdf".into(),
                source: AttachmentSource::Inline("JVBERi0=".into()),
                name: None,
                bytes: 8,
            },
        ];
        store.add_message_with_attachments(
            &s.id,
            Role::User,
            "look at these".into(),
            attachments.clone(),
        );

        let msgs = store.get_messages(&s.id);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "look at these");
        assert_eq!(msgs[0].attachments.as_deref(), Some(attachments.as_slice()));
    }

    #[test]
    fn plain_message_has_no_attachments() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "hi".into());
        assert!(store.get_messages(&s.id)[0].attachments.is_none());
    }

    #[test]
    fn empty_attachments_persist_as_none() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message_with_attachments(&s.id, Role::User, "hi".into(), vec![]);
        assert!(store.get_messages(&s.id)[0].attachments.is_none());
    }

    #[test]
    fn fork_session_copies_attachments() {
        use ff_core::{AttachmentKind, AttachmentSource};
        let store = SessionStore::new();
        let s = store.create_session(None);
        let att = Attachment {
            kind: AttachmentKind::Image,
            media_type: "image/jpeg".into(),
            source: AttachmentSource::Path("/tmp/a.jpg".into()),
            name: None,
            bytes: 10,
        };
        store.add_message_with_attachments(&s.id, Role::User, "see".into(), vec![att.clone()]);

        let forked = store.fork_session(&s.id).unwrap();
        let msgs = store.get_messages(&forked.id);
        assert_eq!(msgs[0].attachments.as_deref(), Some([att].as_slice()));
    }

    #[test]
    fn reconcile_relabels_orphaned_empty_assistant_row() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "hi".into());
        // The row the agent loop reserves and never finalizes.
        store.add_message(&s.id, Role::Assistant, String::new());

        let changed = store.reconcile_orphaned_assistant_rows(&s.id, "[stopped: interrupted]");
        assert_eq!(changed, 1);
        let msgs = store.get_messages(&s.id);
        assert_eq!(msgs[1].content, "[stopped: interrupted]");
    }

    #[test]
    fn reconcile_leaves_non_orphan_rows_untouched() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        // A normal answer.
        store.add_message(&s.id, Role::Assistant, "real answer".into());
        // A tool-call-only row (empty content but legitimately carries tool calls).
        let tc = store.add_message(&s.id, Role::Assistant, String::new());
        store.attach_tool_calls(
            &tc.id,
            &s.id,
            vec![ToolCall {
                id: "call_1".into(),
                name: "view".into(),
                arguments: "{}".into(),
            }],
        );
        // A reasoning-only reserved row (mid-stream: CoT arrived before content).
        let r = store.add_message(&s.id, Role::Assistant, String::new());
        store.set_message_reasoning(&r.id, &s.id, "thinking");
        // An empty user row must never be relabeled.
        store.add_message(&s.id, Role::User, String::new());

        let changed = store.reconcile_orphaned_assistant_rows(&s.id, "[stopped: interrupted]");
        assert_eq!(changed, 0, "no genuine orphan present");
        let msgs = store.get_messages(&s.id);
        assert_eq!(msgs[0].content, "real answer");
        assert_eq!(msgs[1].content, "");
        assert!(msgs[1].tool_calls.is_some());
        assert_eq!(msgs[2].content, "");
        assert_eq!(msgs[2].reasoning.as_deref(), Some("thinking"));
        assert_eq!(msgs[3].content, "");
    }

    #[test]
    fn set_message_reasoning_round_trips() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        let m = store.add_message(&s.id, Role::Assistant, String::new());
        store.set_message_reasoning(&m.id, &s.id, "step 1: multiply 17 by 23");

        let msgs = store.get_messages(&s.id);
        assert_eq!(
            msgs[0].reasoning.as_deref(),
            Some("step 1: multiply 17 by 23")
        );
    }

    #[test]
    fn set_message_reasoning_is_returned_by_set_message_content() {
        // run_turn persists reasoning, then finalizes content; the finalized
        // Message must carry both (#375 PR-1).
        let store = SessionStore::new();
        let s = store.create_session(None);
        let m = store.add_message(&s.id, Role::Assistant, String::new());
        store.set_message_reasoning(&m.id, &s.id, "thinking...");
        let finalized = store.set_message_content(&m.id, &s.id, "answer".into());
        assert_eq!(finalized.content, "answer");
        assert_eq!(finalized.reasoning.as_deref(), Some("thinking..."));
    }

    #[test]
    fn plain_message_has_no_reasoning() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::Assistant, "hi".into());
        assert!(store.get_messages(&s.id)[0].reasoning.is_none());
    }

    #[test]
    fn set_message_reasoning_unknown_message_is_noop() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.set_message_reasoning("nope", &s.id, "orphan");
        assert!(store.get_messages(&s.id).is_empty());
    }

    #[test]
    fn reasoning_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let sid;
        let mid;
        {
            let store = SessionStore::open(&path).unwrap();
            let s = store.create_session(None);
            sid = s.id.clone();
            let m = store.add_message(&s.id, Role::Assistant, "answer".into());
            mid = m.id.clone();
            store.set_message_reasoning(&mid, &sid, "chain of thought");
        }
        let store = SessionStore::open(&path).unwrap();
        let msgs = store.get_messages(&sid);
        assert_eq!(msgs[0].id, mid);
        assert_eq!(msgs[0].reasoning.as_deref(), Some("chain of thought"));
    }

    #[test]
    fn fork_session_copies_reasoning() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        let m = store.add_message(&s.id, Role::Assistant, "a".into());
        store.set_message_reasoning(&m.id, &s.id, "because");

        let forked = store.fork_session(&s.id).unwrap();
        let msgs = store.get_messages(&forked.id);
        assert_eq!(msgs[0].reasoning.as_deref(), Some("because"));
    }

    #[test]
    fn migration_v3_to_v4_preserves_messages_and_adds_reasoning() {
        // A v3 database (pre-reasoning) must gain the column on open without
        // losing existing rows, and old rows read back with reasoning = None.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let sid = "sess-1";
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                     id TEXT PRIMARY KEY, goal TEXT, title TEXT, summary TEXT,
                     status TEXT NOT NULL, created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL, phenotype TEXT, mode TEXT, workspace TEXT
                 );
                 CREATE TABLE messages (
                     id TEXT PRIMARY KEY, session_id TEXT NOT NULL, seq INTEGER NOT NULL,
                     role TEXT NOT NULL, content TEXT NOT NULL, tool_calls TEXT,
                     tool_call_id TEXT, attachments TEXT, created_at INTEGER NOT NULL
                 );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (id, status, created_at, updated_at)
                 VALUES (?1, 'active', 0, 0)",
                params![sid],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO messages (id, session_id, seq, role, content, created_at)
                 VALUES ('m1', ?1, 0, 'assistant', 'legacy', 0)",
                params![sid],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 3).unwrap();
        }

        let store = SessionStore::open(&path).unwrap();
        let msgs = store.get_messages(sid);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "legacy");
        assert!(msgs[0].reasoning.is_none());

        let version: i64 = store
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 7);
    }

    #[test]
    fn put_and_get_compaction_original_round_trip() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        let m = store.add_tool_result_message(&s.id, "call-1".into(), "compressed".into());
        store.put_compaction_original(&s.id, &m.id, "abc123", "the full original blob");
        assert_eq!(
            store.compaction_original("abc123").as_deref(),
            Some("the full original blob")
        );
        assert!(store.compaction_original("missing").is_none());
    }

    #[test]
    fn put_compaction_original_is_idempotent_on_key() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.put_compaction_original(&s.id, "m1", "k", "first");
        store.put_compaction_original(&s.id, "m2", "k", "second");
        assert_eq!(store.compaction_original("k").as_deref(), Some("first"));
    }

    #[test]
    fn compaction_originals_cascade_on_session_delete() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.put_compaction_original(&s.id, "m1", "k", "blob");
        assert!(store.compaction_original("k").is_some());
        assert!(store.delete_session(&s.id));
        assert!(
            store.compaction_original("k").is_none(),
            "deleting a session must cascade-drop its compaction originals"
        );
    }

    #[test]
    fn rehome_compaction_original_survives_child_session_delete() {
        // #469: a sub-agent stashes originals under its ephemeral child session.
        // Re-homing a marker's original to the parent before the child is deleted
        // must keep it retrievable; a sibling original left behind must still cascade.
        let store = SessionStore::new();
        let parent = store.create_session(None);
        let child = store.create_session(None);
        store.put_compaction_original(&child.id, "m1", "kept", "the kept original");
        store.put_compaction_original(&child.id, "m2", "dropped", "the dropped original");

        store.rehome_compaction_original("kept", &parent.id);
        assert!(store.delete_session(&child.id));

        assert_eq!(
            store.compaction_original("kept").as_deref(),
            Some("the kept original"),
            "a re-homed original must survive the child session teardown"
        );
        assert!(
            store.compaction_original("dropped").is_none(),
            "an original left on the child session must still cascade away"
        );
    }

    #[test]
    fn rehome_compaction_original_unknown_key_is_noop() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.rehome_compaction_original("nope", &s.id);
        assert!(store.compaction_original("nope").is_none());
    }

    #[test]
    fn migration_v4_to_v5_preserves_messages_and_creates_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let sid = "sess-1";
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                     id TEXT PRIMARY KEY, goal TEXT, title TEXT, summary TEXT,
                     status TEXT NOT NULL, created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL, phenotype TEXT, mode TEXT, workspace TEXT
                 );
                 CREATE TABLE messages (
                     id TEXT PRIMARY KEY, session_id TEXT NOT NULL, seq INTEGER NOT NULL,
                     role TEXT NOT NULL, content TEXT NOT NULL, tool_calls TEXT,
                     tool_call_id TEXT, attachments TEXT, reasoning TEXT,
                     created_at INTEGER NOT NULL
                 );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (id, status, created_at, updated_at)
                 VALUES (?1, 'active', 0, 0)",
                params![sid],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO messages (id, session_id, seq, role, content, created_at)
                 VALUES ('m1', ?1, 0, 'assistant', 'legacy', 0)",
                params![sid],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 4).unwrap();
        }

        let store = SessionStore::open(&path).unwrap();
        let msgs = store.get_messages(sid);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "legacy");

        store.put_compaction_original(sid, "m1", "k", "blob");
        assert_eq!(store.compaction_original("k").as_deref(), Some("blob"));

        let version: i64 = store
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 7);
    }

    #[test]
    fn migration_v5_to_v6_preserves_session_and_adds_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let sid = "sess-1";
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                     id TEXT PRIMARY KEY, goal TEXT, title TEXT, summary TEXT,
                     status TEXT NOT NULL, created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL, phenotype TEXT, mode TEXT, workspace TEXT
                 );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (id, goal, status, created_at, updated_at)
                 VALUES (?1, 'legacy goal', 'active', 0, 0)",
                params![sid],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 5).unwrap();
        }

        let store = SessionStore::open(&path).unwrap();
        // The pre-existing row survives the ALTER with a NULL (inherited) model.
        let s = store.get_session(sid).unwrap();
        assert_eq!(s.goal.as_deref(), Some("legacy goal"));
        assert!(s.model.is_none());
        // ...and the new column is writable post-upgrade.
        store.set_session_model(sid, Some(model_sel("openai-main", "gpt-4o")));
        assert_eq!(
            store.session_model(sid),
            Some(model_sel("openai-main", "gpt-4o"))
        );

        let version: i64 = store
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 7);
    }

    #[test]
    fn fork_session_clones_messages_with_fresh_ids() {
        let store = SessionStore::new();
        let s = store.create_session(Some("fix bug".into()));
        store.add_message(&s.id, Role::User, "hello".into());
        store.add_message(&s.id, Role::Assistant, "hi".into());

        let forked = store.fork_session(&s.id).unwrap();
        assert_ne!(forked.id, s.id);

        let src_msgs = store.get_messages(&s.id);
        let fork_msgs = store.get_messages(&forked.id);
        assert_eq!(fork_msgs.len(), src_msgs.len());
        for (orig, copy) in src_msgs.iter().zip(&fork_msgs) {
            assert_ne!(copy.id, orig.id);
            assert_eq!(copy.session_id, forked.id);
            assert_eq!(copy.role, orig.role);
            assert_eq!(copy.content, orig.content);
        }
    }

    #[test]
    fn fork_session_titled_gets_copy_suffix() {
        let store = SessionStore::new();
        let titled = store.create_session(None);
        store.set_title(&titled.id, "Fix bug".into());
        let forked = store.fork_session(&titled.id).unwrap();
        assert_eq!(forked.title.as_deref(), Some("Fix bug (copy)"));

        let untitled = store.create_session(None);
        let forked_untitled = store.fork_session(&untitled.id).unwrap();
        assert!(forked_untitled.title.is_none());
    }

    #[test]
    fn fork_session_unknown_returns_none() {
        let store = SessionStore::new();
        assert!(store.fork_session("nope").is_none());
    }

    #[test]
    fn fork_session_leaves_source_untouched() {
        let store = SessionStore::new();
        let s = store.create_session(Some("keep me".into()));
        store.add_message(&s.id, Role::User, "original".into());
        let before = store.get_messages(&s.id);

        store.fork_session(&s.id).unwrap();

        let after = store.get_messages(&s.id);
        assert_eq!(after.len(), before.len());
        assert_eq!(after[0].id, before[0].id);
        assert_eq!(after[0].session_id, s.id);
        assert_eq!(store.list_sessions().len(), 2);
    }

    #[test]
    fn new_session_has_no_phenotype_binding() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        assert!(s.phenotype.is_none());
        assert!(store.session_phenotype(&s.id).is_none());
    }

    #[test]
    fn set_and_clear_session_phenotype() {
        let store = SessionStore::new();
        let s = store.create_session(None);

        store.set_session_phenotype(&s.id, Some("codon".into()));
        assert_eq!(store.session_phenotype(&s.id).as_deref(), Some("codon"));

        store.set_session_phenotype(&s.id, None);
        assert!(store.session_phenotype(&s.id).is_none());
    }

    #[test]
    fn set_session_phenotype_unknown_session_is_noop() {
        let store = SessionStore::new();
        store.set_session_phenotype("nope", Some("codon".into()));
        assert!(store.session_phenotype("nope").is_none());
    }

    #[test]
    fn fork_session_copies_phenotype_binding() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.set_session_phenotype(&s.id, Some("codon".into()));

        let forked = store.fork_session(&s.id).unwrap();
        assert_eq!(forked.phenotype.as_deref(), Some("codon"));
        // Changing the fork's binding does not touch the source.
        store.set_session_phenotype(&forked.id, Some("default".into()));
        assert_eq!(store.session_phenotype(&s.id).as_deref(), Some("codon"));
    }

    #[test]
    fn new_session_has_no_workspace() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        assert!(s.workspace.is_none());
        assert!(store.session_workspace(&s.id).is_none());
    }

    #[test]
    fn set_and_clear_session_workspace() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.set_session_workspace(&s.id, Some("/work/proj".into()));
        assert_eq!(
            store.session_workspace(&s.id).as_deref(),
            Some("/work/proj")
        );
        store.set_session_workspace(&s.id, None);
        assert!(store.session_workspace(&s.id).is_none());
    }

    #[test]
    fn set_session_workspace_unknown_session_is_noop() {
        let store = SessionStore::new();
        store.set_session_workspace("nope", Some("/work/proj".into()));
        assert!(store.session_workspace("nope").is_none());
    }

    #[test]
    fn fork_session_copies_workspace() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.set_session_workspace(&s.id, Some("/work/proj".into()));

        let forked = store.fork_session(&s.id).unwrap();
        assert_eq!(forked.workspace.as_deref(), Some("/work/proj"));
        // Changing the fork's cwd does not touch the source.
        store.set_session_workspace(&forked.id, Some("/work/other".into()));
        assert_eq!(
            store.session_workspace(&s.id).as_deref(),
            Some("/work/proj")
        );
    }

    #[test]
    fn new_session_has_no_mode_binding() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        assert!(s.mode.is_none());
        assert!(store.session_mode(&s.id).is_none());
    }

    #[test]
    fn set_and_clear_session_mode() {
        let store = SessionStore::new();
        let s = store.create_session(None);

        store.set_session_mode(&s.id, Some(Mode::Plan));
        assert_eq!(store.session_mode(&s.id), Some(Mode::Plan));

        store.set_session_mode(&s.id, None);
        assert!(store.session_mode(&s.id).is_none());
    }

    #[test]
    fn set_session_mode_unknown_session_is_noop() {
        let store = SessionStore::new();
        store.set_session_mode("nope", Some(Mode::Act));
        assert!(store.session_mode("nope").is_none());
    }

    #[test]
    fn fork_session_copies_mode_binding() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.set_session_mode(&s.id, Some(Mode::Act));

        let forked = store.fork_session(&s.id).unwrap();
        assert_eq!(forked.mode, Some(Mode::Act));
        // Changing the fork's binding does not touch the source.
        store.set_session_mode(&forked.id, Some(Mode::Auto));
        assert_eq!(store.session_mode(&s.id), Some(Mode::Act));
    }

    fn model_sel(connection: &str, model: &str) -> ModelSelection {
        ModelSelection {
            connection: connection.into(),
            model: model.into(),
        }
    }

    #[test]
    fn new_session_has_no_model_binding() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        assert!(s.model.is_none());
        assert!(store.session_model(&s.id).is_none());
    }

    #[test]
    fn set_and_clear_session_model() {
        let store = SessionStore::new();
        let s = store.create_session(None);

        let sel = model_sel("openai-main", "gpt-4o");
        store.set_session_model(&s.id, Some(sel.clone()));
        assert_eq!(store.session_model(&s.id), Some(sel));

        store.set_session_model(&s.id, None);
        assert!(store.session_model(&s.id).is_none());
    }

    #[test]
    fn set_session_model_unknown_session_is_noop() {
        let store = SessionStore::new();
        store.set_session_model("nope", Some(model_sel("openai-main", "gpt-4o")));
        assert!(store.session_model("nope").is_none());
    }

    #[test]
    fn fork_session_copies_model_binding() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        let sel = model_sel("openai-main", "gpt-4o");
        store.set_session_model(&s.id, Some(sel.clone()));

        let forked = store.fork_session(&s.id).unwrap();
        assert_eq!(forked.model, Some(sel.clone()));
        // Changing the fork's binding does not touch the source.
        store.set_session_model(&forked.id, Some(model_sel("anthropic", "claude")));
        assert_eq!(store.session_model(&s.id), Some(sel));
    }

    // --- Session-tier MCP overrides (RFC 0018 C3, #590) ---

    fn ws_server(id: &str) -> McpServerConfig {
        McpServerConfig {
            id: id.into(),
            command: "codegraph".into(),
            args: vec!["serve".into(), "--mcp".into()],
            env: Default::default(),
            disabled: false,
            scope: ff_core::McpScope::Workspace,
        }
    }

    #[test]
    fn new_session_has_no_mcp_servers_binding() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        assert!(s.mcp_servers.is_none());
        assert!(store.session_mcp_servers(&s.id).is_none());
    }

    #[test]
    fn set_and_clear_session_mcp_servers() {
        let store = SessionStore::new();
        let s = store.create_session(None);

        let servers = vec![ws_server("codegraph")];
        store.set_session_mcp_servers(&s.id, Some(servers.clone()));
        assert_eq!(store.session_mcp_servers(&s.id), Some(servers));

        store.set_session_mcp_servers(&s.id, None);
        assert!(store.session_mcp_servers(&s.id).is_none());
    }

    #[test]
    fn set_session_mcp_servers_unknown_session_is_noop() {
        let store = SessionStore::new();
        store.set_session_mcp_servers("nope", Some(vec![ws_server("codegraph")]));
        assert!(store.session_mcp_servers("nope").is_none());
    }

    #[test]
    fn fork_session_copies_mcp_servers_binding() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        let servers = vec![ws_server("codegraph")];
        store.set_session_mcp_servers(&s.id, Some(servers.clone()));

        let forked = store.fork_session(&s.id).unwrap();
        assert_eq!(forked.mcp_servers, Some(servers.clone()));
        // Changing the fork's binding does not touch the source.
        store.set_session_mcp_servers(&forked.id, Some(vec![ws_server("other")]));
        assert_eq!(store.session_mcp_servers(&s.id), Some(servers));
    }

    #[test]
    fn migration_v6_to_v7_preserves_session_and_adds_mcp_servers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let sid = "sess-1";
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                     id TEXT PRIMARY KEY, goal TEXT, title TEXT, summary TEXT,
                     status TEXT NOT NULL, created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL, phenotype TEXT, mode TEXT,
                     workspace TEXT, model TEXT
                 );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (id, goal, status, created_at, updated_at)
                 VALUES (?1, 'legacy goal', 'active', 0, 0)",
                params![sid],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 6).unwrap();
        }

        let store = SessionStore::open(&path).unwrap();
        // The pre-existing v6 row survives the ALTER with a NULL (inherited) set.
        let s = store.get_session(sid).unwrap();
        assert_eq!(s.goal.as_deref(), Some("legacy goal"));
        assert!(s.mcp_servers.is_none());
        // ...and the new column is writable post-upgrade.
        let servers = vec![ws_server("codegraph")];
        store.set_session_mcp_servers(sid, Some(servers.clone()));
        assert_eq!(store.session_mcp_servers(sid), Some(servers));

        let version: i64 = store
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 7);
    }

    // --- SQLite-backed persistence (RFC 0012 / #276) ---

    #[test]
    fn disk_store_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");

        let sid;
        let mid;
        {
            let store = SessionStore::open(&path).unwrap();
            let s = store.create_session(Some("durable goal".into()));
            sid = s.id.clone();
            store.set_title(&s.id, "Durable".into());
            store.set_session_phenotype(&s.id, Some("codon".into()));
            store.set_session_mode(&s.id, Some(Mode::Act));
            store.set_session_workspace(&s.id, Some("/work/proj".into()));
            store.set_session_model(&s.id, Some(model_sel("openai-main", "gpt-4o")));
            let m = store.add_message(&s.id, Role::User, "remember me".into());
            mid = m.id.clone();
            store.add_message(&s.id, Role::Assistant, "noted".into());
        }

        // Reopen over the same path: state is still there.
        let store = SessionStore::open(&path).unwrap();
        let sessions = store.list_sessions();
        assert_eq!(sessions.len(), 1);
        let reopened = &sessions[0];
        assert_eq!(reopened.id, sid);
        assert_eq!(reopened.title.as_deref(), Some("Durable"));
        assert_eq!(reopened.goal.as_deref(), Some("durable goal"));
        assert_eq!(store.session_phenotype(&sid).as_deref(), Some("codon"));
        assert_eq!(store.session_mode(&sid), Some(Mode::Act));
        assert_eq!(store.session_workspace(&sid).as_deref(), Some("/work/proj"));
        assert_eq!(
            store.session_model(&sid),
            Some(model_sel("openai-main", "gpt-4o"))
        );

        let msgs = store.get_messages(&sid);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].id, mid);
        assert_eq!(msgs[0].content, "remember me");
        assert_eq!(msgs[1].role, Role::Assistant);
    }

    #[test]
    fn fork_session_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");

        let source_id;
        let forked_id;
        {
            let store = SessionStore::open(&path).unwrap();
            let s = store.create_session(Some("durable fork".into()));
            source_id = s.id.clone();
            store.set_title(&s.id, "Durable fork".into());
            store.add_message(&s.id, Role::User, "copy me".into());
            store.add_message(&s.id, Role::Assistant, "copied".into());

            let forked = store.fork_session(&s.id).unwrap();
            forked_id = forked.id;
        }

        let store = SessionStore::open(&path).unwrap();
        let source_msgs = store.get_messages(&source_id);
        let forked_msgs = store.get_messages(&forked_id);
        assert_eq!(forked_msgs.len(), source_msgs.len());
        assert_eq!(forked_msgs[0].content, "copy me");
        assert_eq!(forked_msgs[1].content, "copied");
        assert_eq!(forked_msgs[0].session_id, forked_id);
        assert_ne!(forked_msgs[0].id, source_msgs[0].id);

        let forked = store.get_session(&forked_id).unwrap();
        assert_eq!(forked.title.as_deref(), Some("Durable fork (copy)"));
    }

    #[test]
    fn tool_calls_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let calls = vec![ToolCall {
            id: "call_1".into(),
            name: "bash".into(),
            arguments: r#"{"command":"ls"}"#.into(),
        }];
        let sid;
        let mid;
        {
            let store = SessionStore::open(&path).unwrap();
            let s = store.create_session(None);
            sid = s.id.clone();
            let m = store.add_message(&s.id, Role::Assistant, String::new());
            mid = m.id.clone();
            store.attach_tool_calls(&mid, &sid, calls.clone());
        }
        let store = SessionStore::open(&path).unwrap();
        let msgs = store.get_messages(&sid);
        assert_eq!(msgs[0].tool_calls.as_deref(), Some(calls.as_slice()));
    }

    #[test]
    fn cascade_delete_removes_messages_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let store = SessionStore::open(&path).unwrap();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "a".into());
        store.add_message(&s.id, Role::Assistant, "b".into());

        assert!(store.delete_session(&s.id));

        // The FK cascade must have removed the orphaned messages too.
        let count: i64 = store
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn messages_keep_insertion_order_via_seq() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        for i in 0..5 {
            store.add_message(&s.id, Role::User, format!("msg {i}"));
        }
        let msgs = store.get_messages(&s.id);
        let contents: Vec<&str> = msgs.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["msg 0", "msg 1", "msg 2", "msg 3", "msg 4"]);
    }

    #[test]
    fn migration_is_idempotent_across_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        SessionStore::open(&path).unwrap();
        let store = SessionStore::open(&path).unwrap();
        let version: i64 = store
            .conn
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 7);
    }

    #[test]
    fn export_json_round_trips() {
        let store = SessionStore::new();
        let s = store.create_session(Some("ship it".into()));
        store.add_message(&s.id, Role::User, "hello".into());
        store.add_message(&s.id, Role::Assistant, "hi there".into());

        let json = store.export_session(&s.id, Format::Json).unwrap();
        let env: ExportEnvelope = serde_json::from_str(&json).unwrap();

        assert_eq!(env.session, store.get_session(&s.id).unwrap());
        assert_eq!(env.messages, store.get_messages(&s.id));
    }

    #[test]
    fn export_markdown_has_headings_and_folds_large_tool_output() {
        let store = SessionStore::new();
        let s = store.create_session(Some("debug".into()));
        store.add_message(&s.id, Role::User, "whats wrong".into());
        let assistant = store.add_message(&s.id, Role::Assistant, "let me check".into());
        store.attach_tool_calls(
            &assistant.id,
            &s.id,
            vec![ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: "{\"path\":\"x\"}".into(),
            }],
        );
        let big = "x".repeat(TOOL_OUTPUT_FOLD_THRESHOLD + 100);
        store.add_tool_result_message(&s.id, "call_1".into(), big);

        let md = store.export_session(&s.id, Format::Markdown).unwrap();

        assert!(md.contains("- **Goal:** debug"));
        assert!(md.contains("## You"));
        assert!(md.contains("## Assistant"));
        assert!(md.contains("## Tool"));
        assert!(md.contains("**Tool call:** read_file("));
        assert!(md.contains("<details>"));
    }

    #[test]
    fn export_markdown_skips_system_messages() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::System, "you are a helpful agent".into());
        store.add_message(&s.id, Role::User, "hi".into());

        let md = store.export_session(&s.id, Format::Markdown).unwrap();

        assert!(!md.contains("you are a helpful agent"));
        assert!(md.contains("## You"));
    }

    #[test]
    fn export_markdown_includes_reasoning() {
        // #549: persisted chain-of-thought is folded into the Markdown export.
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "what is 17*23?".into());
        let a = store.add_message(&s.id, Role::Assistant, "391".into());
        store.set_message_reasoning(&a.id, &s.id, "17 * 23 = 391");

        let md = store.export_session(&s.id, Format::Markdown).unwrap();

        assert!(md.contains("<details><summary>Thought</summary>"));
        assert!(md.contains("17 * 23 = 391"));
        // The answer is still present after the fold.
        assert!(md.contains("391"));
    }

    #[test]
    fn export_unknown_session_is_none() {
        let store = SessionStore::new();
        assert!(store.export_session("nope", Format::Json).is_none());
        assert!(store.export_session("nope", Format::Markdown).is_none());
    }

    // --- edit_user_message (#464) ---

    #[test]
    fn edit_user_message_replaces_content_and_truncates_after() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        let u1 = store.add_message(&s.id, Role::User, "first question".into());
        store.add_message(&s.id, Role::Assistant, "first answer".into());
        store.add_message(&s.id, Role::User, "second question".into());
        store.add_message(&s.id, Role::Assistant, "second answer".into());

        let edited = store
            .edit_user_message(&s.id, &u1.id, "first question (edited)".into(), None)
            .unwrap();
        assert_eq!(edited, u1.id, "returns the edited message's id");

        let msgs = store.get_messages(&s.id);
        assert_eq!(
            msgs.len(),
            1,
            "old response and everything after is dropped"
        );
        assert_eq!(msgs[0].id, u1.id);
        assert_eq!(msgs[0].content, "first question (edited)");
    }

    #[test]
    fn edit_user_message_truncation_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let session_id;
        let edited_id;
        {
            let store = SessionStore::open(&path).unwrap();
            let s = store.create_session(None);
            session_id = s.id.clone();
            let u1 = store.add_message(&s.id, Role::User, "q1".into());
            edited_id = u1.id.clone();
            store.add_message(&s.id, Role::Assistant, "a1".into());
            store.add_message(&s.id, Role::User, "q2".into());
            store
                .edit_user_message(&s.id, &u1.id, "q1 edited".into(), None)
                .unwrap();
        }
        let store = SessionStore::open(&path).unwrap();
        let msgs = store.get_messages(&session_id);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, edited_id);
        assert_eq!(msgs[0].content, "q1 edited");
    }

    #[test]
    fn edit_user_message_updates_and_clears_attachments() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        let u = store.add_message(&s.id, Role::User, "look".into());

        let att = vec![Attachment {
            kind: ff_core::AttachmentKind::Image,
            media_type: "image/png".into(),
            source: ff_core::AttachmentSource::Inline("AAAA".into()),
            name: Some("shot.png".into()),
            bytes: 3,
        }];
        store
            .edit_user_message(&s.id, &u.id, "look (with image)".into(), Some(att))
            .unwrap();
        let msgs = store.get_messages(&s.id);
        assert_eq!(msgs[0].attachments.as_ref().map(Vec::len), Some(1));

        // Editing again with no attachments clears them back to NULL.
        store
            .edit_user_message(&s.id, &u.id, "look (no image)".into(), None)
            .unwrap();
        let msgs = store.get_messages(&s.id);
        assert_eq!(msgs[0].attachments, None);
    }

    #[test]
    fn edit_user_message_rejects_non_user_message() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "q".into());
        let a = store.add_message(&s.id, Role::Assistant, "a".into());

        let err = store
            .edit_user_message(&s.id, &a.id, "tampered".into(), None)
            .unwrap_err();
        assert_eq!(err, EditMessageError::NotUserMessage);
        // Transcript untouched.
        assert_eq!(store.get_messages(&s.id).len(), 2);
        assert_eq!(store.get_messages(&s.id)[1].content, "a");
    }

    #[test]
    fn edit_user_message_rejects_unknown_id() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "q".into());

        let err = store
            .edit_user_message(&s.id, "does-not-exist", "x".into(), None)
            .unwrap_err();
        assert_eq!(err, EditMessageError::UnknownMessage);
    }

    #[test]
    fn edit_user_message_rejects_id_from_another_session() {
        let store = SessionStore::new();
        let a = store.create_session(None);
        let b = store.create_session(None);
        let u = store.add_message(&a.id, Role::User, "in a".into());

        // The id exists, but not in session b.
        let err = store
            .edit_user_message(&b.id, &u.id, "x".into(), None)
            .unwrap_err();
        assert_eq!(err, EditMessageError::UnknownMessage);
        // Session a is untouched.
        assert_eq!(store.get_messages(&a.id)[0].content, "in a");
    }

    #[test]
    fn edit_user_message_drops_orphaned_compaction_originals() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        let u = store.add_message(&s.id, Role::User, "q".into());
        let a = store.add_message(&s.id, Role::Assistant, "a".into());
        store.put_compaction_original(&s.id, &a.id, "hash-key-1", "verbatim original");
        assert_eq!(
            store.compaction_original("hash-key-1").as_deref(),
            Some("verbatim original")
        );

        store
            .edit_user_message(&s.id, &u.id, "q edited".into(), None)
            .unwrap();

        // The assistant message that backed the original was truncated, so its
        // original must be gone too (no orphan).
        assert_eq!(store.compaction_original("hash-key-1"), None);
    }
}
