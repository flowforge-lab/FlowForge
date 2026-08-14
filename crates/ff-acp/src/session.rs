//! `session/cancel` → [`CancelToken`].
//!
//! `session/cancel` is a **notification**, not one of the twelve client→agent requests,
//! so there is no response in which to report an unknown session. It is also
//! session-scoped, while our turn machinery cancels per token — this registry is the
//! join between the two.

use crate::wire;
use ff_agent::CancelToken;
use ff_core::Mode;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Maps live ACP sessions to the token that stops their in-flight turn.
///
/// Mirrors the desktop's proven `register_cancel` / `take_cancel_if` pattern, kept here
/// so an ACP host does not have to reinvent it.
///
/// # Why poisoning is recovered rather than propagated
///
/// An ACP host is a long-lived process, so a panic in one turn must not make *every*
/// later `session/cancel` panic — that turns one failed turn into an unusable host, and
/// cancellation is exactly the path a user reaches for when something has gone wrong.
///
/// Recovery is sound here because the maps hold only `CancelToken`s (`Arc<AtomicBool>`
/// flags) and `Copy` [`Mode`] values, neither of which carries a cross-entry invariant,
/// so a panic mid-operation cannot leave the contents inconsistent — there is no broken
/// state for poisoning to protect us from. Follows the same
/// `unwrap_or_else(|p| p.into_inner())` pattern as `ff-observer`'s watcher lock.
///
/// # Two maps, two lifecycles
///
/// The cancel-token map is **per-turn**: a token is registered when a `session/prompt`
/// begins and removed when it ends. The mode map is **per-session**: it is seeded at
/// `session/new`, updated by `session/set_mode`, read by every subsequent turn, and must
/// therefore survive turn cleanup. Keeping them separate is what lets a turn's [`remove`]
/// clear its token without forgetting the session's mode.
///
/// [`remove`]: SessionRegistry::remove
#[derive(Default)]
pub struct SessionRegistry {
    inner: Mutex<HashMap<Arc<str>, CancelToken>>,
    modes: Mutex<HashMap<Arc<str>, Mode>>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<Arc<str>, CancelToken>> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn mode_entries(&self) -> std::sync::MutexGuard<'_, HashMap<Arc<str>, Mode>> {
        self.modes.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Associate a session with the token for its current turn.
    ///
    /// Re-registering an id replaces the previous token, which is what a second
    /// `session/prompt` on the same session should do.
    pub fn register(&self, session: &wire::SessionId, token: CancelToken) {
        self.entries().insert(Arc::clone(&session.0), token);
    }

    /// Cancel a session's in-flight turn.
    ///
    /// Returns `false` for an unknown session. The caller logs and drops it rather than
    /// erroring: a notification has no response channel, and a late `session/cancel`
    /// arriving after a turn has finished is legal per spec, not a fault.
    pub fn cancel(&self, session: &wire::SessionId) -> bool {
        match self.entries().get(&session.0) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Forget a session's cancel token once its turn is done, so the token map does not
    /// grow without bound in a long-lived host process.
    ///
    /// This is **per-turn** cleanup and deliberately leaves the session's mode intact —
    /// the session outlives any single turn, and a later `session/prompt` reuses its mode.
    pub fn remove(&self, session: &wire::SessionId) {
        self.entries().remove(&session.0);
    }

    /// Record the mode a session should run in, replacing any previous value.
    ///
    /// Called at `session/new` (seeding the default) and on every `session/set_mode`.
    pub fn set_mode(&self, session: &wire::SessionId, mode: Mode) {
        self.mode_entries().insert(Arc::clone(&session.0), mode);
    }

    /// The mode a session is currently in, or [`Mode::default`] for an unknown session.
    ///
    /// Falling back to the default (rather than erroring) keeps a `session/prompt` that
    /// races ahead of its `session/new` bookkeeping safe — it runs in the same mode a
    /// fresh session would.
    pub fn mode(&self, session: &wire::SessionId) -> Mode {
        self.mode_entries()
            .get(&session.0)
            .copied()
            .unwrap_or_default()
    }

    /// Number of tracked sessions. Test/observability aid.
    pub fn len(&self) -> usize {
        self.entries().len()
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

    /// A panic while the lock is held must not disable cancellation for the rest of the
    /// host's life. Without poison recovery every later `session/cancel` would panic,
    /// escalating one bad turn into an unusable process — and cancellation is precisely
    /// what a user reaches for when a turn has gone wrong.
    #[test]
    fn a_poisoned_lock_still_cancels() {
        use std::panic::{catch_unwind, AssertUnwindSafe};

        let reg = SessionRegistry::new();
        let token = CancelToken::new();
        reg.register(&id("sess"), token.clone());

        // Poison the mutex: panic while holding the guard.
        let poisoned = catch_unwind(AssertUnwindSafe(|| {
            let _guard = reg.inner.lock().unwrap();
            panic!("simulated panic while holding the registry lock");
        }));
        assert!(poisoned.is_err(), "the test must actually panic");
        assert!(
            reg.inner.is_poisoned(),
            "the mutex must actually be poisoned, or this test proves nothing"
        );

        // The registry keeps working, and the entry survived.
        assert_eq!(reg.len(), 1);
        assert!(reg.cancel(&id("sess")));
        assert!(token.is_cancelled());
    }
}
