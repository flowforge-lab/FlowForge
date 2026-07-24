//! Session-scoped supervisor of background observers. Mirrors the shape
//! of `ProcessSupervisor` (`crates/ff-tools/src/process.rs:178`): a
//! `HashMap<ObserverId, ManagedObserver>` guarded by a mutex, an
//! `AtomicU64` for id allocation, and a `start / stop / list /
//! reap_session` API that scopes every operation to the caller's
//! session.
//!
//! Two extras the process supervisor doesn't need:
//!
//! - A single shared `mpsc::UnboundedSender<ObserverEvent>` that all
//!   source tasks forward into. The receiver is returned from [`new`]
//!   and owned by the desktop pump.
//! - A per-session `VecDeque<ObserverEvent>` buffer of events that
//!   fired while a turn was in flight. The pump defers to the next
//!   `spawn_assistant_turn` (deferral is the spec choice: dropping
//!   loses signal, interrupting races `cancel_turn`).

use super::cancel::Cancel;
use super::file::FileSource;
use super::http::HttpSource;
use super::process::ProcessSource;
use super::source::{
    ObserverContext, ObserverEvent, ObserverId, ObserverInfo, ObserverKind, ObserverSource,
    ObserverSpec,
};
use ff_tools::process::ProcessSupervisor;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::Notify;

/// Cap on live observers per session. Mirrors
/// `ProcessSupervisor::MAX_CONCURRENT` (8) — same rough budget, same
/// reasoning: 8 file watchers is already an aggressive upper bound for
/// any plausible session.
pub const MAX_PER_SESSION: usize = 8;

struct ManagedObserver {
    info: ObserverInfo,
    session_id: String,
    cancel: Cancel,
    /// Signaled by the source task right before it returns. The
    /// supervisor's `stop` / `reap_session` paths `await` this so
    /// callers know the OS-level resources (kqueue/inotify fd) are
    /// closed before they return. Independent of `cancel` so a
    /// clean source-exit path (e.g. file deleted and watch
    /// invalidated) doesn't need a cancel signal first.
    done: Arc<Notify>,
}

/// A long-lived supervisor for background observers. Cloning is `Arc`
/// deep — every clone shares the same map, channel, and id space.
pub struct ObserverSupervisor {
    observers: Arc<Mutex<HashMap<ObserverId, ManagedObserver>>>,
    next_id: AtomicU64,
    /// Sender side of the wake channel. Every source task clones this
    /// to push events. The receiver is owned by the desktop pump.
    events_tx: UnboundedSender<ObserverEvent>,
    /// Per-session queue of events that arrived while a turn was in
    /// flight. The pump `drain`s on the next idle wake.
    buffer: Mutex<HashMap<String, VecDeque<ObserverEvent>>>,
    /// Phase 3 (#893): handle to the global `ProcessSupervisor` so the
    /// `process` observer kind can subscribe to a running process.
    /// `None` in tests / CLIs that don't manage background processes;
    /// in that case `start` rejects `kind=process` with an actionable
    /// error rather than a confusing "no such process".
    process_supervisor: Option<Arc<ProcessSupervisor>>,
}

impl Default for ObserverSupervisor {
    fn default() -> Self {
        Self::new().0
    }
}

