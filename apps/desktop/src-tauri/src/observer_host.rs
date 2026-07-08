//! `ObserverHost` — the host-side wrapper around [`ff_observer::ObserverSupervisor`].
//!
//! Two responsibilities:
//!
//! 1. **Lazy per-session subscriber arming.** The model can call
//!    `observer start` at any point in a turn. The first such call for a
//!    session must spawn the background task that converts fired events
//!    into synthetic user messages + assistant turns. Subsequent calls
//!    are no-ops on the subscriber side — the task is already running.
//!    The map of "armed" sessions is the bookkeeping state; `reap_session`
//!    clears it so a re-armed session (e.g. after a `delete_session` then
//!    a new `start`) re-spawns a fresh subscriber task.
//!
//! 2. **Shared `ProcessSupervisor` injection.** The `observer --source
//!    process` backend subscribes to the same per-process line stream the
//!    `process_manager` tool populates. The supervisor lives in
//!    `AppState`; the host stashes a clone via
//!    [`ObserverHost::set_process_supervisor`] at boot.
//!
//! Both responsibilities are small enough that a single module owns them
//! without a public surface bigger than the supervisor's own.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use ff_observer::{ObserverId, ObserverSpec, ObserverSupervisor};
use ff_tools::process::ProcessSupervisor;
use tauri::AppHandle;
use tracing::warn;

/// A pre-armed `ObserverHost`. Cheap to construct; carries the supervisor
/// + the per-session subscriber-armed set + the shared process supervisor.
pub struct ObserverHost {
    supervisor: Arc<ObserverSupervisor>,
    /// Sessions with a live observer-event subscriber task. Inserted on
    /// the first `start` for the session, removed when the subscriber
    /// task exits (the broadcast channel closes when the supervisor's
    /// `reap_session` drops the sender, or when the supervisor itself is
    /// dropped at app exit).
    armed: Mutex<HashSet<String>>,
    /// The `ProcessSupervisor` shared with `process_manager`. `None` until
    /// [`set_process_supervisor`] runs at app boot; the `process`
    /// observer backend errors if a `start` arrives before this is set.
    process_supervisor: Mutex<Option<Arc<ProcessSupervisor>>>,
}

impl ObserverHost {
    pub fn new(_process_supervisor: Arc<ProcessSupervisor>) -> Self {
        // The constructor takes a stand-in supervisor; the real one is
        // installed by `set_process_supervisor` once `AppState::new` has
        // finished allocating its fields. The stand-in is never observed
        // because the swap happens before any tool call.
        Self {
            supervisor: Arc::new(ObserverSupervisor::new()),
            armed: Mutex::new(HashSet::new()),
            process_supervisor: Mutex::new(None),
        }
    }

    /// Install the shared `ProcessSupervisor` for the `process` observer
    /// backend. Idempotent: a second call replaces the handle (rare; the
    /// host typically installs exactly once at boot).
    pub fn set_process_supervisor(&self, sup: Arc<ProcessSupervisor>) {
        *self.process_supervisor.lock().unwrap() = Some(sup.clone());
        // Mirror into the static the `ProcessSource` reads from, so the
        // source can be built without an explicit supervisor injection
        // (the public `ObserverTool` API doesn't take a process supervisor).
        ff_observer::process::set_supervisor(sup);
    }

    /// The wrapped supervisor. Used by `start_dev_update_watcher` and
    /// the `ObserverTool` constructor.
    pub fn supervisor(&self) -> Arc<ObserverSupervisor> {
        self.supervisor.clone()
    }

