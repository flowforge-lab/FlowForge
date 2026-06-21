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
use ff_core::{Mode, Role};

use crate::approver::{ApprovalMode, CliApprover};

/// FlowForge on the command line: run an agent turn, inspect skills, no GUI.
/// With no subcommand, opens an interactive REPL (multi-turn chat).
///
/// Exit codes (`run`): 0 = success, non-zero on an agent error or a
/// required-but-denied tool approval. The REPL exits 0 on clean shutdown
/// (EOF / `exit`); per-turn failures are printed inline.
#[derive(Parser)]
#[command(name = "flowforge", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

/// CLI surface for the agent's Plan/Act/Auto mode. Maps to [`ff_core::Mode`].
/// `auto` (the default) auto-approves writes; `act` prompts for every write and
/// dangerous call; `plan` additionally hides write/dangerous tools from the model.
/// Dangerous calls always still prompt regardless of mode.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum ModeArg {
    Plan,
    Act,
    Auto,
}

impl From<ModeArg> for Mode {
    fn from(arg: ModeArg) -> Self {
        match arg {
            ModeArg::Plan => Mode::Plan,
            ModeArg::Act => Mode::Act,
            ModeArg::Auto => Mode::Auto,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Run a single agent turn against a prompt and stream the result.
    ///
    /// Use `--json` for machine-readable output (one JSON event per line).
    /// The process exits non-zero if an agent error occurs or a required
    /// tool approval is denied (`--deny`, piped-no-policy, or `N` at a prompt).
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
        /// Override the provider's default model for this turn.
        #[arg(long, value_name = "ID")]
        model: Option<String>,
        /// Activate a skill's body for the turn (repeatable). Unknown names error.
        #[arg(long, value_name = "NAME")]
        skill: Vec<String>,
        /// Load a phenotype's active skills, model, and persona (see `~/.flowforge/phenos`).
        #[arg(long, value_name = "NAME")]
        pheno: Option<String>,
        /// Approval mode: `auto` auto-approves writes, `act` prompts every write,
        /// `plan` also hides write/dangerous tools. Dangerous calls always prompt.
        #[arg(long, value_enum, default_value = "auto")]
        mode: ModeArg,
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
        /// Approval mode: `auto` auto-approves writes, `act` prompts every write,
        /// `plan` also hides write/dangerous tools. Dangerous calls always prompt.
        #[arg(long, value_enum, default_value = "auto")]
        mode: ModeArg,
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
        mode: ModeArg::Auto,
    }) {
        Command::Run {
            prompt,
            json,
            yes,
            deny,
            model,
            skill,
            pheno,
            mode,
        } => {
            run(
                prompt,
                json,
                approval_mode(yes, deny),
                mode.into(),
                model,
                skill,
                pheno,
            )
            .await
        }
        Command::Chat {
            json,
            yes,
            deny,
            mode,
        } => chat(json, approval_mode(yes, deny), mode.into()).await,
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

/// Resolved per-turn inputs derived from the `run` flags. Mirrors what the
/// desktop's `AppState` computes each turn: an active-skill set (registry
/// validated), a model (flag → phenotype → provider default), and an optional
/// persona. Built once before the turn so `run` stays a flat call.
#[derive(Debug)]
struct TurnInputs {
    model: String,
    persona: Option<String>,
    active: Vec<String>,
    max_iterations: usize,
}

/// Resolve the `--model`/`--skill`/`--pheno` flags to per-turn inputs, mirroring
/// the desktop's `send_message` assembly. `pheno` is already resolved (or `None`)
/// by the caller — the desktop resolves its active phenotype from the persisted
/// pointer; the CLI resolves it from `--pheno`.
///
/// Precedence mirrors the desktop: a phenotype seeds the active skills, persona,
/// and a model candidate; `--model` is the most specific override (wins over the
/// phenotype's model, which wins over the provider default). `--skill` names must
/// resolve in the registry (unknown → `Err`). A phenotype's own skills are
/// validated the same way the desktop validates them: unknown names are dropped
/// with a warning, not an error (the installed set can drift from a definition).
fn resolve_turn_inputs(
    default_model: &str,
    skills: &ff_skills::SkillRegistry,
    model_flag: Option<&str>,
    skill_flags: &[String],
    pheno: Option<&ff_core::Phenotype>,
) -> Result<TurnInputs, String> {
    use std::collections::BTreeSet;

    let (mut active, persona, pheno_model, pheno_max_iter) = match pheno {
        Some(p) => {
            let mut validated = BTreeSet::new();
            for name in &p.skills {
                if skills.get(name).is_some() {
                    validated.insert(name.clone());
                } else {
                    eprintln!(
                        "warning: phenotype \"{}\" names unknown skill \"{}\"; skipping",
                        p.name, name
                    );
                }
            }
            (
                validated,
                p.persona.clone(),
                p.model.clone(),
                p.max_iterations,
            )
        }
        None => (BTreeSet::new(), None, None, None),
    };

    for name in skill_flags {
        if skills.get(name).is_some() {
            active.insert(name.clone());
        } else {
            return Err(format!("unknown skill: {name}"));
        }
    }

    let model = model_flag
        .map(str::to_string)
        .or(pheno_model)
        .unwrap_or_else(|| default_model.to_string());

    Ok(TurnInputs {
        model,
        persona,
        active: active.into_iter().collect(),
        max_iterations: pheno_max_iter.unwrap_or(ff_agent::DEFAULT_MAX_ITERATIONS),
    })
}

/// Loads persisted web-search settings from `~/.config/flowforge/search.json` (the
/// same file the desktop Settings pane writes). Falls back to the default
/// (SearXNG, no endpoint) when the file is missing or unparseable; with no
/// endpoint configured the tool returns a clear "not configured" error rather
/// than failing the registry build.
fn load_search_config() -> ff_core::SearchConfig {
    dirs::config_dir()
        .map(|d| d.join("flowforge").join("search.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Shared durable-memory setup (RFC 0006). Builds the store + FTS5 index, does a
/// full reindex from disk, and registers the three memory tools. Best-effort: an
/// index failure leaves the ambient block working but skips the recall tools.
fn build_registry_with_memory() -> (ff_tools::ToolRegistry, std::sync::Arc<ff_memory::Memory>) {
    let mut registry = ff_tools::ToolRegistry::with_defaults();
    registry.register(Box::new(ff_tools::WebSearchTool::new(std::sync::Arc::new(
        std::sync::Mutex::new(load_search_config()),
    ))));
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

async fn run(
    prompt: String,
    json: bool,
    approval_mode: ApprovalMode,
    mode: Mode,
    model: Option<String>,
    skill: Vec<String>,
    pheno: Option<String>,
) -> ExitCode {
    let (provider, default_model) = host::load_provider();
    let skills = host::load_skills();
    let workspace = host::workspace_root();
    let store = ff_session::SessionStore::new();
    let (registry, memory_store) = build_registry_with_memory();
    let approver = CliApprover::new(approval_mode, mode);

    let session = store.create_session(None);
    store.add_message(&session.id, Role::User, prompt);

    // Resolve the --model/--skill/--pheno flags the same way the desktop resolves
    // its active phenotype each turn: a phenotype seeds the active skills, persona,
    // and a model candidate; `--model` is the most specific override; `--skill`
    // adds validated skills on top. Unknown --pheno/--skill names fail cleanly.
    let active_pheno = match pheno.as_deref() {
        Some(name) => match host::resolve_phenotype(name) {
            Some(p) => Some(p),
            None => {
                eprintln!("error: unknown phenotype: {name}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    let inputs = match resolve_turn_inputs(
        &default_model,
        &skills,
        model.as_deref(),
        &skill,
        active_pheno.as_ref(),
    ) {
        Ok(inputs) => inputs,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::FAILURE;
        }
    };

    let user_ctx = UserContext::now();
    let memory = memory_store.ambient_block();
    let system_prompt = ff_agent::build_system_prompt(
        inputs.persona.as_deref(),
        &skills,
        &inputs.active,
        &user_ctx,
        memory.as_deref(),
        mode,
    );

    let mut tool_ctx = ToolContext::new(&registry, &workspace, &approver, inputs.max_iterations);
    tool_ctx.mode = mode;

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
            &inputs.model,
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
            &inputs.model,
            Some(system_prompt.as_str()),
            true,
            cancel,
            render_event_text,
        )
        .await
    };

    match result {
        Ok(_) => {
            if approver.was_denied() {
                eprintln!("error: one or more required tool approvals were denied");
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Interactive REPL (multi-turn, one in-process session). Keeps a single
/// [`ff_session::SessionStore`] alive for the life of the process so each turn
/// sees the full accumulated history. Loops until EOF, `exit`, or `quit`.
async fn chat(json: bool, approval_mode: ApprovalMode, mode: Mode) -> ExitCode {
    let (provider, model) = host::load_provider();
    let skills = host::load_skills();
    let workspace = host::workspace_root();
    let store = ff_session::SessionStore::new();
    let (registry, memory_store) = build_registry_with_memory();
    let approver = CliApprover::new(approval_mode, mode);
    let session = store.create_session(None);

    let mut tool_ctx = ToolContext::new(
        &registry,
        &workspace,
        &approver,
        ff_agent::DEFAULT_MAX_ITERATIONS,
    );
    tool_ctx.mode = mode;

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
        mode,
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
    mode: Mode,
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
            ff_agent::build_system_prompt(None, skills, &[], &user_ctx, memory.as_deref(), mode);

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
        AgentEvent::MemoryFlushed { writes, .. } => {
            eprintln!(
                "\n[memory] auto-curated {writes} durable fact{}",
                if writes == 1 { "" } else { "s" }
            );
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
    use super::{approval_mode, build_registry_with_memory, resolve_turn_inputs, Cli};
    use crate::approver::ApprovalMode;
    use async_trait::async_trait;
    use clap::CommandFactory;
    use clap::Parser;
    use ff_agent::{run_turn, AgentEvent, Approver, CancelToken, ToolContext};
    use ff_core::{Phenotype, Role};
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
            AgentEvent::MemoryFlushed {
                message_id: "m1".into(),
                writes: 3,
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
        let tool_ctx = ToolContext::new(&registry, &root, &approver, 8);

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
        let tool_ctx = ToolContext::new(&registry, &root, &approver, 8);

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
            ff_core::Mode::Auto,
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
        let tool_ctx = ToolContext::new(&registry, &root, &approver, 8);

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
            ff_core::Mode::Auto,
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
        let tool_ctx = ToolContext::new(&registry, &root, &approver, 8);

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
                ff_core::Mode::Auto,
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

    // -- resolve_turn_inputs tests -------------------------------------------

    /// Build a Phenotype for tests. Mirrors `ff_skills::default_phenotype` in
    /// shape but lets each test set fields independently.
    fn test_phenotype(
        name: &str,
        skills: &[&str],
        model: Option<&str>,
        persona: Option<&str>,
    ) -> Phenotype {
        Phenotype {
            name: name.to_string(),
            skills: skills.iter().map(|s| s.to_string()).collect(),
            model: model.map(str::to_string),
            persona: persona.map(str::to_string),
            max_iterations: None,
        }
    }

    /// Build a SkillRegistry populated with named skills by writing minimal
    /// SKILL.md files to a temp dir and loading it — the same path `host::load_skills`
    /// uses in production. Each skill's frontmatter has just name/version/description.
    fn registry_with_skills(names: &[&str]) -> SkillRegistry {
        let tmp = tempfile::tempdir().unwrap();
        for name in names {
            let dir = tmp.path().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: d\nversion: 0.1.0\n---\nbody\n"),
            )
            .unwrap();
        }
        let (reg, _errors) = SkillRegistry::load_dir(tmp.path());
        reg
    }

    #[test]
    fn model_flag_overrides_default() {
        let reg = SkillRegistry::new();
        let inputs =
            resolve_turn_inputs("default-model", &reg, Some("flag-model"), &[], None).unwrap();
        assert_eq!(inputs.model, "flag-model");
    }

    #[test]
    fn pheno_model_used_when_no_flag() {
        let reg = SkillRegistry::new();
        let p = test_phenotype("rust", &[], Some("pheno-model"), None);
        let inputs = resolve_turn_inputs("default-model", &reg, None, &[], Some(&p)).unwrap();
        assert_eq!(inputs.model, "pheno-model");
    }

    #[test]
    fn model_flag_wins_over_pheno_model() {
        let reg = SkillRegistry::new();
        let p = test_phenotype("rust", &[], Some("pheno-model"), None);
        let inputs =
            resolve_turn_inputs("default-model", &reg, Some("flag-model"), &[], Some(&p)).unwrap();
        assert_eq!(inputs.model, "flag-model");
    }

    #[test]
    fn default_model_when_nothing_set() {
        let reg = SkillRegistry::new();
        let inputs = resolve_turn_inputs("default-model", &reg, None, &[], None).unwrap();
        assert_eq!(inputs.model, "default-model");
    }

    #[test]
    fn skill_flag_adds_active_skill() {
        let reg = registry_with_skills(&["alpha"]);
        let inputs = resolve_turn_inputs("d", &reg, None, &["alpha".to_string()], None).unwrap();
        assert_eq!(inputs.active, vec!["alpha"]);
    }

    #[test]
    fn unknown_skill_flag_errors() {
        let reg = SkillRegistry::new();
        let err = resolve_turn_inputs("d", &reg, None, &["bogus".to_string()], None).unwrap_err();
        assert!(err.contains("unknown skill"), "{err}");
        assert!(err.contains("bogus"), "{err}");
    }

    #[test]
    fn pheno_unknown_skill_dropped_not_errored() {
        // A phenotype can name skills that aren't installed; the desktop drops
        // them with a warning. The turn must still proceed (Ok), just without
        // that skill.
        let reg = SkillRegistry::new();
        let p = test_phenotype("rust", &["missing"], None, None);
        let inputs = resolve_turn_inputs("d", &reg, None, &[], Some(&p)).unwrap();
        assert!(inputs.active.is_empty());
    }

    #[test]
    fn pheno_known_skill_kept() {
        let reg = registry_with_skills(&["clippy"]);
        let p = test_phenotype("rust", &["clippy"], None, None);
        let inputs = resolve_turn_inputs("d", &reg, None, &[], Some(&p)).unwrap();
        assert_eq!(inputs.active, vec!["clippy"]);
    }

    #[test]
    fn pheno_persona_flows_through() {
        let reg = SkillRegistry::new();
        let p = test_phenotype("rust", &[], None, Some("You are a Rust expert."));
        let inputs = resolve_turn_inputs("d", &reg, None, &[], Some(&p)).unwrap();
        assert_eq!(inputs.persona.as_deref(), Some("You are a Rust expert."));
    }

    #[test]
    fn pheno_rust_full_acceptance() {
        // --pheno rust with skills + model + persona, plus an extra --skill and
        // --model override. Verifies the full acceptance scenario: model flag
        // wins, persona flows, pheno skills + flag skills both active.
        let reg = registry_with_skills(&["cargo-check", "clippy", "extra"]);
        let p = test_phenotype(
            "rust",
            &["cargo-check", "clippy"],
            Some("qwen3-coder"),
            Some("Rust expert"),
        );
        let inputs = resolve_turn_inputs(
            "default-model",
            &reg,
            Some("override-model"),
            &["extra".to_string()],
            Some(&p),
        )
        .unwrap();

        // --model wins over pheno model.
        assert_eq!(inputs.model, "override-model");
        // Persona from pheno.
        assert_eq!(inputs.persona.as_deref(), Some("Rust expert"));
        // Active = pheno's known skills + the --skill flag, sorted (BTreeSet).
        let mut active = inputs.active.clone();
        active.sort();
        assert_eq!(active, vec!["cargo-check", "clippy", "extra"]);
    }

    #[test]
    fn cli_registry_includes_web_search() {
        let (registry, _memory) = build_registry_with_memory();
        assert!(
            registry.get("web_search").is_some(),
            "web_search must be registered in the CLI tool registry (#241)"
        );
    }
}
