//! Headless cron firing engine (RFC 0017 §4, §8.2). A background task wakes on a
//! fixed interval, asks the store for due tasks, and fires each one through a
//! host-supplied [`TaskRunner`]. This crate stays Tauri-free: the runner trait
//! is abstract, so the desktop supplies an impl that builds a session and drives
//! the agent loop, while tests supply a fake. Event emission lives in the impl,
//! not the loop.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::FutureExt;

use crate::store::ScheduledStore;
use ff_core::{RunRecord, RunStatus, ScheduledTask};

/// The outcome of firing one task: the session it ran in (when one was created)
/// and the terminal status to record (RFC 0017 §8.4).
pub struct RunOutcome {
    pub session_id: Option<String>,
    pub status: RunStatus,
}

/// Fires a single due task. The host (desktop) builds a session and runs the
/// agent turn; tests supply a fake. Implementations own any per-fire timeout and
/// cancellation so the loop stays a thin scheduler (RFC 0017 §8.2).
#[async_trait]
pub trait TaskRunner: Send + Sync {
    async fn fire(&self, task: &ScheduledTask) -> RunOutcome;
}

/// Spawn the background scheduler. On each `tick` it fires every due task
/// serialized, then stamps `last_run` and appends a run record. Serializing the
/// fires within a tick (and stamping before the next tick re-evaluates due-ness)
/// is the dedupe: a long-running fire cannot be re-fired underneath itself.
pub fn spawn_scheduler(
    store: Arc<ScheduledStore>,
    runner: Arc<dyn TaskRunner>,
    tick: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tick);
        loop {
            interval.tick().await;
            run_due_once(store.as_ref(), runner.as_ref()).await;
        }
    })
}

/// Fire every currently-due task once. Factored out of the loop so tests can
/// drive a single sweep deterministically without waiting on a timer. Returns
/// the [`RunRecord`]s it appended (newest fire last) so the caller can emit the
/// FE-facing `scheduled:fired` events without re-reading the store (#544).
///
/// Each fire is panic-isolated: a `fire()` that panics (e.g. a desktop helper on
/// a misconfigured task) is caught, recorded as an `Error` run, and stamped like
/// any other outcome, so one bad task cannot unwind and silently kill the
/// long-lived scheduler loop. Stamping a caught panic also prevents it from
/// hot-looping (it stays due until stamped).
pub async fn run_due_once(store: &ScheduledStore, runner: &dyn TaskRunner) -> Vec<RunRecord> {
    let mut records = Vec::new();
    for task in store.due_tasks() {
        let fired_ms = chrono::Local::now().timestamp_millis();
        let outcome = match AssertUnwindSafe(runner.fire(&task)).catch_unwind().await {
            Ok(outcome) => outcome,
            Err(_) => {
                tracing::error!(task = %task.id, "scheduled fire panicked; recording error and continuing");
                RunOutcome {
                    session_id: None,
                    status: RunStatus::Error,
                }
            }
        };
        let record = store.append_run(&task.id, outcome.session_id.as_deref(), outcome.status);
        store.stamp_last_run(&task.id, fired_ms);
        records.push(record);
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ff_core::{CreateScheduledTaskInput, SafetyCeiling, TaskKind};

    struct CountingRunner {
        fires: AtomicUsize,
        status: RunStatus,
    }

    impl CountingRunner {
        fn new(status: RunStatus) -> Self {
            Self {
                fires: AtomicUsize::new(0),
                status,
            }
        }
    }

    #[async_trait]
    impl TaskRunner for CountingRunner {
        async fn fire(&self, _task: &ScheduledTask) -> RunOutcome {
            self.fires.fetch_add(1, Ordering::SeqCst);
            RunOutcome {
                session_id: Some("sess-1".into()),
                status: self.status,
            }
        }
    }

    fn every_minute_task(name: &str) -> CreateScheduledTaskInput {
        CreateScheduledTaskInput {
            name: name.into(),
            cron: "0 * * * * *".into(),
            kind: TaskKind::Prompt("do the thing".into()),
            workspace: None,
            profile: None,
            safety_ceiling: SafetyCeiling::ReadOnly,
            catch_up: None,
        }
    }

    #[tokio::test]
    async fn due_task_fires_once_stamps_last_run_and_records_a_run() {
        let store = ScheduledStore::open_in_memory().unwrap();
        store.create(every_minute_task("t")).unwrap();
        let runner = CountingRunner::new(RunStatus::Ok);

        let fired = run_due_once(&store, &runner).await;

        assert_eq!(runner.fires.load(Ordering::SeqCst), 1);
        assert_eq!(
            fired.len(),
            1,
            "the appended record is returned for emission"
        );
        assert_eq!(fired[0].status, RunStatus::Ok);
        assert_eq!(fired[0].session_id.as_deref(), Some("sess-1"));
        // last_run is stamped, so the task is no longer due on the next sweep.
        let second = run_due_once(&store, &runner).await;
        assert_eq!(
            runner.fires.load(Ordering::SeqCst),
            1,
            "a stamped task must not re-fire within the same minute"
        );
        assert!(second.is_empty(), "nothing due, no records returned");
        let task = &store.list()[0];
        assert!(task.last_run.is_some(), "last_run must be stamped");
    }

    #[tokio::test]
    async fn needs_attention_status_is_recorded() {
        let store = ScheduledStore::open_in_memory().unwrap();
        store.create(every_minute_task("t")).unwrap();
        let runner = CountingRunner::new(RunStatus::NeedsAttention);

        run_due_once(&store, &runner).await;

        assert_eq!(runner.fires.load(Ordering::SeqCst), 1);
        // The loop does not hang on a NeedsAttention outcome; it stamps and moves on.
        assert!(store.list()[0].last_run.is_some());
    }

    #[tokio::test]
    async fn paused_task_does_not_fire() {
        let store = ScheduledStore::open_in_memory().unwrap();
        let task = store.create(every_minute_task("t")).unwrap();
        store.toggle_paused(&task.id);
        let runner = CountingRunner::new(RunStatus::Ok);

        run_due_once(&store, &runner).await;

        assert_eq!(runner.fires.load(Ordering::SeqCst), 0);
    }

    /// A fire that panics must not unwind the scheduler loop: the panic is
    /// caught, recorded as an `Error` run, and `last_run` is stamped so the task
    /// does not hot-loop and the next due task still fires.
    #[tokio::test]
    async fn panicking_fire_is_isolated_recorded_and_stamped() {
        struct PanickingRunner;
        #[async_trait]
        impl TaskRunner for PanickingRunner {
            async fn fire(&self, _task: &ScheduledTask) -> RunOutcome {
                panic!("boom");
            }
        }

        let store = ScheduledStore::open_in_memory().unwrap();
        store.create(every_minute_task("t")).unwrap();

        // Does not panic out of run_due_once.
        run_due_once(&store, &PanickingRunner).await;

        let task = &store.list()[0];
        assert!(
            task.last_run.is_some(),
            "a caught panic must still stamp last_run so the task does not hot-loop"
        );
        let statuses = store.run_statuses(&task.id);
        assert_eq!(
            statuses,
            vec!["error".to_string()],
            "a caught panic is recorded as one error run"
        );
    }
}
