use std::path::PathBuf;
use std::process::ExitCode;

use clap::Subcommand;
use ff_core::{CreateScheduledTaskInput, RunStatus, SafetyCeiling, TaskKind};
use ff_scheduled::{ScheduledStore, TaskRunner};

#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// List all scheduled tasks.
    List,
    /// Add a new scheduled task.
    Add {
        /// Task name.
        name: String,
        /// Cron expression (e.g., "0 0 * * * *" for daily at midnight).
        cron: String,
        /// The prompt to run.
        prompt: String,
        /// Safety ceiling: read_only (default) or write.
        #[arg(long, default_value = "read_only")]
        ceiling: String,
    },
    /// Run a task immediately (bypasses schedule).
    Run {
        /// Task ID to run.
        id: String,
    },
    /// Toggle pause state of a task.
    Pause {
        /// Task ID to pause/resume.
        id: String,
    },
    /// Delete a scheduled task.
    Delete {
        /// Task ID to delete.
        id: String,
    },
}

pub async fn run(command: TaskCommand) -> ExitCode {
    let store = match open_store() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {}", e);
            return ExitCode::FAILURE;
        }
    };

    match command {
        TaskCommand::List => task_list(&store),
        TaskCommand::Add {
            name,
            cron,
            prompt,
            ceiling,
        } => task_add(&store, name, cron, prompt, ceiling).await,
        TaskCommand::Run { id } => {
            let runner = crate::task_runner::CliTaskRunner::new();
            task_run(&store, id, &runner).await
        }
        TaskCommand::Pause { id } => task_pause(&store, id),
        TaskCommand::Delete { id } => task_delete(&store, id),
    }
}

fn store_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flowforge")
        .join("scheduled.db")
}

fn open_store() -> Result<ScheduledStore, String> {
    let path = store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    ScheduledStore::open(&path).map_err(|e| e.to_string())
}

fn task_list(store: &ScheduledStore) -> ExitCode {
    let tasks = store.list();

    if tasks.is_empty() {
        println!("No scheduled tasks");
        return ExitCode::SUCCESS;
    }

    println!("Scheduled Tasks:\n");
    for task in &tasks {
        println!(
            "  {:8}  {:20}  {:15}  {}",
            if task.paused { "paused" } else { "active" },
            task.cadence_label,
            task.id.chars().take(8).collect::<String>(),
            task.name
        );
        if let TaskKind::Prompt(p) = &task.kind {
            println!("    Prompt: {}", p.chars().take(60).collect::<String>());
        }
        if let Some(next) = task.next_run {
            if let Some(dt) = chrono::DateTime::from_timestamp_millis(next) {
                println!("    Next run: {}", dt.format("%Y-%m-%d %H:%M"));
            } else {
                println!("    Next run: {}", next);
            }
        }
        println!();
    }

    ExitCode::SUCCESS
}

async fn task_add(
    store: &ScheduledStore,
    name: String,
    cron: String,
    prompt: String,
    ceiling: String,
) -> ExitCode {
    let safety_ceiling = match ceiling.as_str() {
        "write" => SafetyCeiling::Write,
        _ => SafetyCeiling::ReadOnly,
    };

    let input = CreateScheduledTaskInput {
        name,
        cron,
        kind: TaskKind::Prompt(prompt),
        workspace: None,
        profile: None,
        safety_ceiling,
        catch_up: None,
    };

    match store.create(input) {
        Ok(task) => {
            println!("Created task: {} ({})", task.name, task.id);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::FAILURE
        }
    }
}

pub(crate) async fn task_run(
    store: &ScheduledStore,
    id: String,
    runner: &dyn TaskRunner,
) -> ExitCode {
    let task = match store.get(&id) {
        Some(t) => t,
        None => {
            eprintln!("error: task not found: {}", id);
            return ExitCode::FAILURE;
        }
    };

    println!("Running task: {} ({})", task.name, task.id);
    println!(
        "(Use `ff run \"{}\"` to execute the prompt directly)",
        if let TaskKind::Prompt(p) = &task.kind {
            p
        } else {
            ""
        }
    );

    let fired_ms = chrono::Local::now().timestamp_millis();
    let outcome = runner.fire(&task).await;
    let status = outcome.status;

    store.append_run(&task.id, outcome.session_id.as_deref(), status);
    store.stamp_last_run(&task.id, fired_ms);

    println!("Task completed with status: {:?}", status);

    if status == RunStatus::Error || status == RunStatus::NeedsAttention {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn task_pause(store: &ScheduledStore, id: String) -> ExitCode {
    match store.toggle_paused(&id) {
        Some(task) => {
            println!(
                "Task {} is now {}",
                task.name,
                if task.paused { "paused" } else { "active" }
            );
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("error: task not found: {}", id);
            ExitCode::FAILURE
        }
    }
}

fn task_delete(store: &ScheduledStore, id: String) -> ExitCode {
    match store.delete(&id) {
        Ok(true) => {
            println!("Deleted task: {}", id);
            ExitCode::SUCCESS
        }
        Ok(false) => {
            eprintln!("error: task not found: {}", id);
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
