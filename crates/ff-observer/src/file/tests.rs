//! File source integration tests. Exercise the OS watcher end-to-end:
//! construct a `FileSource` against a tempdir, mutate the target, and
//! confirm the source fires an event with the expected summary.

use std::time::Duration;

use tempfile::TempDir;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::FileSource;
use crate::event::{ObserverError, ObserverSpec};
use crate::source::ObserverSource;

fn spec_for(target: &str) -> ObserverSpec {
    ObserverSpec {
        kind: crate::event::ObserverKind::File,
        target: target.to_string(),
        filter: None,
        interval: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fires_on_new_file_in_watched_dir() {
    let dir = TempDir::new().unwrap();
    let mut src = FileSource::from_spec(spec_for(dir.path().to_str().unwrap()))
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    // The watcher is set up lazily inside `next_event` (so the source is
    // constructible without an active runtime). Trigger that and confirm an
    // event lands within a reasonable timeout.
    let dir_clone = dir.path().to_path_buf();
    let writer = tokio::task::spawn_blocking(move || {
        // Give the kqueue/inotify a moment to register before we write.
        std::thread::sleep(Duration::from_millis(150));
        std::fs::write(dir_clone.join("created.txt"), "hi").unwrap();
    });
    let event = timeout(
        Duration::from_secs(3),
        src.next_event(crate::event::ObserverId(42), &cancel),
    )
    .await
    .expect("event within timeout")
    .expect("source result")
    .expect("event was Some");
    writer.await.unwrap();
    assert_eq!(event.id, crate::event::ObserverId(42));
    assert!(event.summary.starts_with("file changed:"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_missing_target() {
    let err = FileSource::from_spec(spec_for("/this/path/does/not/exist/anywhere"))
        .await
        .unwrap_err();
    assert!(matches!(err, ObserverError::InvalidTarget { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filter_rejects_non_matching_event() {
    let dir = TempDir::new().unwrap();
    // Watch a single file with a regex that should never match the dir
    // basename. We expect `next_event` to keep waiting (timeout) and not
    // produce an event.
    let target = dir.path().join("watched.txt");
    std::fs::write(&target, "x").unwrap();
    let mut src = FileSource::from_spec(ObserverSpec {
        kind: crate::event::ObserverKind::File,
        target: target.display().to_string(),
        filter: Some(r"^zzz_does_not_match$".to_string()),
        interval: None,
    })
    .await
    .unwrap();
    let cancel = CancellationToken::new();
    // Drive a write; the source should keep waiting (the basename of the
    // changed file is `watched.txt`, which won't match the strict regex).
    let target_clone = target.clone();
    let writer = tokio::task::spawn_blocking(move || {
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(&target_clone, "mutated").unwrap();
    });
    let res = timeout(
        Duration::from_millis(500),
        src.next_event(crate::event::ObserverId(7), &cancel),
    )
    .await;
    writer.await.unwrap();
    assert!(res.is_err(), "expected timeout, got {res:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fires_on_single_file_watch() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("watched.txt");
    std::fs::write(&target, "initial").unwrap();
    let mut src = FileSource::from_spec(spec_for(target.display().to_string().as_str()))
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let target_clone = target.clone();
    let writer = tokio::task::spawn_blocking(move || {
        std::thread::sleep(Duration::from_millis(150));
        std::fs::write(&target_clone, "changed").unwrap();
    });
    let event = timeout(
        Duration::from_secs(3),
        src.next_event(crate::event::ObserverId(8), &cancel),
    )
    .await
    .expect("event within timeout")
    .expect("source result")
    .expect("event was Some");
    writer.await.unwrap();
    assert_eq!(event.id, crate::event::ObserverId(8));
    assert!(event.summary.starts_with("file changed:"));
    assert!(
        event.summary.contains("watched.txt"),
        "summary should contain filename: {}",
        event.summary
    );
}

/// A rapid-fire rewrite (a "save storm") must coalesce into a single event; if
/// we forwarded every raw OS event we'd spawn a whole assistant turn per
/// kernel notify — the issue's most expensive bug. The 5 writes here fire
/// inside one trailing window and the source must report exactly one event.
/// A second quiet storm-then-quiet cycle should emit a second event to prove
/// the debouncer window actually closes (not just the first event coalescing).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn save_storm_coalesces_into_one_event() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("watched.txt");
    std::fs::write(&target, "v0").unwrap();
    let mut src = FileSource::from_spec(spec_for(target.display().to_string().as_str()))
        .await
        .unwrap();
    let cancel = CancellationToken::new();

    // Storm A: an initial sleep gives the lazily-spawned watcher time to
    // register with kqueue/inotify, then 5 writes 80ms apart stay inside one
    // 500ms trailing window. Without debouncing the source would emit five
    // independent events.
    let target_a = target.clone();
    let storm_a = tokio::task::spawn_blocking(move || {
        std::thread::sleep(Duration::from_millis(250));
        for i in 1..=5 {
            std::fs::write(&target_a, format!("v{i}")).unwrap();
            std::thread::sleep(Duration::from_millis(80));
        }
    });
    // Total storm runtime: 250ms + 5*80ms = 650ms. Debouncer trailing edge:
    // ~650ms + 500ms = ~1150ms. 3s timeout gives generous headroom for OS
    // event delivery and CI scheduling jitter.
    let first = timeout(
        Duration::from_secs(3),
        src.next_event(crate::event::ObserverId(9), &cancel),
    )
    .await
    .expect("first event within timeout")
    .expect("source result")
    .expect("event was Some");
    storm_a.await.unwrap();
    assert_eq!(first.id, crate::event::ObserverId(9));
    assert!(first.summary.contains("watched.txt"), "{}", first.summary);

    // Storm B: a second cycle must produce a second event. Without the
    // trailing flush in `tick`, the source would sit on the first window
    // forever and this second `next_event` would hang past the 3s budget.
    let target_b = target.clone();
    let storm_b = tokio::task::spawn_blocking(move || {
        std::thread::sleep(Duration::from_millis(150));
        for i in 6..=8 {
            std::fs::write(&target_b, format!("v{i}")).unwrap();
            std::thread::sleep(Duration::from_millis(80));
        }
    });
    let second = timeout(
        Duration::from_secs(3),
        src.next_event(crate::event::ObserverId(9), &cancel),
    )
    .await
    .expect("second event within timeout")
    .expect("source result")
    .expect("event was Some");
    storm_b.await.unwrap();
    assert!(second.summary.contains("watched.txt"), "{}", second.summary);

    // Quiet period: no further writes. The debouncer must have closed both
    // storms' windows, so this `next_event` times out — catches a debouncer
    // that never closes its window (the bug the issue called out).
    std::thread::sleep(Duration::from_millis(700));
    let third = timeout(
        Duration::from_millis(700),
        src.next_event(crate::event::ObserverId(9), &cancel),
    )
    .await;
    assert!(
        third.is_err(),
        "expected no third event; got {:?}",
        third.ok().and_then(|r| r.ok()),
    );
}

/// Dropping the source must tear down its OS watcher thread (issue: "cancel
/// tears down the OS handles"). On Linux that means the inotify fd should
/// be reclaimed when the source goes out of scope. Counting
/// `/proc/self/fd/inotify` entries before and after drops the thread lets
/// the test catch a leak without coupling to private fields.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_source_releases_inotify_fd() {
    let count_before = count_inotify_fds();

    {
        let dir = TempDir::new().unwrap();
        let mut src = FileSource::from_spec(spec_for(dir.path().to_str().unwrap()))
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        // Drive a short next_event call so the lazy worker thread is spawned
        // and registers the inotify watch.
        let _ = timeout(
            Duration::from_millis(50),
            src.next_event(crate::event::ObserverId(7), &cancel),
        )
        .await;
        // Give the worker a beat to actually open the fd before we drop.
        std::thread::sleep(Duration::from_millis(200));
        // `src` drops here. Without the `disconnected_is_stop` fix, the
        // worker keeps spinning forever and the inotify fd leaks.
    }

    // The loop sleeps for `POLL_INTERVAL` (100ms) plus a `Debouncer::tick`
    // slot before noticing the dropped stop sender; allow generous headroom
    // for CI scheduling jitter.
    std::thread::sleep(Duration::from_millis(800));

    let count_after = count_inotify_fds();
    assert_eq!(
        count_after, count_before,
        "inotify fd leaked: before={count_before}, after={count_after} (worker thread didn't exit on source drop)",
    );
}

/// Linux-only: count `/proc/self/fd` entries whose target is an `inotify`
/// resource. Letting the test stay Linux-only avoids pulling in `procfs` as
/// a dev-dep; the assertion below is the diagnostic value, not the helper.
#[cfg(target_os = "linux")]
fn count_inotify_fds() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("/proc/self/fd is readable on Linux")
        .filter_map(|e| e.ok())
        .filter(|entry| {
            std::fs::read_link(entry.path())
                .map(|target| target.to_string_lossy().contains("inotify"))
                .unwrap_or(false)
        })
        .count()
}
