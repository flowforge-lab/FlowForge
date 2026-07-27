//! `flowforge` — the headless FlowForge CLI. Drives the same `ff_agent::run_turn`
//! loop the desktop app uses, rendering agent events to the terminal instead of a
//! webview. See `docs/rfcs/0004-cli.md`.
//!
//! Tier-1 platforms: macOS + Linux. Windows is best-effort via WSL (the `bash` tool
//! assumes a POSIX shell).

mod approver;
mod config;
mod host;
mod json_events;
mod memory;
mod registry;
mod secrets;

#[cfg(test)]
mod test_support;

use std::io::{BufRead, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ff_agent::{run_turn, AgentEvent, CancelToken, ToolContext, UserContext};
use ff_core::{Mode, PermissionMatrix, ReasoningVisibility, Role};

use crate::approver::{ApprovalMode, CliApprover};
use crate::config::ConfigCommand;
use crate::memory::MemoryCommand;

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

#[derive(Subcommand, Debug)]
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
    /// Inspect or modify provider credentials (#724). Reads/writes the same
    /// `provider-registry.json` the desktop's settings panel uses, and stores
    /// secrets in the OS keychain. See `flowforge config --help` for the
    /// sub-subcommands (`list`, `<provider> <secret> <value|--stdin|--clear>`).
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Inspect or write durable memory (RFC 0006) directly: search the FTS5
    /// index, read a file by path, or append a note. These share the exact
    /// store + index the agent's `memory_*` tools use (#1081), so a human at
    /// the terminal can recall or jot a memory note without spending an agent
    /// turn. See `flowforge memory --help` for the sub-subcommands.
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
}

#[derive(Subcommand, Debug)]
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
        Command::Config { command } => config::run(command),
        Command::Memory { command } => memory::run(command).await,
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

/// Synthetic keychain account for a search backend's API key (#1010), matching the
/// desktop host's `search:*` namespace so a key set in either surface is shared.
fn search_secret_conn_id(backend: ff_core::SearchBackend) -> &'static str {
    match backend {
        ff_core::SearchBackend::Tavily => "search:tavily",
        ff_core::SearchBackend::SearxNg => "search:searxng",
        ff_core::SearchBackend::Brave => "search:brave",
        ff_core::SearchBackend::OpenAiCompatible => "search:openai-compatible",
    }
}

/// CLI [`SearchKeyProvider`](ff_tools::SearchKeyProvider): resolves search backend
/// keys from the same OS keychain the desktop app uses (#1010).
struct KeychainSearchKeys;

impl ff_tools::SearchKeyProvider for KeychainSearchKeys {
    fn key_for(&self, backend: ff_core::SearchBackend) -> Option<String> {
        secrets::get(search_secret_conn_id(backend), ff_core::SecretKind::ApiKey)
    }
}

