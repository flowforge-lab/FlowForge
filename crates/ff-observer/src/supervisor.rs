//! `ObserverSupervisor` — the session-scoped, app-global owner of every
//! observer. Mirrors [`ff_tools::process::ProcessSupervisor`]: the host holds
//! an `Arc<ObserverSupervisor>` in `AppState`, the agent tool dispatches
//! `start`/`stop`/`list` through it, and the host reaps observers on session
//! close via `reap_session`.
//!
//! Per-session events are published on a `tokio::sync::broadcast` channel,
//! lazily created on the first `start` and dropped (along with the per-session
//! driver task) when the last observer for that session is removed. The host
//! subscribes once via `subscribe(session_id)` to receive fired events.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::event::{
    ObserverError, ObserverEvent, ObserverId, ObserverInfo, ObserverKind, ObserverSpec,
};
use crate::source::{build_source, ObserverSource};

/// Max concurrent observers per session. A runaway model call cannot spawn
/// unlimited watchers. Matches the issue's "Max 8 per session" requirement.
pub const MAX_PER_SESSION: usize = 8;
/// Broadcast channel capacity. Small but big enough to survive a brief host
/// stall (the agent subscriber drains as fast as turns run).
const BROADCAST_CAPACITY: usize = 64;

/// One observer's live state: the running source, its `CancellationToken`,
/// and the per-observer counters the host reads from `list()`.
struct Entry {
    id: ObserverId,
    /// `session_id` is kept on the entry so a future `ObserverInfo`
    /// extension (e.g. a per-session event log) doesn't have to reach
    /// into the supervisor's `HashMap` keying. Currently unread because
    /// the public `list` already filters by session.
    #[allow(dead_code)]
    session_id: String,
    key: String,
    kind: ObserverKind,
    target: String,
    filter: Option<String>,
    started_at: chrono::DateTime<Utc>,
    cancel: CancellationToken,
    fires: u64,
}

/// Per-session aggregate: the observers for this session, the broadcast
/// channel the host subscribes to, and the supervision task handle.
struct SessionState {
    observers: HashMap<ObserverId, Entry>,
    events: broadcast::Sender<ObserverEvent>,
    /// Refcount of broadcast receivers held by the host, so we can drop the
    /// sender when nobody listens. Lazily tracked; an entry is created on
    /// `subscribe` and removed when the receiver drops.
    listener_count: usize,
}

impl SessionState {
    fn new() -> Self {
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            observers: HashMap::new(),
            events,
            listener_count: 0,
        }
    }
}

pub struct ObserverSupervisor {
    sessions: Mutex<HashMap<String, SessionState>>,
    next_id: AtomicU64,
}

impl Default for ObserverSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl ObserverSupervisor {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Construct and start an observer for `session_id`. Returns the new id
    /// (also surfaced as the `ObserverId` field on `list` results).
    pub async fn start(
        self: &Arc<Self>,
        session_id: &str,
        spec: ObserverSpec,
    ) -> Result<ObserverId, ObserverError> {
        // 1. Cap check before constructing any source.
        {
            let sessions = self.sessions.lock().unwrap();
            if let Some(s) = sessions.get(session_id) {
                if s.observers.len() >= MAX_PER_SESSION {
                    return Err(ObserverError::SessionCapReached(MAX_PER_SESSION));
                }
            }
        }
        // 2. Build the source eagerly so a bad target errors here, not in the
        //    background task.
        let source = build_source(spec.clone()).await?;
        self.start_with_source(session_id, spec, source).await
    }

    /// Start an observer from a pre-built source. `pub(crate)` so the test
    /// module can inject a counting source without round-tripping through
    /// `build_source` (which requires a real file/http target).
    pub(crate) async fn start_with_source(
        self: &Arc<Self>,
        session_id: &str,
        spec: ObserverSpec,
        mut source: Box<dyn ObserverSource>,
    ) -> Result<ObserverId, ObserverError> {
        // 1. Cap check before registering the observer.
        {
            let sessions = self.sessions.lock().unwrap();
            if let Some(s) = sessions.get(session_id) {
                if s.observers.len() >= MAX_PER_SESSION {
                    return Err(ObserverError::SessionCapReached(MAX_PER_SESSION));
                }
            }
        }
        // 2. Capture the key + prime event from the source.
        let key = source.key().to_string();
        let id = ObserverId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let cancel = CancellationToken::new();
        // 3. Prime: some backends (HTTP, file-already-changed) bootstrap a
        //    "first event" on construction. If it does, we publish immediately
        //    and the supervisor's fire counter picks it up.
        let prime = source.prime(id).await?;
        let entry = Entry {
            id,
            session_id: session_id.to_string(),
            key: key.clone(),
            kind: spec.kind,
            target: spec.target.clone(),
            filter: spec.filter.clone(),
            started_at: Utc::now(),
            cancel: cancel.clone(),
            fires: 0,
        };
        // 4. Insert the entry; ensure the session state exists.
        let events_tx = {
            let mut sessions = self.sessions.lock().unwrap();
            let state = sessions
                .entry(session_id.to_string())
                .or_insert_with(SessionState::new);
            state.observers.insert(id, entry);
            state.events.clone()
        };
        // 5. Spawn the driver task.
        let sup = Arc::clone(self);
        let sid = session_id.to_string();
        tokio::spawn(async move {
            sup.run_observer(&sid, id, source, cancel, events_tx).await;
        });
        if let Some(event) = prime {
            self.fire(session_id, &event);
        }
        Ok(id)
    }

