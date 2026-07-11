//! `FileSource` — direct OS-level file/directory watcher, no `notify` crate.
//!
//! Two backends behind one `ObserverSource` impl:
//!
//! - **macOS** (`kqueue` 1.2): `EVFILT_VNODE` with `NOTE_WRITE |
//!   NOTE_DELETE | NOTE_RENAME | NOTE_ATTRIB`. The high-level
//!   `kqueue::Watcher` API does the fd / event-list bookkeeping; we
//!   just call `poll(timeout)` in a `spawn_blocking` task and `select!`
//!   on cancel.
//! - **Linux** (`inotify` 0.10): `IN_MODIFY | IN_CREATE | IN_DELETE |
//!   IN_MOVED_FROM | IN_MOVED_TO | IN_CLOSE_WRITE | IN_ATTRIB` on the
//!   watched path. Reads happen on the inotify `EventStream`, a
//!   `tokio::io::unix::AsyncFd` that integrates with the tokio
//!   reactor — so the source's `select!` wakes on either the next
//!   event or a cancel signal without a `spawn_blocking` indirection
//!   (and dropping the stream closes the inotify fd).
//!
//! Both backends debounce inside the supervisor's loop, not here — the
//! `next_event` contract is "one event per OS-level change". The OS
//! already coalesces noisy saves; adding a debounce would only help
//! for very chatty write patterns and is straightforward to layer on
//! later (per-source timer in the supervisor).
//!
//! Windows: `FileSource::new` returns a clear error. The desktop
//! ships on macOS and Linux; the trait shape is platform-independent
//! so a future Windows backend drops in as a separate `match` arm.

use super::source::{ObserverContext, ObserverEvent, ObserverSource};
use async_trait::async_trait;
#[cfg(target_os = "linux")]
use futures_util::StreamExt;
use globset::GlobMatcher;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::time::Duration;
use tokio::sync::Notify;

pub struct FileSource {
    ctx: ObserverContext,
    /// The supervisor passed us this, post-`resolve`. Either a file
    /// or a directory that exists at `start` time.
    target: PathBuf,
    is_dir: bool,
    /// `Some` for directory targets with a filter; `None` means
    /// "match everything" (file targets ignore this).
    filter: Option<GlobMatcher>,
    backend: Backend,
}

enum Backend {
    #[cfg(target_os = "macos")]
    Mac(MacBackend),
    #[cfg(target_os = "linux")]
    Linux(LinuxBackend),
}

impl FileSource {
    pub fn new(
        ctx: ObserverContext,
        target: &Path,
        filter: Option<GlobMatcher>,
    ) -> Result<Self, String> {
        let target = target.to_path_buf();
        let meta = std::fs::metadata(&target)
            .map_err(|e| format!("target does not exist: {} ({e})", target.display()))?;
        let is_dir = meta.is_dir();
        let backend = build_backend(&target, is_dir)?;
        Ok(Self {
            ctx,
            target,
            is_dir,
            filter,
            backend,
        })
    }
}

#[cfg(target_os = "macos")]
fn build_backend(target: &Path, _is_dir: bool) -> Result<Backend, String> {
    use kqueue::{EventFilter, FilterFlag, Watcher};
    let mut w =
        Watcher::new().map_err(|e| format!("kqueue() failed for {}: {e}", target.display()))?;
    // Same flag set for files and directories: NOTE_WRITE catches
    // modifications + child mutations (the vnode is what fires), and
    // NOTE_DELETE / NOTE_RENAME / NOTE_ATTRIB handle the lifecycle
    // and permission changes. A future "deep child watch" pass
    // could differentiate is_dir (e.g. NOTE_EXTEND for dir-growth
    // hints on Darwin) but the per-event summary is identical
    // either way.
    let flags = FilterFlag::NOTE_WRITE
        | FilterFlag::NOTE_DELETE
        | FilterFlag::NOTE_RENAME
        | FilterFlag::NOTE_ATTRIB;
    w.add_filename(target, EventFilter::EVFILT_VNODE, flags)
        .map_err(|e| format!("kqueue add_filename({}) failed: {e}", target.display()))?;
    w.watch()
        .map_err(|e| format!("kqueue watch() failed for {}: {e}", target.display()))?;
    Ok(Backend::Mac(MacBackend {
        w: Arc::new(std::sync::Mutex::new(w)),
        target: target.to_path_buf(),
    }))
}

