//! `flowforge` — the headless FlowForge CLI. Drives the same `ff_agent::run_turn`
//! loop the desktop app uses, rendering agent events to the terminal instead of a
//! webview. See `docs/rfcs/0004-cli.md`.
//!
//! Tier-1 platforms: macOS + Linux. Windows is best-effort via WSL (the `bash` tool
//! assumes a POSIX shell).

mod approver;
mod host;
mod json_events;

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
        /// Emit each event as one JSON line to stdout; no human-only text is printed.
        #[arg(long)]
        json: bool,
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
        Command::Run { prompt, json } => run(prompt, json).await,
        Command::Skills { command } => match command {
            SkillsCommand::List => skills_list(),
        },
    }
}

fn skills_list() -> ExitCode {
    let skills = host::load_skills();
    let names = skills.names();
    if names.is_empty() {
        eprintln!(
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

async fn run(prompt: String, json: bool) -> ExitCode {
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

    let result = if json {
        run_turn(
            provider.as_ref(),
            &store,
            &tool_ctx,
            &session.id,
            &model,
            Some(system_prompt.as_str()),
            cancel,
            |event| {
                json_events::emit_line(&event);
            },
        )
        .await
    } else {
        run_turn(
            provider.as_ref(),
            &store,
            &tool_ctx,
            &session.id,
            &model,
            Some(system_prompt.as_str()),
            cancel,
            render_event_text,
        )
        .await
    };

    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Plain-text renderer: assistant tokens stream to stdout; tool steps are annotated
/// on stderr so piping stdout yields just the model's text.
fn render_event_text(event: AgentEvent) {
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
    use super::json_events;
    use super::Cli;
    use async_trait::async_trait;
    use clap::CommandFactory;
    use ff_agent::{run_turn, AgentEvent, Approver, CancelToken, ToolContext};
    use ff_core::Role;
    use ff_llm::{ChatRequest, Chunk, ChunkStream, LlmError, Provider};
    use ff_memory::MemoryStore;
    use ff_tools::{Safety, ToolRegistry};
    use futures_util::StreamExt;

    /// Validates the whole clap command tree (names, args, conflicts) at test time.
    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    /// Golden-line snapshot: each AgentEvent serialises to one JSON object on stdout;
    /// every line is jq-parseable with a consistent shape.
    #[test]
    fn json_mode_golden_lines() {
        let events: Vec<AgentEvent> = vec![
            AgentEvent::Token {
                message_id: "m1".into(),
                delta: "Hello ".into(),
            },
            AgentEvent::ToolCallStarted {
                message_id: "m1".into(),
                call_id: "c1".into(),
                name: "bash".into(),
                args: serde_json::json!({"command": "echo hi"}),
            },
            AgentEvent::ToolCallFinished {
                message_id: "m1".into(),
                call_id: "c1".into(),
                success: true,
                result: "hi\n".into(),
            },
            AgentEvent::Done {
                message_id: "m1".into(),
                final_message: Some("Hello world!".into()),
                turns: Some(2),
                token_count: None,
            },
        ];

        for event in &events {
            let line = serde_json::to_string(event).expect("serializable");
            assert!(line.starts_with('{'), "one JSON object per event");
            assert!(!line.contains('\n'), "no embedded newlines");

            let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid JSON object");
            let obj = parsed.as_object().unwrap();
            assert_eq!(obj.len(), 1, "exactly one discriminator key");

            // The inner payload carries message_id for every event.
            let inner = obj.values().next().unwrap().as_object().unwrap();
            assert!(inner.get("message_id").is_some());
        }
    }

    struct TestApprover;

    #[async_trait]
    impl Approver for TestApprover {
        async fn approve(
            &self,
            _message_id: &str,
            _call_id: &str,
            _name: &str,
            _safety: Safety,
            _args: &serde_json::Value,
        ) -> bool {
            true
        }
    }

    struct JsonTextProvider;

    #[async_trait]
    impl Provider for JsonTextProvider {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let chunks = vec![
                Ok(Chunk {
                    delta: "clean ".into(),
                    ..Chunk::default()
                }),
                Ok(Chunk {
                    delta: "json".into(),
                    done: true,
                    ..Chunk::default()
                }),
            ];
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    #[tokio::test]
    async fn json_run_output_ends_with_single_discriminated_done() {
        let store = MemoryStore::new();
        let session = store.create_session(None);
        store.add_message(&session.id, Role::User, "say json".into());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approver = TestApprover;
        let tool_ctx = ToolContext {
            registry: &registry,
            root: &root,
            approve: &approver,
            max_iterations: 8,
        };

        let mut stdout = Vec::new();
        let msg = run_turn(
            &JsonTextProvider,
            &store,
            &tool_ctx,
            &session.id,
            "mock",
            None,
            CancelToken::new(),
            |event| {
                json_events::emit_line_to(&event, &mut stdout).expect("write JSON event");
            },
        )
        .await
        .unwrap();

        assert_eq!(msg.content, "clean json");

        let output = String::from_utf8(stdout).expect("utf8 output");
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 3, "two token records plus one terminal Done");

        for line in &lines {
            assert!(
                serde_json::from_str::<serde_json::Value>(line).is_ok(),
                "stdout contains only JSON lines: {line:?}"
            );
        }

        let done_lines: Vec<serde_json::Value> = lines
            .iter()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|value| value.get("Done").is_some())
            .collect();
        assert_eq!(done_lines.len(), 1, "exactly one terminal Done record");

        let terminal = done_lines[0]["Done"].as_object().unwrap();
        assert_eq!(terminal["final_message"], "clean json");
        assert_eq!(terminal["turns"], 1);
        assert!(
            terminal.get("tool_count").is_none(),
            "terminal record uses the discriminated AgentEvent schema only"
        );
    }
}
