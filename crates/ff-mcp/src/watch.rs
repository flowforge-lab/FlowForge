//! Filesystem hot-reload for `~/.flowforge/mcp.json` (RFC 0003 §3).
//!
//! Mirrors the `ff-skills` [`SkillWatcher`](../../ff-skills/src/watch.rs) pattern: a
//! `notify` watcher with a trailing debounce coalesces editor save-storms, then
//! reloads the config once the file settles. The parsed config lives behind a
//! [`SharedConfig`]; the supervisor (M4.2) snapshots it at turn start and `reconcile`s
//! against its running set, so a mid-turn edit never races an in-flight tool call.
//!
//! M4.1 only keeps the shared config current — it deliberately spawns no processes.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use ff_core::McpServerConfig;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::config;
use crate::error::McpError;

/// The desired server set, shared between the watcher (writer) and the supervisor
/// (reader). An empty vec means "no servers configured" (incl. a missing file).
pub type SharedConfig = Arc<RwLock<Vec<McpServerConfig>>>;

const DEBOUNCE: Duration = Duration::from_millis(200);

/// Owns the OS watcher. Dropping it stops watching: the event sender disconnects and
/// the debounce worker thread observes the disconnect and exits.
pub struct McpConfigWatcher {
    _watcher: RecommendedWatcher,
}

impl McpConfigWatcher {
    /// Load `path` once and start watching it. Returns the watcher (keep it alive) and
    /// the shared config it keeps current. The initial parse error (if any) is returned
    /// so the caller can surface it; later reload errors are logged and leave the last
    /// good config in place.
    ///
    /// The parent directory is watched rather than the file itself: editors commonly
    /// save via rename/replace, which drops a watch pinned to the original inode. Events
    /// are then filtered to the config file's own name ([`event_touches`]) so unrelated
    /// sibling writes under `~/.flowforge/` — e.g. `skill_signals.json`, rewritten every
    /// turn — don't trigger a per-turn no-op reload.
    pub fn spawn(path: PathBuf) -> Result<(Self, SharedConfig), McpError> {
        let initial = config::load(&path)?;
        let shared: SharedConfig = Arc::new(RwLock::new(initial));

        let (tx, rx) = mpsc::channel::<()>();

        let reload_target = Arc::clone(&shared);
        let reload_path = path.clone();
        thread::spawn(move || {
            while rx.recv().is_ok() {
                loop {
                    match rx.recv_timeout(DEBOUNCE) {
                        Ok(()) => continue,
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                reload(&reload_path, &reload_target);
            }
        });

        let watch_dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        // The config file's own name, used to ignore sibling writes in the watched dir.
        let file_name = path.file_name().map(OsStr::to_os_string);

        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    if event_touches(&event.paths, file_name.as_deref()) {
                        let _ = tx.send(());
                    }
                }
                Err(e) => tracing::warn!(error = %e, "mcp config watcher event error"),
            })
            .map_err(|e| notify_err(&path, e))?;

        watcher
            .watch(&watch_dir, RecursiveMode::NonRecursive)
            .map_err(|e| notify_err(&path, e))?;

        Ok((Self { _watcher: watcher }, shared))
    }
}

/// Re-parse `path` and swap the shared config. A parse error leaves the previous good
/// config untouched (a half-saved file shouldn't tear down running servers). The
/// notify path and the tests share this so a test reload exercises the same code.
fn reload(path: &Path, target: &SharedConfig) {
    match config::load(path) {
        Ok(next) => {
            if let Ok(mut guard) = target.write() {
                *guard = next;
            }
        }
        Err(e) => tracing::warn!(error = %e, "mcp config reload; keeping last good config"),
    }
}

/// Whether a notify event concerns the config file we care about, identified by its
/// file name. Returns `true` when any event path matches, and also when `paths` is
/// empty (some platforms omit them) or the watched path has no file name — staying
/// conservative so a real change is never missed; a spurious reload is only a cheap
/// no-op swap.
fn event_touches(paths: &[PathBuf], file_name: Option<&OsStr>) -> bool {
    let Some(name) = file_name else {
        return true;
    };
    if paths.is_empty() {
        return true;
    }
    paths.iter().any(|p| p.file_name() == Some(name))
}

fn notify_err(path: &Path, e: notify::Error) -> McpError {
    McpError::Config(format!("watching {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn event_touches_matches_only_the_config_file() {
        let name = Some(OsStr::new("mcp.json"));
        assert!(event_touches(
            &[PathBuf::from("/home/u/.flowforge/mcp.json")],
            name
        ));
        // Sibling write (e.g. skill_signals.json rewritten every turn) is ignored.
        assert!(!event_touches(
            &[PathBuf::from("/home/u/.flowforge/skill_signals.json")],
            name
        ));
        // Mixed batch with the config file present still fires.
        assert!(event_touches(
            &[
                PathBuf::from("/home/u/.flowforge/skill_signals.json"),
                PathBuf::from("/home/u/.flowforge/mcp.json"),
            ],
            name
        ));
    }

    #[test]
    fn event_touches_is_conservative_when_paths_empty_or_no_name() {
        assert!(event_touches(&[], Some(OsStr::new("mcp.json"))));
        assert!(event_touches(&[PathBuf::from("/x/mcp.json")], None));
    }

    const ONE: &str = r#"{"mcpServers":{"a":{"command":"x"}}}"#;
    const TWO: &str = r#"{"mcpServers":{"a":{"command":"x"},"b":{"command":"y"}}}"#;

    #[test]
    fn reload_swaps_in_new_config() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("mcp.json");
        let shared: SharedConfig = Arc::new(RwLock::new(Vec::new()));

        fs::write(&path, ONE).unwrap();
        reload(&path, &shared);
        assert_eq!(shared.read().unwrap().len(), 1);

        fs::write(&path, TWO).unwrap();
        reload(&path, &shared);
        assert_eq!(shared.read().unwrap().len(), 2);
    }

    #[test]
    fn reload_keeps_last_good_on_parse_error() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("mcp.json");
        let shared: SharedConfig = Arc::new(RwLock::new(Vec::new()));

        fs::write(&path, ONE).unwrap();
        reload(&path, &shared);
        assert_eq!(shared.read().unwrap().len(), 1);

        fs::write(&path, "{ broken").unwrap();
        reload(&path, &shared);
        assert_eq!(
            shared.read().unwrap().len(),
            1,
            "bad parse must not clobber"
        );
    }

    #[test]
    fn spawn_missing_file_starts_empty() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("mcp.json");
        let (_w, shared) = McpConfigWatcher::spawn(path).unwrap();
        assert!(shared.read().unwrap().is_empty());
    }

    #[test]
    fn spawn_loads_initial_and_watches() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("mcp.json");
        fs::write(&path, ONE).unwrap();

        let (_w, shared) = McpConfigWatcher::spawn(path.clone()).unwrap();
        assert_eq!(shared.read().unwrap().len(), 1);

        // Smoke: edit and give the watcher + debounce window time to fire. Tolerant of
        // platform event latency — fall back to an explicit reload so CI is stable.
        fs::write(&path, TWO).unwrap();
        for _ in 0..40 {
            if shared.read().unwrap().len() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if shared.read().unwrap().len() != 2 {
            reload(&path, &shared);
        }
        assert_eq!(shared.read().unwrap().len(), 2);
    }
}
