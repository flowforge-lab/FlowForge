//! File-system watcher for the local dev-update feed (#705, Phase 2).
//!
//! Watches `~/.config/flowforge/dev-update/latest.json` via kqueue/inotify so the
//! running app detects a new `dev-release.sh` build **instantly** rather than waiting
//! for the 15s poll. Mirrors the `git_watch.rs` debounce pattern: a burst of writes
//! (the script writes latest.json + tarball in quick succession) coalesces into a
//! single notification after the storm settles.
//!
//! Activated only when the frontend enables it (the `localUpdateChannel` flag is
//! FE-only localStorage; the FE calls `start_dev_update_watcher` on boot when the
//! flag is on). Zero-cost when idle (OS event-driven, no polling).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use tokio::sync::mpsc as tokio_mpsc;

/// Trailing debounce: `dev-release.sh` writes the tarball then `latest.json` in quick
/// succession; wait for the burst to settle before signalling.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// The file we care about inside the watched directory.
const FEED_FILE: &str = "latest.json";

/// The well-known directory the watcher observes.
pub fn dev_update_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flowforge")
        .join("dev-update")
}

/// Owns the OS watcher; dropping it stops watching and the worker thread exits.
pub struct DevUpdateWatcher {
    _watcher: notify::RecommendedWatcher,
}

impl DevUpdateWatcher {
    /// Start watching the dev-update directory. Returns the watcher (keep alive) and
    /// a receiver that yields `()` each time `latest.json` changes (debounced).
    pub fn spawn() -> notify::Result<(Self, tokio_mpsc::UnboundedReceiver<()>)> {
        let dir = dev_update_dir();
        // Ensure the directory exists so the watcher doesn't fail on first run.
        let _ = std::fs::create_dir_all(&dir);

        let (tx, rx) = mpsc::channel::<()>();
        let (emit_tx, emit_rx) = tokio_mpsc::unbounded_channel::<()>();

        // Debounce worker: coalesce a write burst into one signal.
        thread::spawn(move || {
            while rx.recv().is_ok() {
                loop {
                    match rx.recv_timeout(DEBOUNCE) {
                        Ok(()) => continue,
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                let _ = emit_tx.send(());
            }
        });

        let watch_dir = dir.clone();
        let watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    if touches_feed(&event.paths, &watch_dir) {
                        let _ = tx.send(());
                    }
                }
                Err(e) => tracing::warn!(error = %e, "dev-update watcher event error"),
            })?;

        let mut w = watcher;
        w.watch(&dir, RecursiveMode::NonRecursive)?;

        Ok((Self { _watcher: w }, emit_rx))
    }
}

/// Whether any path in the event is (or is inside) our feed file.
fn touches_feed(paths: &[PathBuf], dir: &Path) -> bool {
    let target = dir.join(FEED_FILE);
    paths.iter().any(|p| p == &target)
}
