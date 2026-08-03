//! Debounced reindex on edit (RFC 0006 §4). Mirrors the `ff-mcp` / `ff-skills`
//! watcher pattern: a `notify` watcher with a trailing debounce coalesces an
//! editor's save-storm, then rebuilds the index once the directory settles. The
//! reindex runs on a background thread, off the turn path.
//!
//! The whole memory root is watched **recursively** (daily logs live in a
//! `daily/` subdirectory) and events are filtered to Markdown files, so writing
//! the sibling `index.db` never triggers a self-reindex loop.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

use crate::index::MemoryIndex;
use crate::Memory;

const DEBOUNCE: Duration = Duration::from_millis(200);

/// Owns the OS watcher. Dropping it stops watching: the event sender disconnects
/// and the debounce worker thread observes the disconnect and exits.
pub struct MemoryWatcher {
    _watcher: Box<dyn Watcher + Send>,
}

impl MemoryWatcher {
    /// Start watching `memory`'s root, reindexing `index` on every settled change.
    /// Does **not** perform the initial build — the host does that once at startup
    /// so a failure there is surfaced; the watcher only keeps the index current.
    pub fn spawn(memory: Arc<Memory>, index: Arc<dyn MemoryIndex>) -> notify::Result<Self> {
        let (tx, rx) = mpsc::channel::<()>();

        let reindex_memory = Arc::clone(&memory);
        thread::spawn(move || {
            while rx.recv().is_ok() {
                // Drain the debounce window: keep resetting while edits arrive.
                loop {
                    match rx.recv_timeout(DEBOUNCE) {
                        Ok(()) => continue,
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                if let Err(e) = index.reindex(&reindex_memory.all_chunks()) {
                    tracing::warn!(error = %e, "memory reindex failed; keeping last good index");
                }
            }
        });

        let root = memory.root().to_path_buf();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    if event.paths.iter().any(|p| is_markdown(p)) {
                        let _ = tx.send(());
                    }
                }
                Err(e) => tracing::warn!(error = %e, "memory watcher event error"),
            })?;

        // The root may not exist yet on a fresh install; create it so the watch
        // attaches (memory writes would create it anyway).
        let _ = std::fs::create_dir_all(&root);
        watcher.watch(&root, RecursiveMode::Recursive)?;

        Ok(Self {
            _watcher: Box::new(watcher),
        })
    }

    /// Test-only: return a no-op watcher handle without starting a filesystem
    /// watcher or a background reindex thread. Gated by `#[cfg(not(test))]` so
    /// it is invisible when this crate is compiled in its own test build — the
    /// leaf-crate watcher tests keep exercising a real [`RecommendedWatcher`] via
    /// [`spawn`].
    #[cfg(not(test))]
    pub fn spawn_without_watcher() -> Self {
        Self {
            _watcher: Box::new(NoopWatcher),
        }
    }
}

/// No-op watcher for test builds: satisfies the [`Watcher`] trait without
/// touching the filesystem.
#[cfg(not(test))]
struct NoopWatcher;

#[cfg(not(test))]
impl Watcher for NoopWatcher {
    fn new<F: notify::EventHandler>(
        _event_handler: F,
        _config: notify::Config,
    ) -> notify::Result<Self> {
        Ok(Self)
    }
    fn watch(&mut self, _path: &Path, _recursive_mode: RecursiveMode) -> notify::Result<()> {
        Ok(())
    }
    fn unwatch(&mut self, _path: &Path) -> notify::Result<()> {
        Ok(())
    }
    fn kind() -> notify::WatcherKind {
        notify::WatcherKind::NullWatcher
    }
}

fn is_markdown(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("md")
}

#[cfg(test)]
mod tests;
