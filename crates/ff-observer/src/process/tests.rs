//! `ProcessSource` integration tests. Drive a real subprocess via the
//! shared `ProcessSupervisor` and confirm the source fires on a matching
//! line. Uses `tempfile` for the cwd and the supervisor's
//! `subscribe_lines` so the test exercises the same plumbing production
//! uses.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use ff_tools::process::ProcessSupervisor;

use super::{from_supervisor, ProcessSource};
use crate::event::{ObserverError, ObserverKind, ObserverSpec};
use crate::source::ObserverSource;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fires_on_matching_line() {
    let dir = tempfile::TempDir::new().unwrap();
    let sup = Arc::new(ProcessSupervisor::new());
    // Write a tiny script that emits a few lines; the `sh -c` is needed
    // because the supervisor shells through `/bin/sh -c` already, so the
    // direct `echo ; sleep` is enough on every platform.
    let id = sup
        .start(
            "echo ready to go; echo error in test; sleep 5",
            dir.path(),
            "sess",
        )
        .unwrap();
    // Subscribe to lines BEFORE the loop so we don't miss the early ones.
    let mut line_rx = sup.subscribe_lines(id).expect("process is live");
    // Drive a separate task that drains the broadcast so the source
    // doesn't see a closed channel prematurely.
    let _drain = tokio::spawn(async move {
        while let Ok(_line) = line_rx.recv().await {
            // discard; we only care that the supervisor is publishing
        }
    });
    // Wait for the drain task to start; the broadcast's first line will
    // arrive within a couple hundred ms on every reasonable platform.
    let filter = regex::Regex::new("ready|error").ok();
    let mut src = from_supervisor(id, filter, sup.clone());
    let cancel = CancellationToken::new();
    let event = timeout(
        Duration::from_secs(5),
        src.next_event(crate::event::ObserverId(7), &cancel),
    )
    .await
    .expect("event within timeout")
    .expect("source result")
    .expect("event was Some");
    assert!(event.summary.contains("matched:"));
    let _ = sup.stop(id, "sess").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_non_numeric_target() {
    let err = ProcessSource::from_spec(ObserverSpec {
        kind: ObserverKind::Process,
        target: "not-a-pid".to_string(),
        filter: None,
        interval: None,
    })
    .unwrap_err();
    assert!(matches!(err, ObserverError::InvalidTarget { .. }));
}
