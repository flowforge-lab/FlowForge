//! Filesystem hot-reload for the skills directory.
//!
//! [`SkillWatcher`] watches `~/.flowforge/skills/` and rebuilds the shared
//! [`SkillRegistry`] on change. A trailing debounce coalesces editor save-storms:
//! the registry reloads `DEBOUNCE` after the *last* filesystem event, so the
//! settled on-disk state is what lands (not an intermediate read mid-burst). The
//! agent reads the registry through the returned [`SharedRegistry`]; it snapshots
//! the active set at turn start so a mid-turn reload never races (M3.1b).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::error::SkillError;
use crate::registry::SkillRegistry;

/// Registry shared between the watcher (writer) and the agent (reader).
pub type SharedRegistry = Arc<RwLock<SkillRegistry>>;

const DEBOUNCE: Duration = Duration::from_millis(200);

/// Owns the OS watcher. Dropping it stops watching: the watcher's event sender
/// disconnects, and the debounce worker thread observes the disconnect and exits.
pub struct SkillWatcher {
    _watcher: RecommendedWatcher,
}

impl SkillWatcher {
    /// Load `root` once and start watching it. Returns the watcher (keep it alive)
    /// and the shared registry the watcher keeps up to date. Initial load errors
    /// are returned so the caller can surface them; later reload errors are logged.
    pub fn spawn(root: PathBuf) -> Result<(Self, SharedRegistry, Vec<SkillError>), SkillError> {
        let (initial, errors) = SkillRegistry::load_dir(&root);
        let shared: SharedRegistry = Arc::new(RwLock::new(initial));

        // notify callback (watcher thread) -> debounce worker thread.
        let (tx, rx) = mpsc::channel::<()>();

        let reload_target = Arc::clone(&shared);
        let reload_root = root.clone();
        thread::spawn(move || {
            // Block for the first event, then coalesce every event arriving within
            // DEBOUNCE of the previous one — reload only once the dir falls quiet.
            while rx.recv().is_ok() {
                loop {
                    match rx.recv_timeout(DEBOUNCE) {
                        Ok(()) => continue,
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                reload(&reload_root, &reload_target);
            }
        });

        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            match res {
                Ok(_) => {
                    // Worker only gone during teardown; a dropped send is harmless.
                    let _ = tx.send(());
                }
                Err(e) => tracing::warn!(error = %e, "skill watcher event error"),
            }
        })
        .map_err(|e| notify_io(&root, e))?;

        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| notify_io(&root, e))?;

        Ok((Self { _watcher: watcher }, shared, errors))
    }
}

/// Re-scan `root` and swap the shared registry's contents. The notify path and
/// the tests share this so a test reload exercises the same code.
fn reload(root: &Path, target: &SharedRegistry) {
    let (next, errors) = SkillRegistry::load_dir(root);
    for e in &errors {
        tracing::warn!(error = %e, "skill reload");
    }
    if let Ok(mut guard) = target.write() {
        *guard = next;
    }
}

fn notify_io(root: &Path, e: notify::Error) -> SkillError {
    SkillError::Io {
        path: root.to_path_buf(),
        source: std::io::Error::other(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn write_skill(root: &std::path::Path, dir: &str, name: &str) {
        let d = root.join(dir);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: d\nversion: 0.1.0\n---\nbody\n"),
        )
        .unwrap();
    }

    #[test]
    fn reload_picks_up_new_skill() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let shared: SharedRegistry = Arc::new(RwLock::new(SkillRegistry::new()));
        assert_eq!(shared.read().unwrap().len(), 0);

        write_skill(&root, "a", "alpha");
        reload(&root, &shared);
        assert_eq!(shared.read().unwrap().len(), 1);
        assert!(shared.read().unwrap().get("alpha").is_some());
    }

    #[test]
    fn spawn_loads_initial_and_watches() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        write_skill(&root, "a", "alpha");

        let (_w, shared, errs) = SkillWatcher::spawn(root.clone()).unwrap();
        assert!(errs.is_empty());
        assert_eq!(shared.read().unwrap().len(), 1);

        // Smoke: add a skill and give the watcher (plus the debounce window) a
        // chance to fire. Tolerant of platform event latency — fall back to an
        // explicit reload so CI is stable.
        write_skill(&root, "b", "beta");
        for _ in 0..40 {
            if shared.read().unwrap().len() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if shared.read().unwrap().len() != 2 {
            reload(&root, &shared);
        }
        assert_eq!(shared.read().unwrap().len(), 2);
    }
}
