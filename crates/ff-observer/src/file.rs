//! `FileSource` — kqueue (macOS) / inotify (Linux), trailing 500ms debounce.
//! No `notify` crate: the issue (#709) is explicit about using direct OS
//! primitives for minimal deps and full control.

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use regex::Regex;
use tokio::sync::mpsc as tokio_mpsc;
use tokio_util::sync::CancellationToken;

use crate::event::{ObserverError, ObserverEvent, ObserverSpec};
use crate::source::{split_target_path, ObserverSource};

/// Trailing debounce: a save-storm or rapid-fire rewrite coalesces into one
/// notification after the storm settles. Matches the existing
/// `dev_update_watcher.rs` / `git_watch.rs` pattern.
pub const DEBOUNCE: Duration = Duration::from_millis(500);

/// Wake-up interval the platform threads use to check `stop_rx`. Keeps the
/// thread dormant (no busy-spin) during silence while bounding how long it
/// takes to observe `stop_tx.send(())` or `stop_tx` being dropped. Matches
/// the macOS kqueue poll cadence so both platforms have equivalent stop
/// latency.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The platform threads park on a sync-channel receiver to observe stop
/// signals, but `try_recv().is_ok()` is `false` when the sender is *dropped*
/// without sending (it returns `Err(Disconnected)`). Without recognising that
/// case, dropping the source would leave the worker thread running forever —
/// the inotify fd + watch would outlive the session. This treats both an
/// explicit unit and a sender-drop as "stop now".
fn disconnected_is_stop(r: Result<(), TryRecvError>) -> bool {
    matches!(r, Ok(()) | Err(TryRecvError::Disconnected))
}

#[cfg(test)]
mod disconnect_tests {
    use super::*;
    use std::sync::mpsc;

    /// Pin down the stop-signal contract: an explicit send *or* a sender drop
    /// are both stop signals. Without `Disconnected` being treated as a
    /// stop signal, `FileSource::drop` would leak the platform thread.
    #[test]
    fn disconnected_counts_as_stop() {
        let (tx, rx) = mpsc::channel::<()>();
        // Sender still alive, nothing sent: not a stop.
        assert!(!disconnected_is_stop(rx.try_recv()));
        drop(tx);
        // Sender dropped: this is the case the bug missed.
        assert!(disconnected_is_stop(rx.try_recv()));
    }

    #[test]
    fn explicit_unit_counts_as_stop() {
        let (tx, rx) = mpsc::channel::<()>();
        tx.send(()).unwrap();
        assert!(disconnected_is_stop(rx.try_recv()));
        assert!(!disconnected_is_stop(rx.try_recv())); // drained
    }
}

/// Compile-time platform tag: the matching `spawn_thread` is selected via
/// `#[cfg]` so non-Linux/macOS builds fail to compile with a clear message
/// instead of silently losing the feature.
#[cfg(all(not(target_os = "macos"), not(target_os = "linux")))]
compile_error!("ff-observer FileSource currently supports macOS (kqueue) and Linux (inotify) only");

#[derive(Debug)]
pub struct FileSource {
    /// Directory we attach the OS watch to.
    dir: PathBuf,
    /// Basename filter, if the user pointed at a single file. `None` for
    /// directory-wide watches.
    name_filter: Option<String>,
    /// Optional regex applied to matched basenames.
    filter_regex: Option<Regex>,
    /// Human-readable key for events: the file basename for single-file
    /// watches, the directory path for directory watches.
    key: String,
    /// Background thread that owns the OS watcher and forwards fired
    /// events to the async side. `None` until the first `next_event` call.
    watcher: Option<FileWatcherThread>,
}

/// Background thread that owns the OS watcher for the source's lifetime.
/// Dropping the struct (and the `stop_tx` sender it owns) signals the
/// thread to exit; the OS handle is torn down in the thread's own drop.
#[derive(Debug)]
struct FileWatcherThread {
    /// Sending half of the stop signal. Drop = thread exits.
    _stop_tx: std_mpsc::Sender<()>,
    /// Receiving half of the event stream. `next_event` selects on this.
    events_rx: tokio_mpsc::UnboundedReceiver<String>,
}

