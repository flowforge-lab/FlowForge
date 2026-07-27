use super::*;
use ff_core::{CreateScheduledTaskInput, RunStatus, SafetyCeiling, ScheduledTask, TaskKind};
use ff_scheduled::ScheduledStore;

struct FakeRunner {
    status: RunStatus,
}

#[async_trait::async_trait]
impl TaskRunner for FakeRunner {
    async fn fire(&self, _task: &ScheduledTask) -> ff_scheduled::RunOutcome {
        ff_scheduled::RunOutcome {
            session_id: Some("sess-test".into()),
            status: self.status,
        }
    }
}

#[tokio::test]
async fn add_list_run_round_trip() {
    let store = ScheduledStore::open_in_memory().unwrap();

    // Add
    let code = task_add(
        &store,
        "digest".into(),
        "0 0 9 * * *".into(),
        "summarize logs".into(),
        "read_only".into(),
    )
    .await;
    assert_eq!(code, ExitCode::SUCCESS);

    let tasks = store.list();
    assert_eq!(tasks.len(), 1);
    let task = &tasks[0];
    assert_eq!(task.name, "digest");
    assert_eq!(task.safety_ceiling, SafetyCeiling::ReadOnly);

    // Run with Ok outcome
    let runner = FakeRunner {
        status: RunStatus::Ok,
    };
    let code = task_run(&store, task.id.clone(), &runner).await;
    assert_eq!(code, ExitCode::SUCCESS);

    // Verify run record and last_run stamp
    let runs = store.runs(&task.id, 10);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunStatus::Ok);
    assert_eq!(runs[0].session_id.as_deref(), Some("sess-test"));
    let task_after = store.get(&task.id).unwrap();
    assert!(task_after.last_run.is_some());
}

#[tokio::test]
async fn run_failure_returns_nonzero() {
    let store = ScheduledStore::open_in_memory().unwrap();
    let t = store
        .create(CreateScheduledTaskInput {
            name: "faily".into(),
            cron: "0 0 9 * * *".into(),
            kind: TaskKind::Prompt("break".into()),
            workspace: None,
            profile: None,
            safety_ceiling: SafetyCeiling::ReadOnly,
            catch_up: None,
        })
        .unwrap();

    let runner = FakeRunner {
        status: RunStatus::Error,
    };
    let code = task_run(&store, t.id.clone(), &runner).await;
    assert_eq!(code, ExitCode::FAILURE);

    let runs = store.runs(&t.id, 10);
    assert_eq!(runs[0].status, RunStatus::Error);
}

#[tokio::test]
async fn run_needs_attention_returns_nonzero() {
    let store = ScheduledStore::open_in_memory().unwrap();
    let t = store
        .create(CreateScheduledTaskInput {
            name: "asky".into(),
            cron: "0 0 9 * * *".into(),
            kind: TaskKind::Prompt("ask user".into()),
            workspace: None,
            profile: None,
            safety_ceiling: SafetyCeiling::ReadOnly,
            catch_up: None,
        })
        .unwrap();

    let runner = FakeRunner {
        status: RunStatus::NeedsAttention,
    };
    let code = task_run(&store, t.id.clone(), &runner).await;
    assert_eq!(code, ExitCode::FAILURE);

    let runs = store.runs(&t.id, 10);
    assert_eq!(runs[0].status, RunStatus::NeedsAttention);
}
