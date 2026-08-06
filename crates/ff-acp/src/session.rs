//! `session/cancel` → [`CancelToken`].
//!
//! `session/cancel` is a **notification**, not one of the twelve client→agent requests,
//! so there is no response in which to report an unknown session. It is also
//! session-scoped, while our turn machinery cancels per token — this registry is the
//! join between the two.

use crate::wire;
use ff_agent::CancelToken;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Maps live ACP sessions to the token that stops their in-flight turn.
///
/// Mirrors the desktop's proven `register_cancel` / `take_cancel_if` pattern, kept here
/// so an ACP host does not have to reinvent it.
#[derive(Default)]
pub struct SessionRegistry {
    inner: Mutex<HashMap<Arc<str>, CancelToken>>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Associate a session with the token for its current turn.
    ///
    /// Re-registering an id replaces the previous token, which is what a second
    /// `session/prompt` on the same session should do.
    pub fn register(&self, session: &wire::SessionId, token: CancelToken) {
        self.inner
            .lock()
            .expect("session registry mutex")
            .insert(Arc::clone(&session.0), token);
    }

    /// Cancel a session's in-flight turn.
    ///
    /// Returns `false` for an unknown session. The caller logs and drops it rather than
    /// erroring: a notification has no response channel, and a late `session/cancel`
    /// arriving after a turn has finished is legal per spec, not a fault.
    pub fn cancel(&self, session: &wire::SessionId) -> bool {
        match self
            .inner
            .lock()
            .expect("session registry mutex")
            .get(&session.0)
        {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Forget a session once its turn is done, so the map does not grow without bound
    /// in a long-lived host process.
    pub fn remove(&self, session: &wire::SessionId) {
        self.inner
            .lock()
            .expect("session registry mutex")
            .remove(&session.0);
    }

    /// Number of tracked sessions. Test/observability aid.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("session registry mutex").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> wire::SessionId {
        wire::SessionId::new(s)
    }

    #[test]
    fn cancel_is_session_scoped() {
        let reg = SessionRegistry::new();
        let (a, b) = (CancelToken::new(), CancelToken::new());
        reg.register(&id("sess_a"), a.clone());
        reg.register(&id("sess_b"), b.clone());

        assert!(reg.cancel(&id("sess_a")));
        assert!(a.is_cancelled());
        assert!(
            !b.is_cancelled(),
            "cancelling one session must not stop another"
        );
    }

    #[test]
    fn an_unknown_session_reports_false_rather_than_failing() {
        let reg = SessionRegistry::new();
        assert!(!reg.cancel(&id("never_registered")));

        // A late cancel for a finished (removed) turn behaves the same way.
        let token = CancelToken::new();
        reg.register(&id("sess"), token);
        reg.remove(&id("sess"));
        assert!(!reg.cancel(&id("sess")));
    }

    #[test]
    fn re_registering_replaces_the_token() {
        let reg = SessionRegistry::new();
        let (first, second) = (CancelToken::new(), CancelToken::new());
        reg.register(&id("sess"), first.clone());
        reg.register(&id("sess"), second.clone());

        assert!(reg.cancel(&id("sess")));
        assert!(second.is_cancelled(), "the current turn must be cancelled");
        assert!(
            !first.is_cancelled(),
            "the superseded token must not be touched"
        );
        assert_eq!(reg.len(), 1, "re-registering must not leak an entry");
    }

    #[test]
    fn remove_frees_the_entry() {
        let reg = SessionRegistry::new();
        reg.register(&id("sess"), CancelToken::new());
        assert_eq!(reg.len(), 1);
        reg.remove(&id("sess"));
        assert!(reg.is_empty());
    }
}