/// Shared durable-memory setup (RFC 0006). Builds the store + FTS5 index, does a
/// full reindex from disk, and registers the three memory tools. Best-effort: an
/// index failure leaves the ambient block working but skips the recall tools.
fn build_registry_with_memory() -> (
    ff_tools::ToolRegistry,
    std::sync::Arc<ff_memory::Memory>,
    Option<std::sync::Arc<dyn ff_memory::MemoryIndex>>,
) {
    let mut registry = ff_tools::ToolRegistry::with_defaults();
    registry.register(Box::new(ff_tools::WebSearchTool::with_keys(
        std::sync::Arc::new(std::sync::Mutex::new(load_search_config())),
        std::sync::Arc::new(KeychainSearchKeys),
    )));
    // #1012: PubMed biomedical search (keyless), same seam as web.
    registry.register(Box::new(ff_tools::SearchTool::new(std::sync::Arc::new(
        ff_tools::PubMedSource::new(),
    ))));
    let (memory_store, memory_index) = build_memory_store();
    if let Some(index) = &memory_index {
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
    (registry, memory_store, memory_index)
}

/// Build the durable-memory store + FTS5 index the way the agent tools do
/// (RFC 0006): `Memory::with_default_root`, open the on-disk FTS5 index, then
/// reindex from disk so recall sees the current files. Best-effort: an index
/// open failure returns `None` for the index (the store still works for `get`,
/// and the ambient block degrades to unfiltered). Shared by
/// [`build_registry_with_memory`] and the `ff memory` subcommands (#1081) so
/// there is exactly one store+index construction seam in the CLI.
fn build_memory_store() -> (
    std::sync::Arc<ff_memory::Memory>,
    Option<std::sync::Arc<dyn ff_memory::MemoryIndex>>,
) {
    let memory_store = std::sync::Arc::new(ff_memory::Memory::with_default_root(
        ff_memory::MemoryConfig::default(),
    ));
    let mut memory_index: Option<std::sync::Arc<dyn ff_memory::MemoryIndex>> = None;
    if let Ok(index) = ff_memory::Fts5Index::open(memory_store.index_path()) {
        let index: std::sync::Arc<dyn ff_memory::MemoryIndex> = std::sync::Arc::new(index);
        let _ = ff_memory::MemoryIndex::reindex(index.as_ref(), &memory_store.all_chunks());
        memory_index = Some(index);
    }
    (memory_store, memory_index)
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
    let store = std::sync::Arc::new(ff_session::SessionStore::new());
    let (mut registry, memory_store, memory_index) = build_registry_with_memory();
    registry.register(Box::new(ff_tools::CompactionRetrieveTool::new(
        store.clone(),
    )));
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

    let user_ctx = UserContext::now().with_working_dir(workspace.display().to_string());
    let (memory, ambient_keys) = match &memory_index {
        Some(idx) => memory_store.ambient_block_filtered_keyed(idx.as_ref()),
        None => (memory_store.ambient_block(), Vec::new()),
    };
    let system_prompt = ff_agent::build_system_prompt(
        inputs.persona.as_deref(),
        &skills,
        &inputs.active,
        &user_ctx,
        memory.as_deref(),
        None,
        None,
        mode,
    );

    let matrix = PermissionMatrix::default();
    let mut tool_ctx = ToolContext::new(
        &registry,
        &workspace,
        &approver,
        inputs.max_iterations,
        &matrix,
    );
    tool_ctx.mode = mode;
    // RFC 0013: apply the active phenotype's egress policy (Open when no --pheno).
    tool_ctx.egress = active_pheno.as_ref().map(|p| p.egress).unwrap_or_default();

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
            Some(&system_prompt),
            true,
            ReasoningVisibility::All,
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
            Some(&system_prompt),
            true,
            ReasoningVisibility::All,
            cancel,
            render_event_text,
        )
        .await
    };

    match result {
        Ok(_) => {
            // Weak ambient reinforcement (RFC 0007 §10.1): the turn produced a
            // reply, so refresh the curated chunks that were ambient-injected.
            // No-op unless `decay.ambient_gain > 0`.
            if let Some(idx) = &memory_index {
                let _ = idx.reinforce_ambient(&ambient_keys);
            }
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
    let store = std::sync::Arc::new(ff_session::SessionStore::new());
    let (mut registry, memory_store, memory_index) = build_registry_with_memory();
    registry.register(Box::new(ff_tools::CompactionRetrieveTool::new(
        store.clone(),
    )));
    let approver = CliApprover::new(approval_mode, mode);
    let session = store.create_session(None);

    let matrix = PermissionMatrix::default();
    let mut tool_ctx = ToolContext::new(
        &registry,
        &workspace,
        &approver,
        ff_agent::DEFAULT_MAX_ITERATIONS,
        &matrix,
    );
    tool_ctx.mode = mode;

    let stdin = std::io::stdin();
    chat_repl(
        provider.as_ref(),
        &model,
        &skills,
        &store,
        &memory_store,
        memory_index.as_ref(),
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
    memory_index: Option<&std::sync::Arc<dyn ff_memory::MemoryIndex>>,
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

        let user_ctx = UserContext::now().with_working_dir(tool_ctx.root.display().to_string());
        let (memory, ambient_keys) = match memory_index {
            Some(idx) => memory_store.ambient_block_filtered_keyed(idx.as_ref()),
            None => (memory_store.ambient_block(), Vec::new()),
        };
        let system_prompt = ff_agent::build_system_prompt(
            None,
            skills,
            &[],
            &user_ctx,
            memory.as_deref(),
            None,
            None,
            mode,
        );

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
                Some(&system_prompt),
                true,
                ReasoningVisibility::All,
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
                Some(&system_prompt),
                true,
                ReasoningVisibility::All,
                cancel,
                render_event_text,
            )
            .await
        };

        // Cancel the ctrl-c listener now that the turn is done so a stray signal
        // from a previous turn cannot fire during the next prompt.
        ctrlc_handle.abort();

        match result {
            Ok(_) => {
                // Weak ambient reinforcement (RFC 0007 §10.1) for the curated
                // chunks injected this turn. No-op unless `ambient_gain > 0`.
                if let Some(idx) = memory_index {
                    let _ = idx.reinforce_ambient(&ambient_keys);
                }
            }
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
        AgentEvent::AttachmentsDropped { count, .. } => {
            // Documents are universally supported as of the #338 follow-up
            // (Bedrock `DocumentBlock`; OpenAI/Ollama extraction fallback), so
            // the only kind that can be dropped in the host path is images —
            // name it specifically rather than the opaque "that attachment kind".
            eprintln!(
                "\n[attachments] {count} image{} not sent -- this model cannot see images.",
                if count == 1 { "" } else { "s" }
            );
        }
        AgentEvent::EgressMismatch { kind, model, .. } => {
            // LocalOnly-but-cloud-inference notice (#888). The user asked for a
            // local-privacy phenotype but the resolved inference path is hosted,
            // so prompt content still leaves this machine. Mirror the
            // `AttachmentsDropped` render style — a single `[privacy]` line —
            // and name both the kind and the model so the warning is
            // unambiguous and the user can act on it (switch connection, or
            // accept the egress with an explicit override if a future gate
            // mode ships).
            eprintln!(
                "\n[privacy] egress=local-only but inference uses {} ({model}). \
                 Prompt content still leaves this machine to reach the model -- \
                 switch to a local connection (Ollama / candle-vllm) for a true enclave.",
                kind.slug()
            );
        }
        AgentEvent::ToolOutputChunk { delta, .. } => {
            // Live command output (#680) streams to stderr so piping stdout still
            // yields only the model's text.
            eprint!("{delta}");
            let _ = std::io::stderr().flush();
        }
        AgentEvent::Done { .. } => {}
        AgentEvent::Reasoning { .. } => {}
        AgentEvent::Error { message } => {
            eprintln!("\n[error] {message}");
        }
        AgentEvent::Reconnecting {
            attempt,
            max_attempts,
            ..
        } => {
            eprintln!("\n[reconnecting] {attempt}/{max_attempts}");
        }
        AgentEvent::ConnectionFailed { message, .. } => {
            eprintln!("\n[connection lost] {message}");
        }
    }
}

#[cfg(test)]
mod tests;
