//! Session and message persistence.
//!
//! M1 uses an in-process store so the contract works end-to-end without a DB.
//! A later milestone swaps the backing store for SQLite behind this same API.
//! (Durable user memory -- facts, daily logs, recall -- is a separate concern,
//! owned by the `ff-memory` crate per RFC 0006.)

use std::collections::HashMap;
use std::sync::Mutex;

use ff_core::{auto_title, Message, Mode, Role, Session, SessionStatus, ToolCall};

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

#[derive(Default)]
struct Inner {
    sessions: HashMap<String, Session>,
    messages: HashMap<String, Vec<Message>>,
}

#[derive(Default)]
pub struct SessionStore {
    inner: Mutex<Inner>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
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
        };
        let mut inner = self.inner.lock().unwrap();
        inner.sessions.insert(session.id.clone(), session.clone());
        inner.messages.insert(session.id.clone(), Vec::new());
        session
    }

    pub fn list_sessions(&self) -> Vec<Session> {
        let inner = self.inner.lock().unwrap();
        let mut sessions: Vec<Session> = inner.sessions.values().cloned().collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        sessions
    }

    pub fn get_messages(&self, session_id: &str) -> Vec<Message> {
        let inner = self.inner.lock().unwrap();
        inner.messages.get(session_id).cloned().unwrap_or_default()
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
        let session_id = msg.session_id.clone();
        let mut inner = self.inner.lock().unwrap();
        let msgs = inner.messages.entry(session_id.clone()).or_default();
        // First user message in an untitled session seeds the auto-title, so a
        // title exists without a manual rename (covers background sessions too).
        let is_first_user_msg =
            msg.role == Role::User && !msgs.iter().any(|m| m.role == Role::User);
        msgs.push(msg.clone());
        if let Some(s) = inner.sessions.get_mut(&session_id) {
            s.updated_at = msg.created_at;
            if is_first_user_msg && s.title.is_none() {
                s.title = Some(auto_title(&msg.content));
            }
        }
        msg
    }

    /// Set a session's display title (the `rename_session` ipc). A manual title
    /// always wins over the auto-derived one. No-op for an unknown session.
    pub fn set_title(&self, session_id: &str, title: String) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(s) = inner.sessions.get_mut(session_id) {
            s.title = Some(title);
            s.updated_at = now_ms();
        }
    }

    /// Bind this session to a phenotype by name, or clear the binding with `None`
    /// so it inherits the global active phenotype again (#246). No-op for an
    /// unknown session. The name is *not* validated here — the store is dumb
    /// persistence; the app layer validates against the phenotype registry before
    /// calling, and resolution falls back to global on an unknown name anyway.
    pub fn set_session_phenotype(&self, session_id: &str, phenotype: Option<String>) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(s) = inner.sessions.get_mut(session_id) {
            s.phenotype = phenotype;
            s.updated_at = now_ms();
        }
    }

    /// The session's bound phenotype name, or `None` if it inherits the global
    /// active one (or the session is unknown).
    pub fn session_phenotype(&self, session_id: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .sessions
            .get(session_id)
            .and_then(|s| s.phenotype.clone())
    }

    /// Bind (or clear, with `None`) this session's autonomy mode (#265). Mirrors
    /// [`set_session_phenotype`](Self::set_session_phenotype).
    pub fn set_session_mode(&self, session_id: &str, mode: Option<Mode>) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(s) = inner.sessions.get_mut(session_id) {
            s.mode = mode;
            s.updated_at = now_ms();
        }
    }

    /// The session's bound mode, or `None` if it inherits the global `defaultMode`
    /// preference (or the session is unknown).
    pub fn session_mode(&self, session_id: &str) -> Option<Mode> {
        self.inner
            .lock()
            .unwrap()
            .sessions
            .get(session_id)
            .and_then(|s| s.mode)
    }

    pub fn set_message_content(
        &self,
        message_id: &str,
        session_id: &str,
        content: String,
    ) -> Message {
        let mut inner = self.inner.lock().unwrap();
        let ts = now_ms();
        if let Some(msgs) = inner.messages.get_mut(session_id) {
            if let Some(m) = msgs.iter_mut().find(|m| m.id == message_id) {
                m.content = content;
                m.created_at = ts;
                return m.clone();
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
        let mut inner = self.inner.lock().unwrap();
        if let Some(msgs) = inner.messages.get_mut(session_id) {
            if let Some(m) = msgs.iter_mut().find(|m| m.id == message_id) {
                m.tool_calls = Some(tool_calls);
            }
        }
    }

    pub fn set_status(&self, session_id: &str, status: SessionStatus) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(s) = inner.sessions.get_mut(session_id) {
            s.status = status;
            s.updated_at = now_ms();
        }
    }

    /// Permanently remove a session and its transcript. Returns whether the
    /// session existed. Idempotent: deleting an unknown id is a no-op.
    pub fn delete_session(&self, session_id: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        inner.messages.remove(session_id);
        inner.sessions.remove(session_id).is_some()
    }

    /// Clone a session and its full transcript into a new session. The copy gets
    /// fresh ids and timestamps; messages are re-keyed to the new session id and
    /// a titled source becomes "<title> (copy)". Returns `None` for an unknown id.
    pub fn fork_session(&self, session_id: &str) -> Option<Session> {
        let ts = now_ms();
        let mut inner = self.inner.lock().unwrap();
        let source = inner.sessions.get(session_id)?.clone();
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
        };
        let cloned: Vec<Message> = inner
            .messages
            .get(session_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|m| Message {
                id: new_id(),
                session_id: forked.id.clone(),
                ..m
            })
            .collect();
        inner.sessions.insert(forked.id.clone(), forked.clone());
        inner.messages.insert(forked.id.clone(), cloned);
        Some(forked)
    }
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
}