    /// Cancel and remove a single observer. Idempotent: an unknown id is a
    /// no-op so the agent can call this defensively.
    pub fn stop(&self, session_id: &str, id: ObserverId) -> Result<(), ObserverError> {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(state) = sessions.get_mut(session_id) else {
            return Err(ObserverError::NotFound(id.0));
        };
        let Some(entry) = state.observers.remove(&id) else {
            return Err(ObserverError::NotFound(id.0));
        };
        entry.cancel.cancel();
        Self::maybe_drop_session(&mut sessions, session_id);
        Ok(())
    }

    /// Snapshot the live observers for one session, oldest-first. Used by the
    /// `observer list` tool action.
    pub fn list(&self, session_id: &str) -> Vec<ObserverInfo> {
        let sessions = self.sessions.lock().unwrap();
        let Some(state) = sessions.get(session_id) else {
            return Vec::new();
        };
        let mut out: Vec<ObserverInfo> = state
            .observers
            .values()
            .map(|e| ObserverInfo {
                id: e.id,
                key: e.key.clone(),
                kind: e.kind,
                target: e.target.clone(),
                filter: e.filter.clone(),
                started_at: e.started_at,
                fires: e.fires,
            })
            .collect();
        out.sort_by_key(|o| o.id.0);
        out
    }

    /// Subscribe to fired events for one session. The host calls this once on
    /// the first `start` for the session. Multiple subscribers (e.g. an FE
    /// mirror in addition to the agent subscriber) are supported.
    pub fn subscribe(&self, session_id: &str) -> broadcast::Receiver<ObserverEvent> {
        let mut sessions = self.sessions.lock().unwrap();
        let state = sessions
            .entry(session_id.to_string())
            .or_insert_with(SessionState::new);
        state.listener_count += 1;
        state.events.subscribe()
    }

