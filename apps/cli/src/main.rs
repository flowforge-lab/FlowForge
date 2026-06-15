//! `flowforge` — the headless FlowForge CLI. Drives the same `ff_agent::run_turn`
//! loop the desktop app uses, rendering agent events to the terminal instead of a
//! webview. See `docs/rfcs/0004-cli.md`.
//!
//! Tier-1 platforms: macOS + Linux. Windows is best-effort via WSL (the `bash` tool
//! assumes a POSIX shell).

mod approver;
mod host;

use std::io::Write;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ff_agent::{run_turn, AgentEvent, CancelToken, ToolContext, UserContext};
use ff_core::Role;

use crate::approver::CliApprover;

/// FlowForge on the command line: run an agent turn, inspect skills, no GUI.
#[derive(Parser)]
#[command(name = "flowforge", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a single agent turn against a prompt and stream the result.
    Run {
        /// The instruction for the agent (quote multi-word prompts).
        prompt: String,
    },
    /// Inspect installed skills (shared with the desktop app).
    Skills {
        #[command(subcommand)]
        command: SkillsCommand,
    },
}

#[derive(Subcommand)]
enum SkillsCommand {
    /// List installed skills and their descriptions.
    List,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { prompt } => run(prompt).await,
        Command::Skills { command } => match command {
            SkillsCommand::List => skills_list(),
        },
    }
}

fn skills_list() -> ExitCode {
    let skills = host::load_skills();
    let names = skills.names();
    if names.is_empty() {
        println!(
            "No skills installed (looked in {}).",
            host::skills_root().display()
        );
        return ExitCode::SUCCESS;
    }
    for name in names {
        match skills.get(name) {
            Some(skill) => println!("{name}\t{}", skill.manifest.description),
            None => println!("{name}"),
        }
    }
    ExitCode::SUCCESS
}

async fn run(prompt: String) -> ExitCode {
    let (provider, model) = host::load_provider();
    let skills = host::load_skills();
    let workspace = host::workspace_root();
    let store = ff_memory::MemoryStore::new();
    let registry = ff_tools::ToolRegistry::with_defaults();
    let approver = CliApprover;

    let session = store.create_session(None);
    store.add_message(&session.id, Role::User, prompt);

    let user_ctx = UserContext::now();
    let system_prompt = ff_agent::build_system_prompt(None, &skills, &[], &user_ctx);

    let tool_ctx = ToolContext {
        registry: &registry,
        root: &workspace,
        approve: &approver,
        max_iterations: 8,
    };

    let cancel = CancelToken::new();
    // Ctrl-C cancels the turn cooperatively. `ctrl_c()` is portable across Unix and
    // Windows, so cancellation works the same on Tier-1 platforms and WSL.
    let cancel_signal = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\n[cancelled]");
            cancel_signal.cancel();
        }
    });

    let result = run_turn(
        provider.as_ref(),
        &store,
        &tool_ctx,
        &session.id,
        &model,
        Some(system_prompt.as_str()),
        cancel,
        render_event,
    )
    .await;

    println!();
    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Plain-text renderer: assistant tokens stream to stdout; tool steps are annotated
/// on stderr so piping stdout yields just the model's text. A `--json` event stream
/// is tracked under the CLI epic.
fn render_event(event: AgentEvent) {
    match event {
        AgentEvent::Token { delta, .. } => {
            print!("{delta}");
            let _ = std::io::stdout().flush();
        }
        AgentEvent::ToolCallStarted { name, .. } => {
            eprintln!("\n[tool] {name} ...");
        }
        AgentEvent::ToolCallFinished {
            success, result, ..
        } => {
            eprintln!("[tool] -> {}", if success { "ok" } else { "failed" });
            if !success {
                let snippet: String = result.chars().take(200).collect();
                eprintln!("{snippet}");
            }
        }
        AgentEvent::Done { .. } => {}
        AgentEvent::Error { message } => {
            eprintln!("\n[error] {message}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    /// Validates the whole clap command tree (names, args, conflicts) at test time —
    /// the idiomatic guard against an ill-formed CLI definition.
    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
