//! Session and message persistence.
//!
//! M1 uses an in-process store so the contract works end-to-end without a DB.
//! M5 swaps the backing store for SQLite + vector recall behind this same API.

use std::collections::HashMap;
use std::sync::Mutex;

use ff_core::{Message, Role, Session, SessionStatus, ToolCall};

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
pub struct MemoryStore {
    inner: Mutex<Inner>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_session(&self, goal: Option<String>) -> Session {
        let ts = now_ms();
        let session = Session {
            id: new_id(),
            goal,
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
        inner
            .messages
            .entry(session_id.clone())
            .or_default()
            .push(msg.clone());
        if let Some(s) = inner.sessions.get_mut(&session_id) {
            s.updated_at = msg.created_at;
        }
        msg
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_and_message_roundtrip() {
        let store = MemoryStore::new();
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