impl FileSource {
    /// Parse the spec and build the source. Validates the target exists and
    /// that any filter compiles. The OS watch is *not* opened here — that
    /// happens inside `next_event` so the source can be created in tests
    /// that don't need the runtime yet.
    pub async fn from_spec(spec: ObserverSpec) -> Result<Self, ObserverError> {
        let (dir, name) = split_target_path(&spec.target)?;
        let key = name
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| dir.clone())
            .to_string_lossy()
            .into_owned();
        let filter_regex = spec
            .filter
            .as_deref()
            .map(Regex::new)
            .transpose()
            .map_err(|e| ObserverError::InvalidFilter(e.to_string()))?;
        Ok(Self {
            dir,
            name_filter: name,
            filter_regex,
            key,
            watcher: None,
        })
    }

    /// Whether the basename `name` should fire given the configured filters.
    fn matches(&self, name: &str) -> bool {
        if let Some(stem) = &self.name_filter {
            if name != stem {
                return false;
            }
        }
        match &self.filter_regex {
            Some(re) => re.is_match(name),
            None => true,
        }
    }

    /// Build a human-readable summary for a fired event.
    fn summary_for(&self, name: &str) -> String {
        format!("file changed: {name}")
    }

    /// Lazily spawn the blocking watcher thread. Idempotent. Spawning a real
    /// OS thread is the only way to own the blocking kqueue/inotify handle
    /// for the lifetime of the source — the async side just reads events
    /// from `events_rx`.
    fn ensure_thread(&mut self) -> Result<(), ObserverError> {
        if self.watcher.is_some() {
            return Ok(());
        }
        let (events_tx, events_rx) = tokio_mpsc::unbounded_channel::<String>();
        let (stop_tx, stop_rx) = std_mpsc::channel::<()>();
        let join = platform::spawn_thread(
            self.dir.clone(),
            self.name_filter.clone(),
            events_tx,
            stop_rx,
        )?;
        self.watcher = Some(FileWatcherThread {
            _stop_tx: stop_tx,
            events_rx,
        });
        // Detach the thread: stopping is by dropping `stop_tx` (i.e. dropping
        // the source), and the join result is uninteresting.
        drop(join);
        Ok(())
    }
}

/// Trailing-edge debouncer shared by the per-platform blocking watcher threads.
/// Lives on the OS thread; no async runtime involvement.
///
/// On every raw filtered event the platform thread calls [`feed`]: if a window
/// is already open and still within `DEBOUNCE` of the previous event, the
/// window is extended with the new name (the new event is consumed but
/// nothing is emitted). If the previous window's `DEBOUNCE` has elapsed, that
/// window flushes first (one coalesced name forward to the async side), then
/// a fresh window opens with the new event.
///
/// The platform thread also calls [`tick`] from its poll/wait-timeout branch
/// so a window that *opened* but never got a follow-up event still emits
/// after the trailing silence — the classic "editor finished saving" case.
///
/// Returns `false` from emit → callers exit their loop (host dropped the
/// source, the channel is gone).
///
/// Pattern mirrors `apps/desktop/src-tauri/src/git_watch.rs::DEBOUNCE`.
struct Debouncer {
    last_at: Option<Instant>,
    /// Name to flush at the trailing edge of the open window. Undefined while
    /// `last_at` is `None`.
    last_name: String,
}

impl Debouncer {
    fn new() -> Self {
        Self {
            last_at: None,
            last_name: String::new(),
        }
    }

    /// Record `name` as the latest raw event at `now`. Flushes any prior
    /// open window whose trailing silence has expired (via `emit`) before
    /// opening/overwriting the new one. Returns `false` if `emit` failed (the
    /// channel was closed and the caller should exit).
    fn feed(&mut self, name: String, now: Instant, emit: &mut dyn FnMut(String) -> bool) -> bool {
        if let Some(t) = self.last_at {
            if now.duration_since(t) >= DEBOUNCE {
                let prior = std::mem::take(&mut self.last_name);
                self.last_at = None;
                if !emit(prior) {
                    return false;
                }
            }
        }
        self.last_name = name;
        self.last_at = Some(now);
        true
    }

