//! Live-sync the active session's git branch by watching `.git/HEAD` (#561, BE half).
//!
//! Backend counterpart to the frontend merged in PR #581, which added the reactive
//! `workspace:branch-changed` listener and the `applyBranchChanged` store patch but
//! had nothing to emit the event. This watcher is that emitter.
//!
//! Mirrors the `ff-mcp` `McpConfigWatcher` pattern: a `notify` watcher on the
//! workspace's `.git` directory with a trailing debounce coalesces git's rapid HEAD
//! rewrites -- a single rebase or checkout touches `HEAD` many times. On settle it
//! re-resolves the branch and, *only when it actually changed*, sends a
//! `SessionWorkspace` that the app forwards to the FE as `workspace:branch-changed`.
//!
//! One long-lived watcher, re-pointed per active session ([`re_point`](GitHeadWatcher::re_point))
//! from the same turn-start hook that aligns codegraph (#548). A single active root at
//! a time; multi-workspace keying is deferred to #557, matching the FE, which patches
//! every cached session sharing the path.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use ff_core::SessionWorkspace;
use notify::{RecursiveMode, Watcher};
use tokio::sync::mpsc as tokio_mpsc;

use crate::git_branch;

/// Trailing debounce; a rebase/checkout rewrites `HEAD` in a burst, so wait for it to
/// settle before resolving once. Matches the `ff-mcp` watcher's window.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// The `.git/HEAD` file name. We watch the `.git` directory and filter to this so
/// sibling writes (`index`, `ORIG_HEAD`, `packed-refs`, lockfiles) never wake the
/// worker for a no-op resolve.
const HEAD_NAME: &str = "HEAD";

/// Current watched root plus the last branch emitted for it, shared between the
/// debounce worker and [`GitHeadWatcher::re_point`].
struct Watched {
    /// The active session root whose `.git/HEAD` is watched, or `None` before the
    /// first `re_point`. Tracked so `re_point` is idempotent and can unwatch the old
    /// `.git` directory.
    root: Option<PathBuf>,
    /// Last branch emitted for `root`. `Some(None)` = detached / no branch; `None` =
    /// nothing seeded yet, so the next resolve always emits. Drives emit suppression.
    last_branch: Option<Option<String>>,
}

/// Owns the OS watcher; dropping it stops watching and the worker thread exits.
/// `re_point` swaps which `.git` directory is observed as the active session changes.
pub struct GitHeadWatcher {
    watcher: notify::RecommendedWatcher,
    watched: Arc<Mutex<Watched>>,
}

impl GitHeadWatcher {
    /// Start an unpointed watcher and its debounce worker. Returns the watcher (keep it
    /// alive) and a receiver that yields a `SessionWorkspace` after each *changed*
    /// branch resolution. Call [`re_point`](Self::re_point) to aim it at a workspace.
    pub fn spawn() -> notify::Result<(Self, tokio_mpsc::UnboundedReceiver<SessionWorkspace>)> {
        let watched = Arc::new(Mutex::new(Watched {
            root: None,
            last_branch: None,
        }));

        let (tx, rx) = mpsc::channel::<()>();
        let (emit_tx, emit_rx) = tokio_mpsc::unbounded_channel::<SessionWorkspace>();

        let worker_state = Arc::clone(&watched);
        thread::spawn(move || {
            while rx.recv().is_ok() {
                // Coalesce a save/rebase storm: keep draining until HEAD settles.
                loop {
                    match rx.recv_timeout(DEBOUNCE) {
                        Ok(()) => continue,
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                if let Some(ws) = settle(&worker_state) {
                    // A closed receiver (app shutting down) is fine.
                    let _ = emit_tx.send(ws);
                }
            }
        });

        let watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    if event_touches(&event.paths) {
                        let _ = tx.send(());
                    }
                }
                Err(e) => tracing::warn!(error = %e, "git head watcher event error"),
            })?;

        Ok((Self { watcher, watched }, emit_rx))
    }

    /// Aim the watcher at `root`'s `.git/HEAD`. Idempotent: a no-op when `root` is
    /// already the watched one. Otherwise unwatch the previous `.git`, watch the new
    /// one, and *seed* the last-branch with `root`'s current branch so re-pointing
    /// never emits a spurious change -- the FE already has that branch from the
    /// turn-start `get_session_workspace`. Best-effort: a non-repo, or a `.git` *file*
    /// (linked worktree / submodule -- deferred per #561), logs and leaves the watcher
    /// inert for that workspace rather than erroring.
    pub fn re_point(&mut self, root: &Path) {
        let mut w = self.watched.lock().unwrap();
        if w.root.as_deref() == Some(root) {
            return;
        }
        if let Some(old) = w.root.take() {
            let _ = self.watcher.unwatch(&old.join(".git"));
        }
        let git_dir = root.join(".git");
        if let Err(e) = self.watcher.watch(&git_dir, RecursiveMode::NonRecursive) {
            tracing::warn!(
                root = %root.display(),
                error = %e,
                "git head watch unavailable; live branch sync inert for this workspace"
            );
        }
        // Seed even when the watch failed: harmless and keeps state consistent.
        w.root = Some(root.to_path_buf());
        w.last_branch = Some(git_branch(root));
    }
}

/// Resolve the watched root's current branch and decide whether to emit. Returns
/// `Some` -- and records the new branch as last-emitted -- only when the branch differs
/// from the last emitted value; `None` suppresses a no-op (and when unpointed). Pure
/// given the filesystem, so the emit policy is unit-testable without the notify and
/// debounce machinery.
fn settle(watched: &Arc<Mutex<Watched>>) -> Option<SessionWorkspace> {
    let mut w = watched.lock().unwrap();
    let root = w.root.clone()?;
    let branch = git_branch(&root);
    if w.last_branch.as_ref() == Some(&branch) {
        return None;
    }
    w.last_branch = Some(branch.clone());
    Some(SessionWorkspace {
        path: root.display().to_string(),
        git_branch: branch,
    })
}

/// Whether a notify event concerns `.git/HEAD`. Conservative: fires when any path's
/// file name is `HEAD`, and also when `paths` is empty (some platforms omit them) so a
/// real change is never missed -- a spurious wake is only a cheap no-op resolve.
fn event_touches(paths: &[PathBuf]) -> bool {
    if paths.is_empty() {
        return true;
    }
    paths
        .iter()
        .any(|p| p.file_name() == Some(OsStr::new(HEAD_NAME)))
}

#[cfg(test)]
mod tests;
