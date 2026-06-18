//! `flowforge` — the headless FlowForge CLI. Drives the same `ff_agent::run_turn`
//! loop the desktop app uses, rendering agent events to the terminal instead of a
//! webview. See `docs/rfcs/0004-cli.md`.
//!
//! Tier-1 platforms: macOS + Linux. Windows is best-effort via WSL (the `bash` tool
//! assumes a POSIX shell).

mod approver;
mod host;
mod json_events;

use std::io::{BufRead, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ff_agent::{run_turn, AgentEvent, CancelToken, ToolContext, UserContext};
use ff_core::Role;

use crate::approver::{ApprovalMode, CliApprover};

/// FlowForge on the command line: run an agent turn, inspect skills, no GUI.
/// With no subcommand, opens an interactive REPL (multi-turn chat).
#[derive(Parser)]
#[command(name = "flowforge", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
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
        /// Auto-approve write and dangerous tool calls without prompting.
        #[arg(long, conflicts_with = "deny")]
        yes: bool,
        /// Auto-deny write and dangerous tool calls without prompting.
        #[arg(long, conflicts_with = "yes")]
        deny: bool,
    },
    /// Open an interactive REPL (multi-turn, in-process session). Default when
    /// no subcommand is given. Type `exit` or press Ctrl-D to quit.
    Chat {
        /// Emit each event as one JSON line to stdout; no human-only text is printed.
        #[arg(long)]
        json: bool,
        /// Auto-approve write and dangerous tool calls without prompting.
        #[arg(long, conflicts_with = "deny")]
        yes: bool,
        /// Auto-deny write and dangerous tool calls without prompting.
        #[arg(long, conflicts_with = "yes")]
        deny: bool,
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
    match cli.command.unwrap_or(Command::Chat {
        json: false,
        yes: false,
        deny: false,
    }) {
        Command::Run {
            prompt,
            json,
            yes,
            deny,
        } => run(prompt, json, approval_mode(yes, deny)).await,
        Command::Chat { json, yes, deny } => chat(json, approval_mode(yes, deny)).await,
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

fn approval_mode(yes: bool, deny: bool) -> ApprovalMode {
    match (yes, deny) {
        (true, false) => ApprovalMode::Yes,
        (false, true) => ApprovalMode::Deny,
        _ => ApprovalMode::Prompt,
    }
}

/// Shared durable-memory setup (RFC 0006). Builds the store + FTS5 index, does a
/// full reindex from disk, and registers the three memory tools. Best-effort: an
/// index failure leaves the ambient block working but skips the recall tools.
fn build_registry_with_memory() -> (ff_tools::ToolRegistry, std::sync::Arc<ff_memory::Memory>) {
    let mut registry = ff_tools::ToolRegistry::with_defaults();
    let memory_store = std::sync::Arc::new(ff_memory::Memory::with_default_root(
        ff_memory::MemoryConfig::default(),
    ));
    if let Ok(index) = ff_memory::Fts5Index::open(memory_store.index_path()) {
        let index: std::sync::Arc<dyn ff_memory::MemoryIndex> = std::sync::Arc::new(index);
        let _ = ff_memory::MemoryIndex::reindex(index.as_ref(), &memory_store.all_chunks());
        registry.register(Box::new(ff_tools::memory::MemorySearchTool::new(
            memory_store.clone(),
            index.clone(),
        )));
        registry.register(Box::new(ff_tools::memory::MemoryGetTool::new(
            memory_store.clone(),
        )));
        registry.register(Box::new(ff_tools::memory::MemoryWriteTool::new(
            memory_store.clone(),
            index.clone(),
        )));
    }
    (registry, memory_store)
}

async fn run(prompt: String, json: bool, approval_mode: ApprovalMode) -> ExitCode {
    let (provider, model) = host::load_provider();
    let skills = host::load_skills();
    let workspace = host::workspace_root();
    let store = ff_session::SessionStore::new();
    let (registry, memory_store) = build_registry_with_memory();
    let approver = CliApprover::new(approval_mode);

    let session = store.create_session(None);
    store.add_message(&session.id, Role::User, prompt);

    let user_ctx = UserContext::now();
    let memory = memory_store.ambient_block();
    let system_prompt =
        ff_agent::build_system_prompt(None, &skills, &[], &user_ctx, memory.as_deref());

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
            true,
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
            true,
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

/// Interactive REPL (multi-turn, one in-process session). Keeps a single
/// [`ff_session::SessionStore`] alive for the life of the process so each turn
/// sees the full accumulated history. Loops until EOF, `exit`, or `quit`.
async fn chat(json: bool, approval_mode: ApprovalMode) -> ExitCode {
    let (provider, model) = host::load_provider();
    let skills = host::load_skills();
    let workspace = host::workspace_root();
    let store = ff_session::SessionStore::new();
    let (registry, memory_store) = build_registry_with_memory();
    let approver = CliApprover::new(approval_mode);
    let session = store.create_session(None);

    let tool_ctx = ToolContext {
        registry: &registry,
        root: &workspace,
        approve: &approver,
        max_iterations: 8,
    };

    let stdin = std::io::stdin();
    chat_repl(
        provider.as_ref(),
        &model,
        &skills,
        &store,
        &memory_store,
        &tool_ctx,
        &session.id,
        json,
        stdin.lock(),
    )
    .await
}

/// Core REPL loop with injectable input for testability. Reads lines from `input`,
/// dispatches each to [`run_turn`], and loops until EOF, `exit`, or `quit`.
#[allow(clippy::too_many_arguments)]
async fn chat_repl(
    provider: &dyn ff_llm::Provider,
    model: &str,
    skills: &ff_skills::SkillRegistry,
    store: &ff_session::SessionStore,
    memory_store: &std::sync::Arc<ff_memory::Memory>,
    tool_ctx: &ToolContext<'_>,
    session_id: &str,
    json: bool,
    mut input: impl BufRead,
) -> ExitCode {
    if !json {
        eprintln!("FlowForge REPL.  Type `exit` or Ctrl-D to quit.\n");
    }

    loop {
        // Prompt on stderr so stdout stays clean (both plain-text and JSON mode).
        if !json {
            eprint!("> ");
            let _ = std::io::stderr().flush();
        }

        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(0) => {
                // EOF (Ctrl-D)
                if !json {
                    eprintln!();
                }
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("read error: {e}");
                break;
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "exit" || trimmed == "quit" {
            break;
        }

        store.add_message(session_id, Role::User, trimmed.to_string());

        let user_ctx = UserContext::now();
        let memory = memory_store.ambient_block();
        let system_prompt =
            ff_agent::build_system_prompt(None, skills, &[], &user_ctx, memory.as_deref());

        let cancel = CancelToken::new();
        let cancel_signal = cancel.clone();
        let ctrlc_handle = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancel_signal.cancel();
            }
        });

        let result = if json {
            run_turn(
                provider,
                store,
                tool_ctx,
                session_id,
                model,
                Some(system_prompt.as_str()),
                true,
                cancel,
                |event| json_events::emit_line(&event),
            )
            .await
        } else {
            run_turn(
                provider,
                store,
                tool_ctx,
                session_id,
                model,
                Some(system_prompt.as_str()),
                true,
                cancel,
                render_event_text,
            )
            .await
        };

        // Cancel the ctrl-c listener now that the turn is done so a stray signal
        // from a previous turn cannot fire during the next prompt.
        ctrlc_handle.abort();

        match result {
            Ok(_) => {}
            Err(e) => {
                eprintln!("\n[error] {e}");
            }
        }

        if !json {
            eprintln!();
        }
    }

    ExitCode::SUCCESS
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
        AgentEvent::Reasoning { .. } => {}
        AgentEvent::Error { message } => {
            eprintln!("\n[error] {message}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;

    use super::json_events;
    use super::{approval_mode, Cli};
    use crate::approver::ApprovalMode;
    use async_trait::async_trait;
    use clap::CommandFactory;
    use clap::Parser;
    use ff_agent::{run_turn, AgentEvent, Approver, CancelToken, ToolContext};
    use ff_core::Role;
    use ff_llm::{ChatRequest, Chunk, ChunkStream, LlmError, Provider};
    use ff_memory::{Memory, MemoryConfig};
    use ff_session::SessionStore;
    use ff_skills::SkillRegistry;
    use ff_tools::{Safety, ToolRegistry};
    use futures_util::StreamExt;

    /// Validates the whole clap command tree (names, args, conflicts) at test time.
    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn run_approval_flags_map_to_modes() {
        assert_eq!(approval_mode(false, false), ApprovalMode::Prompt);
        assert_eq!(approval_mode(true, false), ApprovalMode::Yes);
        assert_eq!(approval_mode(false, true), ApprovalMode::Deny);
    }

    #[test]
    fn yes_and_deny_flags_conflict() {
        let err = match Cli::try_parse_from(["flowforge", "run", "hi", "--yes", "--deny"]) {
            Ok(_) => panic!("--yes and --deny must be mutually exclusive"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
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
        let store = SessionStore::new();
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
            false,
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

    /// Captures the `ChatRequest` messages from each turn so a test can assert what
    /// the provider received (proving multi-turn context). The most recent request
    /// wins; a test asserts against the last value.
    struct RecordingProvider {
        seen: std::sync::Arc<std::sync::Mutex<Vec<ff_llm::ChatMessage>>>,
    }

    #[async_trait]
    impl Provider for RecordingProvider {
        async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
            *self.seen.lock().unwrap() = req.messages;
            Ok(futures_util::stream::iter(vec![Ok(Chunk {
                delta: "ok".into(),
                done: true,
                ..Chunk::default()
            })])
            .boxed())
        }
    }

    /// Feed two prompts through the REPL and assert the second turn's provider
    /// request includes the first turn's messages — proving multi-turn context.
    #[tokio::test]
    async fn chat_multi_turn_context_persists() {
        use std::io::Cursor;
        use std::sync::{Arc, Mutex};

        let store = SessionStore::new();
        let session = store.create_session(None);
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approver = TestApprover;
        let tool_ctx = ToolContext {
            registry: &registry,
            root: &root,
            approve: &approver,
            max_iterations: 8,
        };

        let memory_store = Arc::new(Memory::with_default_root(MemoryConfig::default()));
        let skills = SkillRegistry::new();

        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider = RecordingProvider { seen: seen.clone() };

        // Feed two lines: two separate questions, then exit.
        let input = Cursor::new(b"first question\nsecond question\nexit\n");
        let code = super::chat_repl(
            &provider,
            "mock",
            &skills,
            &store,
            &memory_store,
            &tool_ctx,
            &session.id,
            false,
            input,
        )
        .await;

        assert_eq!(code, ExitCode::SUCCESS);

        let msgs = seen.lock().unwrap();
        // The last (second) turn's request must contain:
        // system, user("first question"), assistant("ok"), user("second question")
        assert!(
            msgs.len() >= 4,
            "second turn request should include both turns' messages, got {msgs:?}"
        );

        let contents: Vec<Option<&str>> = msgs.iter().map(|m| m.content.as_deref()).collect();
        assert!(
            contents.contains(&Some("first question")),
            "first turn's user message not in second turn's request: {contents:?}"
        );
        assert!(
            contents.contains(&Some("second question")),
            "second turn's user message not in request: {contents:?}"
        );

        // Also verify via the session store directly.
        let history = store.get_messages(&session.id);
        assert_eq!(
            history.len(),
            4,
            "history should have 4 messages: user1, asst1, user2, asst2"
        );
        assert_eq!(history[0].role, Role::User);
        assert_eq!(history[0].content, "first question");
        assert_eq!(history[2].role, Role::User);
        assert_eq!(history[2].content, "second question");
    }

    /// Feeding EOF immediately exits cleanly without calling the provider.
    #[tokio::test]
    async fn chat_exits_cleanly_on_eof() {
        use std::io::Cursor;
        use std::sync::Arc;

        let store = SessionStore::new();
        let session = store.create_session(None);
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approver = TestApprover;
        let tool_ctx = ToolContext {
            registry: &registry,
            root: &root,
            approve: &approver,
            max_iterations: 8,
        };

        let memory_store = Arc::new(Memory::with_default_root(MemoryConfig::default()));
        let skills = SkillRegistry::new();

        let code = super::chat_repl(
            &JsonTextProvider,
            "mock",
            &skills,
            &store,
            &memory_store,
            &tool_ctx,
            &session.id,
            false,
            Cursor::new(b""),
        )
        .await;

        assert_eq!(code, ExitCode::SUCCESS);
        // No messages were produced: the loop never entered a turn.
        assert!(store.get_messages(&session.id).is_empty());
    }

    /// The `exit` and `quit` commands break the loop cleanly.
    #[tokio::test]
    async fn chat_exits_cleanly_on_exit_command() {
        use std::io::Cursor;
        use std::sync::Arc;

        let store = SessionStore::new();
        let session = store.create_session(None);
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approver = TestApprover;
        let tool_ctx = ToolContext {
            registry: &registry,
            root: &root,
            approve: &approver,
            max_iterations: 8,
        };

        let memory_store = Arc::new(Memory::with_default_root(MemoryConfig::default()));
        let skills = SkillRegistry::new();

        for cmd in ["exit\n", "quit\n"] {
            let code = super::chat_repl(
                &JsonTextProvider,
                "mock",
                &skills,
                &store,
                &memory_store,
                &tool_ctx,
                &session.id,
                false,
                Cursor::new(cmd.as_bytes()),
            )
            .await;

            assert_eq!(
                code,
                ExitCode::SUCCESS,
                "command {cmd:?} should exit cleanly"
            );
        }
    }
}
