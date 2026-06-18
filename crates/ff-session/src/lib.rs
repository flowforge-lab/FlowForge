//! Session and message persistence.
//!
//! M1 uses an in-process store so the contract works end-to-end without a DB.
//! A later milestone swaps the backing store for SQLite behind this same API.
//! (Durable user memory -- facts, daily logs, recall -- is a separate concern,
//! owned by the `ff-memory` crate per RFC 0006.)

use std::collections::HashMap;
use std::sync::Mutex;

use ff_core::{auto_title, Message, Role, Session, SessionStatus, ToolCall};

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
}