impl ObserverSupervisor {
    /// Construct the supervisor and hand the wake-channel receiver to
    /// the caller. The desktop takes the receiver into a `Mutex<Option<…>>`
    /// on `AppState` and pulls it out once at `start_observer_pump` time.
    pub fn new() -> (Self, UnboundedReceiver<ObserverEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                observers: Arc::new(Mutex::new(HashMap::new())),
                next_id: AtomicU64::new(1),
                events_tx: tx,
                buffer: Mutex::new(HashMap::new()),
                process_supervisor: None,
            },
            rx,
        )
    }

    /// Wire the process supervisor after construction. The host already
    /// holds the same `Arc<ProcessSupervisor>` it passes to
    /// `ProcessManagerTool`, so the observer supervisor borrows it
    /// rather than re-owning the table. Phase 3 (#893).
    pub fn with_process_supervisor(mut self, sup: Arc<ProcessSupervisor>) -> Self {
        self.process_supervisor = Some(sup);
        self
    }

    /// Start an observer owned by `session_id`. Returns the new id, or
    /// an error if the spec is invalid, the per-session cap is reached,
    /// or the target path doesn't resolve to an existing file/dir.
    pub fn start(&self, spec: ObserverSpec, session_id: &str) -> Result<ObserverId, String> {
        let ObserverSpec {
            label,
            kind,
            target,
            filter,
            interval_secs,
            http_mode,
        } = spec;

        // Spec validation. The tool does its own arg validation, but
        // the supervisor is the authoritative gate (the CLI / tests
        // also call it directly).
        if label.trim().is_empty() {
            return Err("observer requires a non-empty `label`".into());
        }
        if target.trim().is_empty() {
            return Err("observer requires a non-empty `target`".into());
        }
        let compiled_filter = match (kind, filter.as_deref()) {
            (ObserverKind::File, Some(pat)) => Some(
                globset::Glob::new(pat)
                    .map_err(|e| format!("invalid filter glob '{pat}': {e}"))?
                    .compile_matcher(),
            ),
            _ => None,
        };

        // Cap check — only counts observers for *this* session. Other
        // sessions' observers don't share the budget.
        {
            let map = self.observers.lock().unwrap();
            let live = map.values().filter(|o| o.session_id == session_id).count();
            if live >= MAX_PER_SESSION {
                return Err(format!(
                    "too many observers for this session (max {MAX_PER_SESSION}); stop one first"
                ));
            }
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let ctx = ObserverContext {
            session_id: session_id.to_string(),
            id,
            label: label.clone(),
        };
        let cancel = Cancel::new();
        let done = Arc::new(Notify::new());

        // Build the source. The factory rejects unimplemented kinds
        // with an actionable error before any fd / network call is opened.
        let source: Box<dyn ObserverSource> = match kind {
            ObserverKind::File => Box::new(
                FileSource::new(ctx, std::path::Path::new(&target), compiled_filter)
                    .map_err(|e| format!("file observer: {e}"))?,
            ),
            ObserverKind::Http => Box::new(
                HttpSource::new(ctx, &target, interval_secs, filter.clone(), http_mode)
                    .map_err(|e| format!("http observer: {e}"))?,
            ),
            ObserverKind::Process => {
                // The spec's `target` is the *string* form of the
                // u64 process id returned by `process_manager start`.
                // Parse it here so a bad target is rejected with a
                // clean error before we touch the process supervisor.
                let pid: u64 = target.trim().parse().map_err(|_| {
                    format!("process observer: target must be a numeric process id, got '{target}'")
                })?;
                let Some(proc_sup) = self.process_supervisor.as_ref() else {
                    return Err(
                        "process observer: no ProcessSupervisor is wired into the ObserverSupervisor"
                            .into(),
                    );
                };
                Box::new(
                    ProcessSource::new(ctx, pid, filter.as_deref(), proc_sup, session_id)
                        .map_err(|e| format!("process observer: {e}"))?,
                )
            }
        };

        let info = ObserverInfo {
            id,
            label: label.clone(),
            kind,
            target: target.clone(),
            started_at: chrono::Utc::now(),
        };

        let tx = self.events_tx.clone();
        let cancel_arc = cancel.as_notify();
        let done_for_task = done.clone();
        let session_id_owned = session_id.to_string();

        // The source task signals `done` exactly once on exit
        // (success or cancel). We don't keep its `JoinHandle` —
        // tokio tasks don't need to be joined, and the
        // `Notify` is the synchronization point `stop` and
        // `reap_session` wait on.
        tokio::spawn(async move {
            run_source(source, cancel_arc, tx, id, session_id_owned).await;
            done_for_task.notify_waiters();
        });

        self.observers.lock().unwrap().insert(
            id,
            ManagedObserver {
                info,
                session_id: session_id.to_string(),
                cancel,
                done,
            },
        );

        Ok(id)
    }

    /// Stop and remove observer `id`. Only the owning session may
    /// stop it — a foreign session sees the same "no such observer"
    /// error as an unknown id, hiding other sessions' work.
    pub async fn stop(&self, id: ObserverId, session_id: &str) -> Result<String, String> {
        let (cancel, done) = {
            let map = self.observers.lock().unwrap();
            let o = map
                .get(&id)
                .ok_or_else(|| format!("no such observer: {id}"))?;
            if o.session_id != session_id {
                return Err(format!("no such observer: {id}"));
            }
            (o.cancel.clone(), o.done.clone())
        };
        cancel.signal();
        // Wait for the source task to exit (it observes the cancel
        // and returns). The Notify is the synchronization point —
        // bounded by a 2 s grace so a misbehaving source that
        // doesn't honor cancel can't block `delete_session`.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), done.notified()).await;
        self.observers.lock().unwrap().remove(&id);
        Ok(format!("observer {id} stopped"))
    }

    /// One line per observer in `session_id`, oldest id first.
    pub fn list(&self, session_id: &str) -> Vec<ObserverInfo> {
        let map = self.observers.lock().unwrap();
        let mut out: Vec<ObserverInfo> = map
            .values()
            .filter(|o| o.session_id == session_id)
            .map(|o| o.info.clone())
            .collect();
        out.sort_by_key(|i| i.id);
        out
    }

    /// Stop and remove every observer owned by `session_id`. Returns
    /// the number reaped. Called by the host on session **delete** so
    /// background observers don't outlive their session.
    pub async fn reap_session(&self, session_id: &str) -> usize {
        // Snapshot the entries first so we can drop the lock before
        // awaiting (Notify::notified() is the wait point, and we don't
        // want to hold the map mutex through it).
        let to_reap: Vec<(ObserverId, Cancel, Arc<Notify>)> = {
            let map = self.observers.lock().unwrap();
            map.iter()
                .filter(|(_, o)| o.session_id == session_id)
                .map(|(id, o)| (*id, o.cancel.clone(), o.done.clone()))
                .collect()
        };
        let mut count = 0;
        for (id, cancel, done) in to_reap {
            cancel.signal();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), done.notified()).await;
            self.observers.lock().unwrap().remove(&id);
            count += 1;
        }
        count
    }

    /// Append `event` to `session_id`'s deferral buffer. Called by the
    /// desktop pump when a turn is already in flight — deferring
    /// (rather than dropping or interrupting) is the spec choice.
    pub fn buffer_event(&self, session_id: &str, event: ObserverEvent) {
        self.buffer
            .lock()
            .unwrap()
            .entry(session_id.to_string())
            .or_default()
            .push_back(event);
    }

    /// Take every buffered event for `session_id`, leaving the buffer
    /// empty. The pump calls this on the next idle wake to fold the
    /// deferred events into the upcoming turn.
    pub fn drain_buffer(&self, session_id: &str) -> Vec<ObserverEvent> {
        self.buffer
            .lock()
            .unwrap()
            .remove(session_id)
            .map(|q| q.into_iter().collect())
            .unwrap_or_default()
    }

    /// Whether `session_id` has any buffered (deferred) events awaiting a
    /// drain. Read-only — does not clear. The pump checks this when a turn
    /// completes to decide whether to spawn a drain turn (#1095).
    pub fn has_buffered(&self, session_id: &str) -> bool {
        self.buffer
            .lock()
            .unwrap()
            .get(session_id)
            .is_some_and(|q| !q.is_empty())
    }
}