    /// Flush any open window whose `DEBOUNCE` has elapsed at `now` (the caller
    /// invokes this on every poll/wait timeout so a window that opens without
    /// a follow-up event still emits after the trailing silence). Returns
    /// `false` if `emit` failed and the thread should exit.
    fn tick(&mut self, now: Instant, emit: &mut dyn FnMut(String) -> bool) -> bool {
        if let Some(t) = self.last_at {
            if now.duration_since(t) >= DEBOUNCE {
                let prior = std::mem::take(&mut self.last_name);
                self.last_at = None;
                return emit(prior);
            }
        }
        true
    }
}

impl Drop for FileSource {
    fn drop(&mut self) {
        // Dropping `_stop_tx` (via `self.watcher = None`) signals the
        // background thread to exit. The thread holds the OS handle and
        // tears it down on its way out.
        self.watcher = None;
    }
}

#[async_trait]
impl ObserverSource for FileSource {
    fn key(&self) -> &str {
        &self.key
    }

    async fn next_event(
        &mut self,
        id: crate::event::ObserverId,
        cancel: &CancellationToken,
    ) -> Result<Option<ObserverEvent>, ObserverError> {
        self.ensure_thread()?;
        let watcher = self.watcher.as_mut().expect("ensure_thread set it");
        // Read the next event from the OS watcher. A `None` (thread
        // closed its sender) is treated as `Ok(None)` so the supervisor
        // drops the observer.
        let name = match tokio::select! {
            _ = cancel.cancelled() => return Ok(None),
            maybe = watcher.events_rx.recv() => maybe,
        } {
            Some(name) => name,
            None => return Ok(None),
        };
        if !self.matches(&name) {
            // Filtered out — wait for the next event. Box the recursive call
            // so a busy directory + strict regex doesn't blow the stack.
            return Box::pin(self.next_event(id, cancel)).await;
        }
        Ok(Some(ObserverEvent {
            id,
            key: self.key.clone(),
            summary: self.summary_for(&name),
            occurred_at: Utc::now(),
        }))
    }
}

// ---- platform dispatch ----------------------------------------------------

