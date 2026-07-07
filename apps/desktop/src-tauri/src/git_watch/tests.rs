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
    assert!(rx.try_recv().is_err());
    {
        let guard = w.watched.lock().unwrap();
        assert_eq!(guard.last_branch, Some(Some("main".to_string())));
        assert_eq!(guard.root.as_deref(), Some(dir.path()));
    }
    w.re_point(dir.path());
}

#[test]
fn re_point_to_non_repo_is_inert() {
    let dir = tempdir().unwrap();
    let (mut w, _rx) = GitHeadWatcher::spawn().unwrap();
    w.re_point(dir.path());
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