impl Drop for ObserverSupervisor {
    fn drop(&mut self) {
        // The last clone of the supervisor going away is the natural
        // shutdown point. Signal every cancel so the source tasks
        // unwind and their fds close. We can't `await` here, but
        // dropping the `JoinHandle` is fine — `tokio::spawn`ed tasks
        // continue to run until they exit on their own; the OS fd
        // (`kqueue` / `inotify`) is the real resource, and that's
        // owned by the source task, not the supervisor.
        if let Ok(map) = self.observers.lock() {
            for o in map.values() {
                o.cancel.signal();
            }
        }
    }
}

/// The per-source event loop. Pulls events from `source` and forwards
/// them through `tx` until the source returns `None` (terminated) or
/// `cancel` is signaled. On exit the task ends; the reaper task in
/// `start` removes the map entry.
async fn run_source(
    mut source: Box<dyn ObserverSource>,
    cancel: Arc<tokio::sync::Notify>,
    tx: UnboundedSender<ObserverEvent>,
    id: ObserverId,
    session_id: String,
) {
    loop {
        let next = source.next_event(cancel.clone()).await;
        let Some(mut ev) = next else {
            return;
        };
        // Defensive: stamp the session_id in case the source forgot.
        // (All built-in sources stamp it at construction via
        // `ObserverContext`, but a future Phase 2/3 source could
        // re-emit a stale event; this keeps routing correct.)
        if ev.session_id.is_empty() {
            ev.session_id = session_id.clone();
        }
        if ev.id == 0 {
            ev.id = id;
        }
        if tx.send(ev).is_err() {
            // The pump dropped its receiver. The app is shutting down
            // or the supervisor was dropped — nothing to do but exit.
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::HttpMode;
    use std::path::PathBuf;
    fn tempdir_target() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().to_string_lossy().into_owned();
        (dir, target)
    }

    fn spec(label: &str, target: &str) -> ObserverSpec {
        ObserverSpec {
            label: label.to_string(),
            kind: ObserverKind::File,
            target: target.to_string(),
            filter: None,
            interval_secs: None,
            http_mode: HttpMode::Change,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_rejects_past_cap() {
        let (sup, _rx) = ObserverSupervisor::new();
        let (_dir, target) = tempdir_target();
        for i in 0..MAX_PER_SESSION {
            sup.start(spec(&format!("watch-{i}"), &target), "s1")
                .expect("start within cap");
        }
        let err = sup
            .start(spec("watch-overflow", &target), "s1")
            .expect_err("start past cap must error");
        assert!(err.contains("too many"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reap_session_stops_and_removes() {
        let (sup, _rx) = ObserverSupervisor::new();
        let (_dir1, target1) = tempdir_target();
        let (_dir2, target2) = tempdir_target();
        sup.start(spec("a", &target1), "s1").unwrap();
        sup.start(spec("b", &target1), "s1").unwrap();
        sup.start(spec("c", &target2), "s2").unwrap();
        assert_eq!(sup.list("s1").len(), 2);
        assert_eq!(sup.list("s2").len(), 1);

        let n = sup.reap_session("s1").await;
        assert_eq!(n, 2);
        // list eventually drops to 0 once the reaper tasks finish.
        // They run asynchronously after the source tasks exit; give
        // them a moment to settle.
        for _ in 0..50 {
            if sup.list("s1").is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(sup.list("s1").is_empty());
        assert_eq!(sup.list("s2").len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cross_session_isolation() {
        let (sup, _rx) = ObserverSupervisor::new();
        let (_dir, target) = tempdir_target();
        let id = sup.start(spec("a", &target), "session-a").unwrap();
        // Session B cannot see, list, or stop session A's observer.
        assert!(sup.stop(id, "session-b").await.is_err());
        assert!(sup.list("session-b").is_empty());
        // Session A still has full access.
        let infos = sup.list("session-a");
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].id, id);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn buffer_event_drain_round_trip() {
        let (sup, _rx) = ObserverSupervisor::new();
        let ev = ObserverEvent {
            session_id: "s1".into(),
            id: 42,
            label: "watcher".into(),
            summary: "modified foo".into(),
        };
        sup.buffer_event("s1", ev.clone());
        assert!(sup.has_buffered("s1"), "has_buffered true after first push");
        sup.buffer_event(
            "s1",
            ObserverEvent {
                session_id: "s1".into(),
                id: 43,
                label: "watcher".into(),
                summary: "modified bar".into(),
            },
        );
        let drained = sup.drain_buffer("s1");
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].id, 42);
        assert_eq!(drained[1].id, 43);
        // Subsequent drain is empty.
        assert!(sup.drain_buffer("s1").is_empty());
        // ...and has_buffered reflects the now-empty buffer (#1095).
        assert!(!sup.has_buffered("s1"), "has_buffered false after drain");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn has_buffered_false_for_unknown_or_empty_session() {
        let (sup, _rx) = ObserverSupervisor::new();
        // Never-seen session: no entry at all.
        assert!(!sup.has_buffered("nobody"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_observer_id_is_an_error() {
        let (sup, _rx) = ObserverSupervisor::new();
        assert!(sup.stop(999, "s1").await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_rejects_empty_label_and_target() {
        let (sup, _rx) = ObserverSupervisor::new();
        let (_dir, target) = tempdir_target();
        assert!(sup
            .start(
                ObserverSpec {
                    label: "".into(),
                    kind: ObserverKind::File,
                    target,
                    filter: None,
                    interval_secs: None,
                    http_mode: HttpMode::Change,
                },
                "s1"
            )
            .is_err());
        let (_dir, _target) = tempdir_target();
        assert!(sup
            .start(
                ObserverSpec {
                    label: "ok".into(),
                    kind: ObserverKind::File,
                    target: "".into(),
                    filter: None,
                    interval_secs: None,
                    http_mode: HttpMode::Change,
                },
                "s1"
            )
            .is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_accepts_http_kind_with_valid_url() {
        // Phase 2: constructing an http source no longer fails. The
        // constructor only parses the URL — no network is opened, so
        // any well-formed URL is enough to assert the supervisor's
        // factory path no longer rejects.
        let (sup, _rx) = ObserverSupervisor::new();
        let id = sup
            .start(
                ObserverSpec {
                    label: "x".into(),
                    kind: ObserverKind::Http,
                    target: "https://example.com/".into(),
                    filter: None,
                    interval_secs: None,
                    http_mode: HttpMode::Change,
                },
                "s1",
            )
            .expect("http start accepts a parseable URL");
        // The source is running; stop it to keep the test self-contained.
        sup.stop(id, "s1").await.expect("stop");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_rejects_http_kind_with_invalid_url() {
        let (sup, _rx) = ObserverSupervisor::new();
        let err = sup
            .start(
                ObserverSpec {
                    label: "x".into(),
                    kind: ObserverKind::Http,
                    target: "not a url".into(),
                    filter: None,
                    interval_secs: None,
                    http_mode: HttpMode::Change,
                },
                "s1",
            )
            .expect_err("malformed URL must error");
        assert!(err.to_lowercase().contains("http"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_rejects_process_kind_without_supervisor() {
        // Phase 3 (#893): with no ProcessSupervisor wired, the
        // supervisor can't subscribe to a process — reject up front
        // with an actionable error instead of a confusing
        // "no such process".
        let (sup, _rx) = ObserverSupervisor::new();
        let err = sup
            .start(
                ObserverSpec {
                    label: "x".into(),
                    kind: ObserverKind::Process,
                    target: "1".into(),
                    filter: None,
                    interval_secs: None,
                    http_mode: HttpMode::Change,
                },
                "s1",
            )
            .expect_err("process kind must error without supervisor");
        assert!(err.to_lowercase().contains("process"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_rejects_process_kind_with_unknown_pid() {
        // Phase 3: with a ProcessSupervisor wired but the pid
        // unknown to it, the source's `is_alive` check returns false
        // and the spec is rejected with the same wording class as
        // `process_manager poll` to keep cross-session ids hidden.
        let dir = tempfile::tempdir().unwrap();
        let proc_sup = Arc::new(ProcessSupervisor::new());
        let (observer_supervisor, _rx) = ObserverSupervisor::new();
        let observer_supervisor =
            Arc::new(observer_supervisor.with_process_supervisor(proc_sup.clone()));
        let err = observer_supervisor
            .start(
                ObserverSpec {
                    label: "x".into(),
                    kind: ObserverKind::Process,
                    target: "999".into(),
                    filter: None,
                    interval_secs: None,
                    http_mode: HttpMode::Change,
                },
                "s1",
            )
            .expect_err("unknown pid must error");
        assert!(err.contains("no such process"), "{err}");
        let _ = dir;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_rejects_nonexistent_path() {
        let (sup, _rx) = ObserverSupervisor::new();
        let bogus: PathBuf = tempfile::tempdir().unwrap().path().join("does-not-exist");
        let err = sup
            .start(
                ObserverSpec {
                    label: "x".into(),
                    kind: ObserverKind::File,
                    target: bogus.to_string_lossy().into_owned(),
                    filter: None,
                    interval_secs: None,
                    http_mode: HttpMode::Change,
                },
                "s1",
            )
            .expect_err("missing path must error");
        assert!(
            err.to_lowercase().contains("does not exist")
                || err.to_lowercase().contains("not found")
                || err.to_lowercase().contains("no such"),
            "{err}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_then_list_includes_label() {
        // End-to-end smoke: spec fields survive the round trip
        // through `start` and back through `list` (labels are the
        // user-visible handle; the model uses them in wake text).
        let (sup, _rx) = ObserverSupervisor::new();
        let (_dir, target) = tempdir_target();
        let id = sup
            .start(
                ObserverSpec {
                    label: "build-output".into(),
                    kind: ObserverKind::File,
                    target,
                    filter: None,
                    interval_secs: None,
                    http_mode: HttpMode::Change,
                },
                "s1",
            )
            .expect("start");
        let infos = sup.list("s1");
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].id, id);
        assert_eq!(infos[0].label, "build-output");
    }
}