    /// Start an observer for `session_id`, arming the per-session
    /// subscriber on the first call. The `app` handle is needed to
    /// dispatch fired events back to the FE.
    pub async fn start(
        self: &Arc<Self>,
        session_id: &str,
        spec: ObserverSpec,
        app: AppHandle,
        state: Arc<crate::state::AppState>,
    ) -> Result<ObserverId, ff_observer::event::ObserverError> {
        let id = self.supervisor.start(session_id, spec).await?;
        // Arm the subscriber if not already armed. Re-arming on
        // subsequent calls is a no-op (the first insertion wins).
        let needs_arm = !self.armed.lock().unwrap().contains(session_id);
        if needs_arm {
            self.armed.lock().unwrap().insert(session_id.to_string());
            self.spawn_subscriber(session_id.to_string(), app, state);
        }
        Ok(id)
    }

    /// Stop a single observer. Idempotent on unknown ids. Clears the
    /// `armed` flag when the last observer for the session is removed,
    /// so the next `start` re-arms a fresh subscriber (the previous one
    /// saw a closed channel and exited).
    // Wired to the FE's observer IPC in a follow-up; for now callers go
    // through `ObserverTool`, which calls the supervisor directly.
    #[allow(dead_code)]
    pub fn stop(
        &self,
        session_id: &str,
        id: ObserverId,
    ) -> Result<(), ff_observer::event::ObserverError> {
        self.supervisor.stop(session_id, id)?;
        let list = self.supervisor.list(session_id);
        if list.is_empty() {
            self.armed.lock().unwrap().remove(session_id);
        }
        Ok(())
    }

    /// Forward a list-snapshot to the model.
    // Wired to the FE's observer IPC in a follow-up; for now callers go
    // through `ObserverTool`, which calls the supervisor directly.
    #[allow(dead_code)]
    pub fn list(&self, session_id: &str) -> Vec<ff_observer::event::ObserverInfo> {
        self.supervisor.list(session_id)
    }

    /// Stop and remove every observer for `session_id`, and clear the
    /// host's `armed` flag so a future `start` re-arms a fresh
    /// subscriber task. Called by `AppState::reap_session_observers`
    /// on `delete_session`. Returns the number of observers reaped.
    pub fn reap_session(&self, session_id: &str) -> usize {
        let n = self.supervisor.reap_session(session_id);
        // Drop the broadcast sender first (already done by `reap_session`),
        // then clear the armed flag. The next `start` for this session
        // (if any) will re-arm a fresh subscriber.
        self.armed.lock().unwrap().remove(session_id);
        n
    }

    /// Spawn the per-session subscriber task. The task reads fired events
    /// from the supervisor's broadcast channel and dispatches each one
    /// through `AppState::dispatch_observer_event` (which persists a
    /// synthetic user message and spawns a new assistant turn). On
    /// `reap_session` the supervisor drops the broadcast sender, the
    /// task sees `Closed`, and exits — the `armed` flag is cleared so
    /// the next `start` re-arms.
    fn spawn_subscriber(
        self: &Arc<Self>,
        session_id: String,
        app: AppHandle,
        state: Arc<crate::state::AppState>,
    ) {
        let host = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let mut rx = host.supervisor.subscribe(&session_id);
            loop {
                match rx.recv().await {
                    Ok(event) => state.dispatch_observer_event(&app, &session_id, &event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // A slow subscriber drops events rather than
                        // blocking the producer; the supervisor's
                        // events are best-effort (a fire is a hint, not
                        // a contract). The next event in the queue is
                        // still good.
                        warn!(
                            session_id,
                            lagged = n,
                            "observer subscriber lagged; dropped events"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        // The session was reaped (delete_session or app
                        // exit). Drop our listener slot and exit; the
                        // supervisor garbage-collects the per-session
                        // state on the last unsubscribe.
                        host.supervisor.unsubscribe(&session_id);
                        host.armed.lock().unwrap().remove(&session_id);
                        return;
                    }
                }
            }
        });
    }
}

/// Re-exported so callers don't need a direct dep on `ff_observer`.
// Reserved for the FE's observer IPC in a follow-up.
#[allow(dead_code)]
pub type ObserverInfo = ff_observer::event::ObserverInfo;
