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

use ff_core::{auto_title, Format, Message, Mode, Role, Session, SessionStatus, ToolCall};
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
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
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
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions
                 (id, goal, title, summary, status, created_at, updated_at, phenotype, mode, workspace)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
            ],
        )
        .expect("insert session");
        session
    }

    pub fn list_sessions(&self) -> Vec<Session> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, goal, title, summary, status, created_at, updated_at, phenotype, mode, workspace
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
                "SELECT id, session_id, role, content, tool_calls, tool_call_id, created_at
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
            created_at: now_ms(),
        })
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
        conn.execute(
            "INSERT INTO messages
                 (id, session_id, seq, role, content, tool_calls, tool_call_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                msg.id,
                msg.session_id,
                seq,
                enum_to_text(&msg.role),
                msg.content,
                tool_calls,
                msg.tool_call_id,
                msg.created_at,
            ],
        )
        .expect("insert message");
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
                    "SELECT id, session_id, role, content, tool_calls, tool_call_id, created_at
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
            created_at: ts,
        }
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

    /// Clone a session and its full transcript into a new session. The copy gets
    /// fresh ids and timestamps; messages are re-keyed to the new session id and
    /// a titled source becomes "<title> (copy)". Returns `None` for an unknown id.
    pub fn fork_session(&self, session_id: &str) -> Option<Session> {
        let ts = now_ms();
        let conn = self.conn.lock().unwrap();
        let source = conn
            .query_row(
                "SELECT id, goal, title, summary, status, created_at, updated_at, phenotype, mode, workspace
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
        };
        conn.execute(
            "INSERT INTO sessions
                 (id, goal, title, summary, status, created_at, updated_at, phenotype, mode, workspace)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
            ],
        )
        .expect("insert forked session");
        // Re-key the transcript to the new session, preserving `seq` order.
        conn.execute(
            "INSERT INTO messages
                 (id, session_id, seq, role, content, tool_calls, tool_call_id, created_at)
             SELECT lower(hex(randomblob(16))), ?1, seq, role, content, tool_calls,
                    tool_call_id, created_at
             FROM messages WHERE session_id = ?2",
            params![forked.id, session_id],
        )
        .expect("clone forked messages");
        Some(forked)
    }

    /// Fetch a single session by id, or `None` if it does not exist.
    pub fn get_session(&self, session_id: &str) -> Option<Session> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, goal, title, summary, status, created_at, updated_at, phenotype, mode, workspace
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
    Ok(())
}

fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<Session> {
    let status: String = row.get("status")?;
    let mode: Option<String> = row.get("mode")?;
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
    })
}

fn row_to_message(row: &rusqlite::Row) -> rusqlite::Result<Message> {
    let role: String = row.get("role")?;
    let tool_calls: Option<String> = row.get("tool_calls")?;
    Ok(Message {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        role: text_to_enum(&role).unwrap_or(Role::User),
        content: row.get("content")?,
        tool_calls: tool_calls.and_then(|s| serde_json::from_str(&s).ok()),
        tool_call_id: row.get("tool_call_id")?,
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

        let msgs = store.get_messages(&sid);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].id, mid);
        assert_eq!(msgs[0].content, "remember me");
        assert_eq!(msgs[1].role, Role::Assistant);
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
        assert_eq!(version, 2);
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
    fn export_unknown_session_is_none() {
        let store = SessionStore::new();
        assert!(store.export_session("nope", Format::Json).is_none());
        assert!(store.export_session("nope", Format::Markdown).is_none());
    }
}
