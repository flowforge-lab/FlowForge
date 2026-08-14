use std::process::ExitCode;

use clap::{Args, Subcommand};
use ff_agent::{drive_goal, LoopStop};
use ff_core::{Goal, GoalStatus, GoalStore};

use crate::goal_loop::CliGoalIteration;

#[derive(Debug, Args)]
pub struct GoalArgs {
    /// The objective to achieve (required when no subcommand is given).
    pub(crate) objective: Option<String>,
    /// Optional session ID to resume an existing goal.
    #[arg(long, value_name = "ID")]
    pub(crate) session: Option<String>,
    /// Max iterations (default 40).
    #[arg(long)]
    pub(crate) max_iterations: Option<u32>,
    #[command(subcommand)]
    pub(crate) command: Option<GoalSubCommand>,
}

#[derive(Debug, Subcommand)]
pub enum GoalSubCommand {
    /// List existing goals (sessions).
    List,
    /// Resume a paused goal.
    Resume {
        /// Session ID of the goal to resume.
        session: String,
    },
    /// Cancel a running goal (pauses it for later resume).
    Cancel {
        /// Session ID of the goal to cancel.
        session: String,
    },
}

pub async fn run(args: GoalArgs) -> ExitCode {
    match args.command {
        None => {
            let objective = match args.objective {
                Some(o) => o,
                None => {
                    eprintln!("error: objective required when no subcommand is given");
                    eprintln!("Usage: ff goal <OBJECTIVE>");
                    eprintln!("       ff goal list");
                    eprintln!("       ff goal resume <SESSION>");
                    eprintln!("       ff goal cancel <SESSION>");
                    return ExitCode::FAILURE;
                }
            };
            goal_start(objective, args.session, args.max_iterations).await
        }
        Some(GoalSubCommand::List) => goal_list(),
        Some(GoalSubCommand::Resume { session }) => goal_resume(session).await,
        Some(GoalSubCommand::Cancel { session }) => goal_cancel(session).await,
    }
}

fn goal_store() -> GoalStore {
    GoalStore::new(ff_core::goal_store_dir())
}

async fn goal_start(
    objective: String,
    session: Option<String>,
    max_iterations: Option<u32>,
) -> ExitCode {
    let session_id = session.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let store = goal_store();

    let mut goal = match store.load(&session_id) {
        Ok(Some(g)) => g,
        Ok(None) => {
            let now = chrono::Local::now().timestamp_millis();
            Goal {
                session_id: session_id.clone(),
                objective: objective.clone(),
                status: GoalStatus::Active,
                iteration: 0,
                budget: ff_core::GoalBudget {
                    max_iterations: max_iterations.unwrap_or(ff_core::DEFAULT_MAX_ITERATIONS),
                    max_tokens: None,
                    max_wall_ms: None,
                },
                spent: ff_core::GoalSpend::default(),
                ledger: Vec::new(),
                pending_steer: None,
                verify_cmd: None,
                created_ms: now,
                updated_ms: now,
            }
        }
        Err(e) => {
            eprintln!("error: failed to read goal file: {}", e);
            return ExitCode::FAILURE;
        }
    };

    if goal.status != GoalStatus::Active && goal.status != GoalStatus::Paused {
        eprintln!(
            "error: goal {} is not Active or Paused (status: {:?})",
            session_id, goal.status
        );
        return ExitCode::FAILURE;
    }

    goal.objective = objective.clone();
    goal.status = GoalStatus::Active;

    let cancel = ff_agent::CancelToken::new();
    let cancel_signal = cancel.clone();
    let handle = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\n[cancelled]");
            cancel_signal.cancel();
        }
    });

    let iter = CliGoalIteration::with_cancel(Some(cancel)).await;

    eprintln!("Starting goal: {}", objective);
    eprintln!("Session: {}", session_id);

    let result = drive_goal(&mut goal, &iter).await;

    handle.abort();

    match result {
        LoopStop::Completed => {
            println!("\nGoal completed: {}", objective);
            ExitCode::SUCCESS
        }
        LoopStop::Exhausted => {
            eprintln!("\nGoal exhausted (budget reached)");
            ExitCode::FAILURE
        }
        LoopStop::Paused => {
            eprintln!(
                "\nGoal paused (use `ff goal resume --session {}` to continue)",
                session_id
            );
            ExitCode::SUCCESS
        }
        LoopStop::Failed => {
            eprintln!("\nGoal failed");
            ExitCode::FAILURE
        }
    }
}

fn goal_list() -> ExitCode {
    let store = goal_store();

    let entries = match std::fs::read_dir(store.dir()) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("No goals found");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: could not read goals directory: {}", e);
            return ExitCode::FAILURE;
        }
    };

    let mut goals = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(goal) = serde_json::from_str::<Goal>(&content) {
                    goals.push(goal);
                }
            }
        }
    }

    if goals.is_empty() {
        println!("No goals found");
        return ExitCode::SUCCESS;
    }

    println!("Goals:\n");
    for goal in goals {
        println!(
            "  {:8}  {:12}  {}",
            format!("{:?}", goal.status).to_lowercase(),
            format!("iter {}", goal.iteration),
            goal.objective.chars().take(60).collect::<String>()
        );
        println!("  Session: {}\n", goal.session_id);
    }

    ExitCode::SUCCESS
}

async fn goal_resume(session: String) -> ExitCode {
    let store = goal_store();

    let mut goal = match store.load(&session) {
        Ok(Some(g)) => g,
        Ok(None) => {
            eprintln!("error: goal not found: {}", session);
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("error: failed to read goal file: {}", e);
            return ExitCode::FAILURE;
        }
    };

    if goal.status != GoalStatus::Paused {
        eprintln!("error: goal is not Paused (status: {:?})", goal.status);
        return ExitCode::FAILURE;
    }

    goal.status = GoalStatus::Active;

    let cancel = ff_agent::CancelToken::new();
    let cancel_signal = cancel.clone();
    let handle = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\n[cancelled]");
            cancel_signal.cancel();
        }
    });

    let iter = CliGoalIteration::with_cancel(Some(cancel)).await;

    eprintln!("Resuming goal: {}", goal.objective);

    let result = drive_goal(&mut goal, &iter).await;

    handle.abort();

    match result {
        LoopStop::Completed => {
            println!("\nGoal completed");
            ExitCode::SUCCESS
        }
        LoopStop::Exhausted => {
            eprintln!("\nGoal exhausted (budget reached)");
            ExitCode::FAILURE
        }
        LoopStop::Paused => {
            eprintln!("\nGoal paused");
            ExitCode::SUCCESS
        }
        LoopStop::Failed => {
            eprintln!("\nGoal failed");
            ExitCode::FAILURE
        }
    }
}

async fn goal_cancel(session: String) -> ExitCode {
    let store = goal_store();

    let mut goal = match store.load(&session) {
        Ok(Some(g)) => g,
        Ok(None) => {
            eprintln!("error: goal not found: {}", session);
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("error: failed to parse goal: {}", e);
            return ExitCode::FAILURE;
        }
    };

    if goal.status != GoalStatus::Active {
        eprintln!("error: goal is not Active (status: {:?})", goal.status);
        return ExitCode::FAILURE;
    }

    goal.status = GoalStatus::Paused;
    goal.updated_ms = chrono::Local::now().timestamp_millis();

    if let Err(e) = store.save(&goal) {
        eprintln!("warning: failed to save goal: {}", e);
    }

    eprintln!("Goal paused: {}", goal.objective);

    ExitCode::SUCCESS
}
