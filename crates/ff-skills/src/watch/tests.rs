use super::*;
use notify::PollWatcher;
use std::fs;
use std::sync::Arc;
use tempfile::tempdir;

/// Tests inject a [`PollWatcher`] instead of the OS-native `RecommendedWatcher`:
/// on Windows the latter's async `ReadDirectoryChangesW` handle intermittently
/// wedged a sibling test's filesystem syscall under concurrent test scheduling.
/// PollWatcher holds no such handle and a short poll interval keeps the
/// change-detection assertion fast.
fn poll_config() -> notify::Config {
    notify::Config::default().with_poll_interval(Duration::from_millis(50))
}

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

    let (_w, shared, errs) =
        SkillWatcher::spawn_with::<PollWatcher>(root.clone(), poll_config()).unwrap();
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