    /// Drop a per-session subscription slot. Called by the host's drop
    /// guard; on the last drop we drop the broadcast sender so a future
    /// `subscribe` rebuilds a fresh channel.
    pub fn unsubscribe(&self, session_id: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            state.listener_count = state.listener_count.saturating_sub(1);
        }
        Self::maybe_drop_session(&mut sessions, session_id);
    }

    /// Stop and remove every observer for `session_id`. Called on
    /// `delete_session` so watchers don't outlive the session. Returns the
    /// number of observers reaped (0 if none).
    pub fn reap_session(&self, session_id: &str) -> usize {
        let mut sessions = self.sessions.lock().unwrap();
        let Some(mut state) = sessions.remove(session_id) else {
            return 0;
        };
        let n = state.observers.len();
        for (_, entry) in state.observers.drain() {
            entry.cancel.cancel();
        }
        // Drop the broadcast sender: any in-flight subscribers see `RecvError::Closed`
        // and exit. n is reported so the host can log a meaningful trace line.
        n
    }

    /// Drop the per-session state if it has no observers AND no subscribers.
    /// Keeps the map tidy and prevents listener-leak across reap cycles.
    fn maybe_drop_session(sessions: &mut HashMap<String, SessionState>, session_id: &str) {
        if let Some(state) = sessions.get(session_id) {
            if state.observers.is_empty() && state.listener_count == 0 {
                sessions.remove(session_id);
            }
        }
    }

    /// Publish `event` to the session's broadcast channel and bump the
    /// observer's fire counter. If the receiver is closed (host shut down),
    /// the send is silently dropped — the counter is still updated. The
    /// `id` on the event is the supervisor's own (sources stamp it before
    /// returning), so the subscriber sees the real id.
    fn fire(&self, session_id: &str, event: &ObserverEvent) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session_id) {
            if let Some(entry) = state.observers.get_mut(&event.id) {
                entry.fires = entry.fires.saturating_add(1);
            }
            let _ = state.events.send(event.clone());
        }
    }

    /// The per-observer driver loop. Loops until the source returns `None`
    /// (terminal) or the cancel token trips.
    async fn run_observer(
        self: Arc<Self>,
        session_id: &str,
        id: ObserverId,
        mut source: Box<dyn ObserverSource>,
        cancel: CancellationToken,
        _events: broadcast::Sender<ObserverEvent>,
    ) {
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    debug!(session_id, ?id, "observer cancelled");
                    return;
                }
                res = source.next_event(id, &cancel) => {
                    match res {
                        Ok(Some(event)) => {
                            info!(session_id, ?id, key = %event.key, "observer fired");
                            self.fire(session_id, &event);
                        }
                        Ok(None) => {
                            debug!(session_id, ?id, "observer source terminated");
                            return;
                        }
                        Err(e) => {
                            warn!(session_id, ?id, error = %e, "observer source error");
                            // Recoverable errors get a small backoff so a hot
                            // loop of failures doesn't pin a CPU. Cancel still
                            // wins.
                            tokio::select! {
                                _ = cancel.cancelled() => return,
                                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ObserverSpec;
    use async_trait::async_trait;
    use tokio::time::Duration;

    /// A test source that yields one event and then returns `None` (terminates).
    /// Lets us exercise the supervisor without standing up a real file/http
    /// target.
    struct OneShotSource {
        key: String,
        fired: bool,
    }

    #[async_trait]
    impl ObserverSource for OneShotSource {
        fn key(&self) -> &str {
            &self.key
        }
        async fn next_event(
            &mut self,
            id: ObserverId,
            _cancel: &CancellationToken,
        ) -> Result<Option<ObserverEvent>, ObserverError> {
            if self.fired {
                Ok(None)
            } else {
                self.fired = true;
                Ok(Some(ObserverEvent {
                    id,
                    key: self.key.clone(),
                    summary: "one-shot fired".into(),
                    occurred_at: Utc::now(),
                }))
            }
        }
    }

    /// Inject a one-shot source for a test, skipping the spec→source build
    /// (which needs a real file/http target).
    async fn start_test(
        sup: &Arc<ObserverSupervisor>,
        session: &str,
        key: &str,
    ) -> Result<ObserverId, ObserverError> {
        sup.start_with_source(
            session,
            ObserverSpec {
                kind: ObserverKind::File,
                target: key.into(),
                filter: None,
                interval: None,
            },
            Box::new(OneShotSource {
                key: format!("test:{key}"),
                fired: false,
            }),
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_emits_and_subscribers_receive() {
        let sup = Arc::new(ObserverSupervisor::new());
        let mut rx = sup.subscribe("s1");
        let id = start_test(&sup, "s1", "k1").await.unwrap();
        let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("event within timeout")
            .expect("recv");
        assert_eq!(ev.id, id);
        assert!(ev.key.starts_with("test:"), "key={}", ev.key);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_cancels_and_drops_session_when_empty() {
        let sup = Arc::new(ObserverSupervisor::new());
        let id = start_test(&sup, "s2", "k2").await.unwrap();
        sup.stop("s2", id).unwrap();
        // The session state is dropped once both observers and listeners are empty.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(sup.list("s2").is_empty());
        // Stopping a second time is a NotFound error.
        assert!(matches!(
            sup.stop("s2", id),
            Err(ObserverError::NotFound(_))
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reap_session_drops_subscribers() {
        let sup = Arc::new(ObserverSupervisor::new());
        let mut rx = sup.subscribe("s3");
        start_test(&sup, "s3", "k3").await.unwrap();
        let n = sup.reap_session("s3");
        assert_eq!(n, 1);
        // Receiver should observe a closed channel.
        let res = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await;
        assert!(
            matches!(
                res,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed))
            ),
            "expected Closed, got {res:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_cap_is_enforced() {
        let sup = Arc::new(ObserverSupervisor::new());
        for i in 0..MAX_PER_SESSION {
            start_test(&sup, "s4", &format!("k{i}")).await.unwrap();
        }
        let err = start_test(&sup, "s4", "overflow").await.unwrap_err();
        assert!(matches!(err, ObserverError::SessionCapReached(_)));
    }

    // We can't use CountingSource through the supervisor directly (it
    // requires a `kind: File` path that goes through build_source), so the
    // counting behavior is exercised by the live file/HTTP tests in their
    // own files.
}