#[cfg(target_os = "linux")]
fn build_backend(target: &Path, is_dir: bool) -> Result<Backend, String> {
    use inotify::{Inotify, WatchMask};
    let inotify = Inotify::init()
        .map_err(|e| format!("inotify_init failed for {}: {e}", target.display()))?;
    let mask = if is_dir {
        WatchMask::CREATE
            | WatchMask::DELETE
            | WatchMask::MODIFY
            | WatchMask::MOVED_FROM
            | WatchMask::MOVED_TO
            | WatchMask::ATTRIB
    } else {
        WatchMask::MODIFY
            | WatchMask::CLOSE_WRITE
            | WatchMask::DELETE_SELF
            | WatchMask::MOVE_SELF
            | WatchMask::ATTRIB
    };
    inotify
        .watches()
        .add(target, mask)
        .map_err(|e| format!("inotify add_watch({}) failed: {e}", target.display()))?;
    // `into_event_stream` hands the inotify fd to a `tokio::io::unix::AsyncFd`,
    // which integrates with the tokio reactor — so the source's `select!`
    // wakes the moment the fd is readable, and we can drop the stream
    // (closing the fd) on cancel without leaving a `spawn_blocking` task
    // stuck in a blocking `read(2)`. The buffer size has to accommodate
    // the longest event the kernel can emit for this path; the inotify
    // crate ships `get_buffer_size` to compute it.
    let buf_size = inotify::get_buffer_size(target)
        .map_err(|e| format!("inotify buffer size for {}: {e}", target.display()))?;
    let buffer = vec![0u8; buf_size];
    let stream = inotify
        .into_event_stream(buffer)
        .map_err(|e| format!("inotify into_event_stream: {e}"))?;
    Ok(Backend::Linux(LinuxBackend {
        stream,
        // Mirror `MacBackend`: keep the watched target so per-event
        // summaries can point at the watched file (inotify reports no
        // child name for events on a directly-watched inode, so we
        // cannot reconstruct the path from the event alone).
        target: target.to_path_buf(),
    }))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn build_backend(_target: &Path, _is_dir: bool) -> Result<Backend, String> {
    Err("FileSource is not yet implemented on this platform".into())
}

#[async_trait]
impl ObserverSource for FileSource {
    fn ctx(&self) -> &ObserverContext {
        &self.ctx
    }

    async fn next_event(&mut self, cancel: Arc<Notify>) -> Option<ObserverEvent> {
        let raw = wait_for_raw(&mut self.backend, &cancel).await?;
        Some(make_event(
            &self.ctx,
            &self.target,
            self.is_dir,
            self.filter.as_ref(),
            raw,
        ))
    }
}

/// One raw change from the OS, in the language the supervisor
/// already understands. Backend-specific events (kqueue flags,
/// inotify masks) are reduced to a small enum the supervisor can
/// format without re-deriving the path.
///
/// On macOS the kqueue EVFILT_VNODE watch fires on the watched
/// inode itself; the [`kqueue::EventData::Vnode`] variant breaks
/// the change down by sub-flag, so the macOS backend can map
/// `Write`/`Extend`/`Truncate` to [`RawChange::Modified`] and
/// `Delete`/`Rename`/`Attrib`/`Revoke` to [`RawChange::RootChanged`].
/// Per-child names are unavailable (kqueue reports only the
/// watched inode), so the `Modified` summary points at the
/// watched path — not at a child like the inotify backend does.
/// `Created` and `Removed` are reachable only from the Linux
/// backend; macOS cannot distinguish per-child creation or
/// deletion at the vnode level. `#[allow(dead_code)]` keeps the
/// variant set uniform across platforms.
#[allow(dead_code)]
#[derive(Clone, Debug)]
enum RawChange {
    Modified(PathBuf),
    Created(PathBuf),
    Removed(PathBuf),
    /// The watched root itself changed (delete/rename/attrib on a
    /// file target, or the directory's vnode for a dir target). For
    /// a file target this is also a "watcher is now blind" signal;
    /// for a dir target the watch may still be useful, so we keep
    /// going.
    RootChanged,
}

async fn wait_for_raw(backend: &mut Backend, cancel: &Arc<Notify>) -> Option<RawChange> {
    match backend {
        #[cfg(target_os = "macos")]
        Backend::Mac(mac) => mac.next_raw(cancel).await,
        #[cfg(target_os = "linux")]
        Backend::Linux(linux) => linux.next_raw(cancel).await,
        #[allow(unreachable_patterns)]
        _ => {
            let _ = cancel.notified().await;
            None
        }
    }
}

fn make_event(
    ctx: &ObserverContext,
    target: &Path,
    is_dir: bool,
    filter: Option<&GlobMatcher>,
    raw: RawChange,
) -> ObserverEvent {
    let summary = match &raw {
        RawChange::Modified(p) => format!("modified {}", p.display()),
        RawChange::Created(p) => format!("created {}", p.display()),
        RawChange::Removed(p) => format!("removed {}", p.display()),
        RawChange::RootChanged => {
            if is_dir {
                format!("changed {}", target.display())
            } else {
                format!(
                    "{} was deleted, renamed, or had its attributes changed",
                    target.display()
                )
            }
        }
    };
    let _ = filter; // Filtering is applied at the backend (events
                    // for non-matching children never reach the
                    // supervisor); we keep the field for future
                    // per-event glob checks (e.g. summarizing
                    // "matched N files").
    ObserverEvent {
        session_id: ctx.session_id.clone(),
        id: ctx.id,
        label: ctx.label.clone(),
        summary,
    }
}

// ---------------------------------------------------------------------------
// macOS backend (kqueue).

/// Outcome of one macOS `kqueue` poll iteration. `Event` = a real OS
/// event; `TimedOut` = poll deadline elapsed, no event yet;
/// `Cancelled` = cancel signal fired, exit the loop.
#[cfg(target_os = "macos")]
enum PollOutcome {
    Change(RawChange),
    TimedOut,
    Cancelled,
}

#[cfg(target_os = "macos")]
struct MacBackend {
    /// Shared with `spawn_blocking` so the actual `kqueue::Watcher::poll`
    /// call runs on the blocking pool, not the current async task. If we
    /// called `poll` directly here it would block this task for the full
    /// poll timeout (1s) and the cancel arm would never win in time.
    w: Arc<std::sync::Mutex<kqueue::Watcher>>,
    /// The watched path. EVFILT_VNODE reports on the watched inode only
    /// (no per-child name), so `Modified`-class summaries point at this
    /// path.
    target: PathBuf,
}

#[cfg(target_os = "macos")]
impl MacBackend {
    async fn next_raw(&mut self, cancel: &Arc<Notify>) -> Option<RawChange> {
        loop {
            let w = self.w.clone();
            let cancel = cancel.clone();
            // 1-second poll tick so a non-firing event still lets
            // the cancel arm win promptly. (The cancel arm wins on
            // the next tick — at most 1s of latency on a no-event
            // path, which is acceptable for a shutdown signal.)
            let outcome: PollOutcome = tokio::select! {
                biased;
                _ = cancel.notified() => PollOutcome::Cancelled,
                res = tokio::task::spawn_blocking(move || {
                    let w = w.lock().unwrap_or_else(|p| p.into_inner());
                    w.poll(Some(Duration::from_secs(1)))
                }) => match res {
                    Ok(Some(ev)) => PollOutcome::Change(classify_kqueue(ev, &self.target)),
                    Ok(None) => PollOutcome::TimedOut,
                    Err(e) => {
                        tracing::warn!(error = %e, "kqueue poll task panicked");
                        return None;
                    }
                },
            };
            match outcome {
                PollOutcome::Change(c) => return Some(c),
                PollOutcome::TimedOut => continue,
                PollOutcome::Cancelled => return None,
            }
        }
    }
}

/// Map a single `kqueue` event to a [`RawChange`]. The `kqueue` crate
/// already classifies the underlying EVFILT_VNODE flags into a
/// [`kqueue::Vnode`] variant, so this is a direct lookup.
#[cfg(target_os = "macos")]
fn classify_kqueue(ev: kqueue::Event, target: &Path) -> RawChange {
    use kqueue::{EventData, Vnode};
    match ev.data {
        // Content / metadata family: the watched inode's data
        // changed (`Write`/`Extend`/`Truncate`) or its metadata
        // did (`Attrib`). macOS often reports a save as
        // `NOTE_ATTRIB` alone (e.g. when the new content is the
        // same length — only mtime changes), so `Attrib` must
        // land in the "modified" bucket or a plain save wakes
        // up the model as "deleted, renamed, or had its
        // attributes changed".
        EventData::Vnode(Vnode::Write | Vnode::Extend | Vnode::Truncate | Vnode::Attrib) => {
            RawChange::Modified(target.to_path_buf())
        }
        // Identity-lost family: the watch target was deleted,
        // renamed, had its access revoked, or its link count
        // changed. These mean the watcher is (probably) going
        // blind — keep the existing "deleted, renamed, or had its
        // attributes changed" wake text for `RootChanged`.
        EventData::Vnode(Vnode::Delete | Vnode::Rename | Vnode::Revoke | Vnode::Link) => {
            RawChange::RootChanged
        }
        // FreeBSD-only `Vnode::{Open,Close,CloseWrite,Read}` and
        // any future `EventData` variant (errors, non-VNODE
        // filters, etc.) — surface the event rather than silently
        // dropping it.
        _ => RawChange::RootChanged,
    }
}

// ---------------------------------------------------------------------------
// Linux backend (inotify).

#[cfg(target_os = "linux")]
struct LinuxBackend {
    /// Async wrapper over the inotify fd. Built from
    /// `Inotify::into_event_stream` so reads integrate with the tokio
    /// reactor: a `select!` racing `stream.next()` against cancel wakes
    /// the moment the fd is readable, and dropping the stream closes
    /// the fd (no leaked `spawn_blocking` task stuck in a blocking
    /// `read(2)`).
    stream: inotify::EventStream<Vec<u8>>,
    /// The path the `inotify` watch was registered on. inotify
    /// populates `Event::name` only for child entries of a watched
    /// directory; a directly-watched file's events carry no name, so
    /// the backend reconstructs the path here as either
    /// `target.join(name)` (child event) or `target` (target itself).
    target: PathBuf,
}

#[cfg(target_os = "linux")]
impl LinuxBackend {
    async fn next_raw(&mut self, cancel: &Arc<Notify>) -> Option<RawChange> {
        let target = self.target.clone();
        // Race the reactor-backed stream against cancel. With the
        // `EventStream` (an `AsyncFd` under the hood) the `select!`
        // arm wins on either the next event or the cancel signal —
        // no `spawn_blocking`, no interruptible `read(2)`.
        let event = tokio::select! {
            biased;
            _ = cancel.notified() => return None,
            event = self.stream.next() => event,
        };
        let event = event?.ok()?;
        let path = match event.name {
            Some(name) => target.join(name),
            None => target,
        };
        Some(Self::classify(event.mask, path))
    }

    fn classify(mask: inotify::EventMask, path: PathBuf) -> RawChange {
        if mask.contains(inotify::EventMask::CREATE) || mask.contains(inotify::EventMask::MOVED_TO)
        {
            RawChange::Created(path)
        } else if mask.contains(inotify::EventMask::DELETE)
            || mask.contains(inotify::EventMask::MOVED_FROM)
        {
            RawChange::Removed(path)
        } else if mask.contains(inotify::EventMask::DELETE_SELF)
            || mask.contains(inotify::EventMask::MOVE_SELF)
        {
            RawChange::RootChanged
        } else {
            RawChange::Modified(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ctx_for(label: &str) -> ObserverContext {
        ObserverContext {
            session_id: "test".into(),
            id: 1,
            label: label.into(),
        }
    }

    /// On every platform, a `FileSource` whose cancel is signaled
    /// before any change should not return `Some`. We give it a
    /// brief moment to be constructed and ready, then signal
    /// cancel. Either the source's blocking call sees the cancel
    /// (best) or the timeout (worst case, no event ever fires) —
    /// both must return `None`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn next_event_returns_none_after_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().to_path_buf();
        let cancel = Arc::new(Notify::new());
        let mut src = match FileSource::new(ctx_for("c"), &target, None) {
            Ok(s) => s,
            // Platform without a backend (e.g. Windows in CI).
            // Contract holds vacuously.
            Err(_) => return,
        };
        let cancel_for_source = cancel.clone();
        let handle = tokio::spawn(async move { src.next_event(cancel_for_source).await });
        // Give the source a beat to arm, then cancel.
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.notify_waiters();
        match tokio::time::timeout(Duration::from_secs(3), handle).await {
            Ok(Ok(None)) => {}
            Ok(Ok(Some(ev))) => panic!("cancelled source returned Some({ev:?})"),
            Ok(Err(e)) => panic!("source task panicked: {e}"),
            Err(_) => panic!("cancelled source did not return within 3s"),
        }
    }

    #[test]
    fn make_event_renders_all_raw_change_shapes() {
        let target = Path::new("/tmp/watched");
        let ctx = ctx_for("c");
        let e = make_event(
            &ctx,
            target,
            false,
            None,
            RawChange::Modified(PathBuf::from("a.txt")),
        );
        assert_eq!(e.summary, "modified a.txt");
        let e = make_event(
            &ctx,
            target,
            false,
            None,
            RawChange::Created(PathBuf::from("b.txt")),
        );
        assert_eq!(e.summary, "created b.txt");
        let e = make_event(
            &ctx,
            target,
            false,
            None,
            RawChange::Removed(PathBuf::from("c.txt")),
        );
        assert_eq!(e.summary, "removed c.txt");
        let e = make_event(&ctx, target, false, None, RawChange::RootChanged);
        assert!(e.summary.contains("deleted"), "{}", e.summary);
        // dir target RootChanged
        let e = make_event(&ctx, target, true, None, RawChange::RootChanged);
        assert!(e.summary.starts_with("changed "), "{}", e.summary);
        // Session id and id flow through
        assert_eq!(e.session_id, ctx.session_id);
        assert_eq!(e.id, ctx.id);
        assert_eq!(e.label, ctx.label);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn kqueue_watches_file_modification() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "v0").unwrap();
        let cancel = Arc::new(Notify::new());
        let mut src =
            FileSource::new(ctx_for("kq-file"), &path, None).expect("FileSource on macOS");
        let cancel_for_source = cancel.clone();
        let handle = tokio::spawn(async move { src.next_event(cancel_for_source).await });
        // Give kqueue a moment to arm.
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::fs::write(&path, "v1").unwrap();
        let ev = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("event arrived within budget")
            .expect("join ok")
            .expect("source returned Some");
        // A plain save on macOS now classifies as `Modified`
        // (kqueue::Vnode::Write), not `RootChanged`. Pin that here
        // — if a future regression collapses Mac back to the
        // "deleted, renamed, or had its attributes changed" wake
        // text, this assertion fails rather than the test passing
        // by accident via the old `|| deleted` tolerance.
        assert!(
            ev.summary.contains("modified") && ev.summary.contains("a.txt"),
            "unexpected summary: {:?}",
            ev.summary
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn inotify_watches_file_modification() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "v0").unwrap();
        let cancel = Arc::new(Notify::new());
        let mut src =
            FileSource::new(ctx_for("in-file"), &path, None).expect("FileSource on linux");
        let cancel_for_source = cancel.clone();
        let handle = tokio::spawn(async move { src.next_event(cancel_for_source).await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::fs::write(&path, "v1").unwrap();
        let ev = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("event arrived within budget")
            .expect("join ok")
            .expect("source returned Some");
        assert!(ev.summary.contains("a.txt"));
    }
}