/// Spawn a blocking thread that owns the OS watcher for `dir`. The thread
/// forwards fired event names to `events_tx` and exits when `stop_rx` is
/// disconnected (the source was dropped).
#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use kqueue::{EventFilter, FilterFlag};
    use std::thread::JoinHandle;
    use tracing::warn;

    pub(super) fn spawn_thread(
        dir: PathBuf,
        name_filter: Option<String>,
        events_tx: tokio_mpsc::UnboundedSender<String>,
        stop_rx: std_mpsc::Receiver<()>,
    ) -> Result<JoinHandle<()>, ObserverError> {
        let dir_str = dir.to_string_lossy().into_owned();
        Ok(std::thread::spawn(move || {
            let (watch_path, send_name) = match name_filter {
                Some(name) => (dir.join(&name), name),
                None => (
                    dir.clone(),
                    dir.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "watched".to_string()),
                ),
            };
            let watch_path_str = watch_path.to_string_lossy().into_owned();
            let mut watcher = match kqueue::Watcher::new() {
                Ok(w) => w,
                Err(e) => {
                    warn!(dir = %dir_str, "kqueue init failed: {}", e);
                    return;
                }
            };
            if let Err(e) = watcher.add_filename(
                &watch_path,
                EventFilter::EVFILT_VNODE,
                FilterFlag::NOTE_WRITE
                    | FilterFlag::NOTE_DELETE
                    | FilterFlag::NOTE_RENAME
                    | FilterFlag::NOTE_EXTEND
                    | FilterFlag::NOTE_ATTRIB,
            ) {
                warn!(path = %watch_path_str, "kqueue add_filename failed: {}", e);
                return;
            }
            let mut debouncer = Debouncer::new();
            // Closed only by the OS-side — `events_tx` (host dropped).
            let mut emit = |name: String| events_tx.send(name).is_ok();
            loop {
                // Recognises both an explicit stop send and the sender being
                // dropped (Disconnected); see `disconnected_is_stop`.
                if disconnected_is_stop(stop_rx.try_recv()) {
                    return;
                }
                // Close any window whose trailing silence has elapsed before
                // we block on the next kevent — otherwise a single isolated
                // write wouldn't fire until a follow-up event opened a new
                // window.
                if !debouncer.tick(Instant::now(), &mut emit) {
                    return;
                }
                if watcher.watch().is_err() {
                    warn!(path = %watch_path_str, "kqueue watch() failed");
                    return;
                }
                match watcher.poll(Some(POLL_INTERVAL)) {
                    Some(ev) if ev.is_err() => {
                        warn!(path = %watch_path_str, "kqueue error event");
                        return;
                    }
                    Some(_event)
                        if !debouncer.feed(send_name.clone(), Instant::now(), &mut emit) =>
                    {
                        return;
                    }
                    Some(_) => {}
                    None => {} // timeout — loop, tick debouncer, recheck stop
                }
            }
        }))
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use inotify::{EventMask, Inotify, WatchMask};
    use std::thread::JoinHandle;

    pub(super) fn spawn_thread(
        dir: PathBuf,
        _name_filter: Option<String>,
        events_tx: tokio_mpsc::UnboundedSender<String>,
        stop_rx: std_mpsc::Receiver<()>,
    ) -> Result<JoinHandle<()>, ObserverError> {
        let mut inotify = Inotify::init().map_err(ObserverError::Io)?;
        inotify
            .watches()
            .add(
                &dir,
                WatchMask::MODIFY
                    | WatchMask::CREATE
                    | WatchMask::DELETE
                    | WatchMask::MOVE
                    | WatchMask::CLOSE_WRITE,
            )
            .map_err(ObserverError::Io)?;

        Ok(std::thread::spawn(move || {
            let mut debouncer = Debouncer::new();
            // Closed only by the host dropping the source.
            let mut emit = |name: String| events_tx.send(name).is_ok();
            let mut buf = [0u8; 4096];
            loop {
                // Recognises both an explicit stop send and the sender being
                // dropped (Disconnected); see `disconnected_is_stop`. Without
                // this, the worker wouldn't exit on `FileSource` drop, so the
                // inotify fd + watch would outlive the session.
                if disconnected_is_stop(stop_rx.try_recv()) {
                    return;
                }
                // Drain any window whose trailing silence has elapsed before
                // we block on the next inotify read — without this, a single
                // isolated write wouldn't fire until a follow-up event opened
                // a new window.
                if !debouncer.tick(Instant::now(), &mut emit) {
                    return;
                }
                let mut events = match inotify.read_events(&mut buf) {
                    Ok(events) => events,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // `Inotify::init()` already sets IN_NONBLOCK, so this
                        // arm fires whenever the kernel has nothing for us.
                        // Sleep briefly to avoid a busy-spin; the wakeup
                        // cadence matches the macOS kqueue poll.
                        std::thread::sleep(POLL_INTERVAL);
                        continue;
                    }
                    Err(_) => return,
                };
                for ev in events.by_ref() {
                    let interesting = ev.mask.intersects(
                        EventMask::MODIFY
                            | EventMask::CREATE
                            | EventMask::DELETE
                            | EventMask::MOVED_TO
                            | EventMask::MOVED_FROM
                            | EventMask::CLOSE_WRITE,
                    );
                    if !interesting {
                        continue;
                    }
                    let name = ev
                        .name
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if !debouncer.feed(name, Instant::now(), &mut emit) {
                        return;
                    }
                }
            }
        }))
    }
}

#[cfg(all(not(target_os = "macos"), not(target_os = "linux")))]
mod platform {
    use super::*;
    pub(super) fn spawn_thread(
        _dir: PathBuf,
        _events_tx: tokio_mpsc::UnboundedSender<String>,
        _stop_rx: std_mpsc::Receiver<()>,
    ) -> Result<std::thread::JoinHandle<()>, ObserverError> {
        Err(ObserverError::Other(
            "ff-observer FileSource: unsupported platform".into(),
        ))
    }
}
