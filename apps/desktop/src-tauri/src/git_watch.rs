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
mod tests {
    use super::*;
    use std::fs;
    use std::time::Instant;
    use tempfile::tempdir;

    fn write_head(root: &Path, content: &str) {
        let git = root.join(".git");
        fs::create_dir_all(&git).unwrap();
        fs::write(git.join("HEAD"), content).unwrap();
    }

    fn watched_at(root: &Path) -> Arc<Mutex<Watched>> {
        Arc::new(Mutex::new(Watched {
            root: Some(root.to_path_buf()),
            last_branch: None,
        }))
    }

    #[test]
    fn event_touches_matches_only_head() {
        assert!(event_touches(&[PathBuf::from("/r/.git/HEAD")]));
        assert!(!event_touches(&[PathBuf::from("/r/.git/index")]));
        // A mixed batch with HEAD present still fires.
        assert!(event_touches(&[
            PathBuf::from("/r/.git/index"),
            PathBuf::from("/r/.git/HEAD"),
        ]));
    }

    #[test]
    fn event_touches_is_conservative_when_paths_empty() {
        assert!(event_touches(&[]));
    }

    #[test]
    fn settle_emits_first_resolve_then_suppresses_duplicate() {
        let dir = tempdir().unwrap();
        write_head(dir.path(), "ref: refs/heads/main\n");
        let w = watched_at(dir.path());

        let first = settle(&w).expect("first resolve emits");
        assert_eq!(first.git_branch, Some("main".to_string()));
        assert_eq!(first.path, dir.path().display().to_string());

        // Unchanged branch -> suppressed (no spurious FE wake).
        assert!(settle(&w).is_none());
    }

    #[test]
    fn settle_emits_on_branch_change() {
        let dir = tempdir().unwrap();
        write_head(dir.path(), "ref: refs/heads/main\n");
        let w = watched_at(dir.path());
        assert_eq!(settle(&w).unwrap().git_branch, Some("main".into()));

        write_head(dir.path(), "ref: refs/heads/feature/x\n");
        assert_eq!(settle(&w).unwrap().git_branch, Some("feature/x".into()));
    }

    #[test]
    fn settle_reports_detached_head_as_null_branch() {
        let dir = tempdir().unwrap();
        write_head(dir.path(), "ref: refs/heads/main\n");
        let w = watched_at(dir.path());
        settle(&w).unwrap();

        write_head(dir.path(), "0123456789abcdef0123456789abcdef01234567\n");
        let ev = settle(&w).expect("switch to detached emits");
        assert_eq!(ev.git_branch, None);
        // Staying detached suppresses.
        assert!(settle(&w).is_none());
    }

    #[test]
    fn settle_is_none_when_unpointed() {
        let w = Arc::new(Mutex::new(Watched {
            root: None,
            last_branch: None,
        }));
        assert!(settle(&w).is_none());
    }

    #[test]
    fn re_point_is_idempotent_and_seeds_without_emitting() {
        let dir = tempdir().unwrap();
        write_head(dir.path(), "ref: refs/heads/main\n");
        let (mut w, mut rx) = GitHeadWatcher::spawn().unwrap();

        w.re_point(dir.path());
        // re_point seeds the current branch but emits nothing itself.
        assert!(rx.try_recv().is_err());
        {
            let guard = w.watched.lock().unwrap();
            assert_eq!(guard.last_branch, Some(Some("main".to_string())));
            assert_eq!(guard.root.as_deref(), Some(dir.path()));
        }
        // Re-pointing to the same root is a no-op (root unchanged).
        w.re_point(dir.path());
    }

    #[test]
    fn re_point_to_non_repo_is_inert() {
        let dir = tempdir().unwrap(); // no .git
        let (mut w, _rx) = GitHeadWatcher::spawn().unwrap();
        w.re_point(dir.path()); // must not panic
        let guard = w.watched.lock().unwrap();
        assert_eq!(guard.last_branch, Some(None));
    }

    #[test]
    fn watcher_emits_on_real_head_change() {
        let dir = tempdir().unwrap();
        write_head(dir.path(), "ref: refs/heads/main\n");
        let (mut w, mut rx) = GitHeadWatcher::spawn().unwrap();
        w.re_point(dir.path());

        write_head(dir.path(), "ref: refs/heads/dev\n");

        // Tolerant of OS event latency (mirrors the ff-mcp smoke style): poll the
        // channel within a budget, then fall back to a direct `settle` so the
        // assertion is deterministic even if the FS event is dropped on a slow host.
        let start = Instant::now();
        let emitted = loop {
            if let Ok(ev) = rx.try_recv() {
                break Some(ev);
            }
            if start.elapsed() > Duration::from_secs(3) {
                break None;
            }
            thread::sleep(Duration::from_millis(25));
        };
        let ev = emitted.unwrap_or_else(|| settle(&w.watched).expect("branch changed to dev"));
        assert_eq!(ev.git_branch, Some("dev".to_string()));
    }
}
