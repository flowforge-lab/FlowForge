//! `flowforge` — the headless FlowForge CLI. Drives the same `ff_agent::run_turn`
//! loop the desktop app uses, rendering agent events to the terminal instead of a
//! webview. See `docs/rfcs/0004-cli.md`.
//!
//! Tier-1 platforms: macOS + Linux. Windows is best-effort via WSL (the `bash` tool
//! assumes a POSIX shell).

mod approver;
mod config;
mod goal;
mod goal_loop;
mod host;
mod json_events;
mod mcp_host;
mod memory;
mod registry;
mod secrets;
mod serve;
mod sessions;
mod task;
mod task_runner;

#[cfg(test)]
mod test_support;

use std::io::{BufRead, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use ff_agent::{run_session_turn, AgentEvent, CancelToken, ToolContext, UserContext};
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
        /// Use an in-memory (ephemeral) session store: nothing is written to disk
        /// and the turn won't appear in `ff sessions list`. Default is persistent
        /// (the turn is saved and resumable). Escape hatch for one-shot `run`.
        #[arg(long)]
        ephemeral: bool,
    },
    /// Open an interactive REPL (multi-turn, one session). Default when
    /// no subcommand is given. Type `exit` or press Ctrl-D to quit.
    ///
    /// By default the session persists to disk (`ff sessions list` / `ff chat
    /// --resume`); pass `--ephemeral` to keep it in-memory only.
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
        /// Override the provider's default model for the session.
        #[arg(long, value_name = "ID")]
        model: Option<String>,
        /// Activate a skill's body for the session (repeatable). Unknown names error.
        #[arg(long, value_name = "NAME")]
        skill: Vec<String>,
        /// Load a phenotype's active skills, model, persona, egress policy, and
        /// search-corpus scope (see `~/.flowforge/phenos`). Applies to every turn
        /// of the session (#1208).
        #[arg(long, value_name = "NAME")]
        pheno: Option<String>,
        /// Approval mode: `auto` auto-approves writes, `act` prompts every write,
        /// `plan` also hides write/dangerous tools. Dangerous calls always prompt.
        #[arg(long, value_enum, default_value = "auto")]
        mode: ModeArg,
        /// Use an in-memory (ephemeral) session store: nothing is written to disk.
        #[arg(long)]
        ephemeral: bool,
        /// Reopen a persisted session by id and continue it. Use `ff sessions
        /// list` to find the id. Errors if the session does not exist.
        #[arg(long, value_name = "ID")]
        resume: Option<String>,
    },
    /// List persisted sessions (`ff sessions list`) — id, label, updated-at.
    /// The store is the same on-disk db the desktop app uses, so every session
    /// from either surface appears here (#1080).
    Sessions {
        #[command(subcommand)]
        command: SessionsCommand,
    },
    /// Fork a session at its tip into a new session, reusing the store's
    /// `fork_session`. The fork gets a `(Fork N)` title for parity with the
    /// desktop's sidebar Fork entry (#1069). Prints the new session id + title.
    Fork {
        /// The session id to fork (see `ff sessions list`).
        id: String,
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
    /// Run autonomous goal loop headless (#1082).
    Goal(goal::GoalArgs),
    /// Serve a Slack channel: routes messages into agent turns and asks for
    /// approval over Block Kit buttons (#1060).
    Serve(serve::ServeArgs),
    /// Manage scheduled tasks (#1082).
    Task {
        #[command(subcommand)]
        command: task::TaskCommand,
    },
}

#[derive(Subcommand, Debug)]
enum SkillsCommand {
    /// List installed skills and their descriptions.
    List,
}

/// `ff sessions <SUBCOMMAND>` — inspect the persisted session store (#1080).
#[derive(Subcommand, Debug)]
enum SessionsCommand {
    /// List persisted sessions: id, label, status, updated-at. TSV to stdout
    /// (machine-parseable, matching `ff config list`), one session per line,
    /// most-recently-updated first. Drafts (never-messaged) are not persisted
    /// and do not appear.
    List,
}

#[tokio::main]
async fn main() -> ExitCode {
    // Install the tracing subscriber before anything else runs, so startup
    // failures are recorded too. Held for the whole of `main`: the guard flushes
    // the non-blocking writer on drop, and binding it to `_` instead would drop
    // it here and silently discard every buffered line (#1060).
    let _log_guard = flowforge_log_dir().and_then(|dir| ff_logging::init(&dir));

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Chat {
        json: false,
        yes: false,
        deny: false,
        model: None,
        skill: Vec::new(),
        pheno: None,
        mode: ModeArg::Auto,
        ephemeral: false,
        resume: None,
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
            ephemeral,
        } => {
            run(
                prompt,
                json,
                approval_mode(yes, deny),
                mode.into(),
                model,
                skill,
                pheno,
                ephemeral,
            )
            .await
        }
        Command::Chat {
            json,
            yes,
            deny,
            model,
            skill,
            pheno,
            mode,
            ephemeral,
            resume,
        } => {
            chat(
                json,
                approval_mode(yes, deny),
                mode.into(),
                ephemeral,
                resume,
                TurnFlags {
                    model,
                    skill,
                    pheno,
                },
            )
            .await
        }
        Command::Skills { command } => match command {
            SkillsCommand::List => skills_list(),
        },
        Command::Sessions { command } => match command {
            SessionsCommand::List => sessions_list(),
        },
        Command::Fork { id } => fork_session_cmd(&id),
        Command::Config { command } => config::run(command),
        Command::Memory { command } => memory::run(command).await,
        Command::Goal(args) => goal::run(args).await,
        Command::Serve(args) => serve::run(args).await,
        Command::Task { command } => task::run(command).await,
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

/// `ff sessions list` — print persisted sessions as TSV (id, label, status,
/// updated-at), most-recently-updated first. Mirrors `ff config list`'s
/// machine-parseable convention. Drafts (never-messaged) are not persisted by
/// the store and so do not appear.
fn sessions_list() -> ExitCode {
    let store = host::build_session_store(false);
    let sessions = store.list_sessions();
    if let Err(e) = sessions::render_list(&sessions, &mut std::io::stdout()) {
        eprintln!("error writing session list: {e}");
        return ExitCode::FAILURE;
    }
    if sessions.is_empty() {
        eprintln!("No persisted sessions yet. Use `ff chat` to start one.");
    }
    ExitCode::SUCCESS
}

/// `ff fork <id>` — fork a session at its tip into a new session with a
/// `(Fork N)` title for parity with the desktop's sidebar Fork entry (#1069).
/// Prints the new session id + title to stdout (TSV) so the user can
/// immediately `ff chat --resume <new_id>`.
fn fork_session_cmd(id: &str) -> ExitCode {
    let store = host::build_session_store(false);
    let source = match store.get_session(id) {
        Some(s) => s,
        None => {
            eprintln!("error: no session with id {id}");
            return ExitCode::FAILURE;
        }
    };
    // `fork_session` clones the session + transcript and stamps a generic
    // "<title> (copy)"; we override with the desktop-parity "(Fork N)" naming
    // when the source has a title (#1069). An untitled source keeps the store's
    // copy (or None), matching the desktop's `if (session.title)` guard.
    let forked = match store.fork_session(id) {
        Some(f) => f,
        None => {
            eprintln!("error: could not fork session {id}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(source_title) = &source.title {
        let all = store.list_sessions();
        let existing: Vec<Option<&str>> = all.iter().map(|s| s.title.as_deref()).collect();
        let new_title = sessions::next_fork_title(source_title, &existing);
        store.set_title(&forked.id, new_title.clone());
        println!("{}\t{}", forked.id, new_title);
    } else {
        println!("{}\t{}", forked.id, sessions::resolve_label(&forked));
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

/// `<config dir>/flowforge` — where [`ff_logging::init`] writes the rolling log.
///
/// Deliberately the same directory the CLI already uses for `sessions.db` and
/// `transports.toml` (`host::config_dir`), so `flowforge` and the desktop app
/// write their logs to one place rather than two: when a Slack turn misbehaves,
/// the `serve` log sits next to the session DB that recorded the turn.
fn flowforge_log_dir() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("flowforge"))
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

/// Resolve `--pheno` to a phenotype and then to per-turn inputs, in one step.
///
/// `run` and `chat` both need the identical pair, and #1208 was caused precisely by
/// them drifting apart: `run` resolved a phenotype while `chat` hardcoded
/// `persona: None` / `active: &[]`, so every phenotype was silently inert in the
/// REPL. Keeping one resolution path makes that class of drift structurally
/// impossible rather than something a test has to notice.
///
/// Returns `Err(message)` for an unknown phenotype or skill so each caller can keep
/// its own exit-code handling; the message is already user-facing.
fn resolve_pheno_and_inputs(
    default_model: &str,
    skills: &ff_skills::SkillRegistry,
    model_flag: Option<&str>,
    skill_flags: &[String],
    pheno_flag: Option<&str>,
) -> Result<(Option<ff_core::Phenotype>, TurnInputs), String> {
    let active_pheno = match pheno_flag {
        Some(name) => match host::resolve_phenotype(name) {
            Some(p) => Some(p),
            None => return Err(format!("unknown phenotype: {name}")),
        },
        None => None,
    };
    let inputs = resolve_turn_inputs(
        default_model,
        skills,
        model_flag,
        skill_flags,
        active_pheno.as_ref(),
    )?;
    Ok((active_pheno, inputs))
}

/// Apply a phenotype's security scopes to a `ToolContext`.
///
/// These two assignments are what make a `LocalOnly` phenotype such as `enclave`
/// actually local: `ToolContext::egress` gates both the advertised tool set and
/// dispatch, and a bridged MCP tool defaults to `reaches_network = true`
/// (`ff-mcp` supervisor.rs), so a command that stands up an MCP host without
/// setting egress hands `enclave` a network path. That was the #1208 regression.
///
/// Kept as one named function so the pair cannot be applied by one caller and
/// forgotten by another — the drift that #1208 was.
fn apply_phenotype_scopes(
    tool_ctx: &mut ToolContext<'_>,
    active_pheno: Option<&ff_core::Phenotype>,
) {
    // RFC 0013: egress policy (Open when no --pheno).
    tool_ctx.egress = active_pheno.map(|p| p.egress).unwrap_or_default();
    // #552 / #1011 2b: search-corpus scope (baseline when no --pheno).
    tool_ctx.search_sources = active_pheno.and_then(|p| p.search_sources.clone());
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

/// The non-MCP tool-registry seam. **Test-only since #1208**: every production command
/// (`run`, `goal`, `task`, `serve`, and now `chat`) uses [`build_registry_with_mcp`].
///
/// Kept rather than deleted because three #1207 regression guards are built on it —
/// they assert the MCP seam registers exactly the same local toolset as this one, which
/// is what catches a tool being added to one path and not the other. Delete this and
/// the drift it was written to catch becomes invisible again.
///
/// Includes the durable-memory setup (RFC 0006): builds the store + FTS5 index, does a
/// full reindex from disk, and registers the three memory tools. Best-effort — an index
/// failure leaves the ambient block working but skips the recall tools.
///
/// There used to be a second copy in `goal_loop.rs`, and the two drifted: goal mode
/// silently lacked PubMed search and all three memory tools, while `run` lacked
/// `goal_complete`. Nothing caught it, because a missing tool is not a type error —
/// it just makes the agent quietly less capable on one path. Both seams therefore share
/// [`build_base_registry`] as their only construction site — this one only to keep the
/// comparison honest, since it no longer serves a production command;
/// `both_registry_seams_register_the_same_base_toolset` pins it (#1207).
#[cfg(test)]
pub(crate) async fn build_registry_with_memory() -> (
    ff_tools::ToolRegistry,
    std::sync::Arc<ff_memory::Memory>,
    Option<std::sync::Arc<dyn ff_memory::MemoryIndex>>,
) {
    // No MCP host is stood up on this path at all. Delegating to
    // `build_registry_with_mcp` and dropping its guard here would be worse than
    // useless: the servers would be killed the moment this function returned, leaving
    // the caller advertising MCP tools whose transports are already dead.
    build_base_registry()
}

/// As [`build_registry_with_memory`], but also returns the MCP server guidance that
/// belongs with the bridged tools (#1173). Callers that build a system prompt want this
/// one: handing a model an MCP tool while withholding its server's usage instructions is
/// the exact failure that guidance injection exists to prevent.
pub(crate) async fn build_registry_with_mcp() -> (
    ff_tools::ToolRegistry,
    std::sync::Arc<ff_memory::Memory>,
    Option<std::sync::Arc<dyn ff_memory::MemoryIndex>>,
    Vec<ff_agent::McpGuidance>,
    Option<mcp_host::McpTeardown>,
) {
    let (mut registry, memory_store, memory_index) = build_base_registry();
    // MCP servers from `~/.flowforge/mcp.json` — the same file the desktop watches
    // (#1207). Fail-soft: no config, or an unreachable server, leaves the rest of the
    // toolset untouched. Deferred servers are skipped with a warning; see `mcp_host`.
    // The guard is returned to the caller rather than dropped here: dropping it at the end
    // of this function would stop every server before the first tool call.
    let (mcp_guidance, mcp_teardown) = match mcp_host::init() {
        Some((handle, awaited)) => {
            mcp_host::bridge_into(&handle, &mut registry, &host::workspace_root(), awaited).await;
            let guidance = mcp_host::guidance(&handle);
            (guidance, Some(mcp_host::McpTeardown::new(handle)))
        }
        None => (Vec::new(), None),
    };
    (
        registry,
        memory_store,
        memory_index,
        mcp_guidance,
        mcp_teardown,
    )
}

/// Every non-MCP tool the CLI registers, plus the durable-memory store and index.
///
/// The single construction site both registry seams delegate to, so the MCP and
/// non-MCP paths cannot drift apart in what they register — the drift that made
/// goal mode quietly lose PubMed and the memory tools in the first place.
fn build_base_registry() -> (
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
    // Goal-mode completion signal (RFC 0020 S7, #716): a ReadOnly tool the agent calls
    // when the objective is met. Registered on every path — harmless outside goal mode,
    // and its absence is what made the two registry copies diverge.
    registry.register(Box::new(ff_tools::GoalCompleteTool));
    // Bookkeeping counterpart: lets the agent record evidence-first steps into the
    // goal's durable ledger (#1225). Registered alongside `goal_complete` for the
    // same reason — divergent registries are how these go missing.
    registry.register(Box::new(ff_tools::GoalStepTool));
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
pub(crate) fn build_memory_store() -> (
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

#[allow(clippy::too_many_arguments)]
async fn run(
    prompt: String,
    json: bool,
    approval_mode: ApprovalMode,
    mode: Mode,
    model: Option<String>,
    skill: Vec<String>,
    pheno: Option<String>,
    ephemeral: bool,
) -> ExitCode {
    let (provider, default_model) = host::load_provider();
    let skills = host::load_skills();
    let workspace = host::workspace_root();
    let store = std::sync::Arc::new(host::build_session_store(ephemeral));
    // `_mcp_teardown` must stay bound for the whole function: dropping it stops every MCP
    // server, so an `_`-discard here would kill them before the first tool call.
    let (mut registry, memory_store, memory_index, mcp_guidance, _mcp_teardown) =
        build_registry_with_mcp().await;
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
    let (active_pheno, inputs) = match resolve_pheno_and_inputs(
        &default_model,
        &skills,
        model.as_deref(),
        &skill,
        pheno.as_deref(),
    ) {
        Ok(pair) => pair,
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
    let system_prompt_inputs = ff_agent::SystemPromptInputs {
        persona: inputs.persona.as_deref(),
        skills: &skills,
        active: &inputs.active,
        user: &user_ctx,
        memory: memory.as_deref(),
        extra_instructions: None,
        goal: None,
        mode,
        mcp_guidance: &mcp_guidance,
    };

    let matrix = PermissionMatrix::default();
    let mut tool_ctx = ToolContext::new(
        &registry,
        &workspace,
        &approver,
        inputs.max_iterations,
        &matrix,
    );
    tool_ctx.mode = mode;
    apply_phenotype_scopes(&mut tool_ctx, active_pheno.as_ref());

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
        run_session_turn(
            provider.as_ref(),
            &store,
            &tool_ctx,
            &session.id,
            &inputs.model,
            &system_prompt_inputs,
            true,
            ReasoningVisibility::All,
            cancel,
            |event| {
                json_events::emit_line(&event);
            },
        )
        .await
    } else {
        run_session_turn(
            provider.as_ref(),
            &store,
            &tool_ctx,
            &session.id,
            &inputs.model,
            &system_prompt_inputs,
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

/// Interactive REPL (multi-turn, one session). Keeps a single
/// [`ff_session::SessionStore`] alive for the life of the process so each turn
/// sees the full accumulated history. Loops until EOF, `exit`, or `quit`.
///
/// When `ephemeral` is false (default), the store is on-disk and the session
/// persists for `ff sessions list` / `ff chat --resume`. When `resume` is
/// `Some(id)`, reopens that session instead of creating a new one — errors if
/// the id is unknown.
///
/// `--model`/`--skill`/`--pheno` resolve exactly as they do for `run`, via the shared
/// [`resolve_turn_inputs`]. The phenotype is resolved once and applies to every turn of
/// the session: unlike `run`, there is no per-turn re-resolution, because a REPL has no
/// point at which the user could pass new flags (#1208).
/// The `--model`/`--skill`/`--pheno` trio, grouped so adding a fourth turn-shaping flag
/// does not push another parameter through [`chat`]'s signature. `run` takes them
/// positionally because its parameter list predates this; new surfaces should use this.
struct TurnFlags {
    model: Option<String>,
    skill: Vec<String>,
    pheno: Option<String>,
}

async fn chat(
    json: bool,
    approval_mode: ApprovalMode,
    mode: Mode,
    ephemeral: bool,
    resume: Option<String>,
    flags: TurnFlags,
) -> ExitCode {
    let (provider, default_model) = host::load_provider();
    let skills = host::load_skills();
    let workspace = host::workspace_root();
    let store = std::sync::Arc::new(host::build_session_store(ephemeral));
    // MCP is safe here only because the phenotype's `egress` is applied below: a bridged
    // tool defaults to `reaches_network = true` (`ff-mcp` supervisor.rs), so a `LocalOnly`
    // phenotype must be able to strip it. Egress wiring and this call site landed together
    // in #1208 for exactly that reason — do not separate them.
    //
    // `_mcp_teardown` must stay bound for the whole function: dropping it stops every MCP
    // server, so an `_`-discard here would kill them before the first turn.
    let (mut registry, memory_store, memory_index, mcp_guidance, _mcp_teardown) =
        build_registry_with_mcp().await;
    registry.register(Box::new(ff_tools::CompactionRetrieveTool::new(
        store.clone(),
    )));
    // Resolved once for the whole session (see the note above), otherwise identical to
    // `run`'s handling — same helper, same precedence, same failure messages.
    let (active_pheno, turn) = match resolve_pheno_and_inputs(
        &default_model,
        &skills,
        flags.model.as_deref(),
        &flags.skill,
        flags.pheno.as_deref(),
    ) {
        Ok(pair) => pair,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::FAILURE;
        }
    };
    let approver = CliApprover::new(approval_mode, mode);
    // Resume an existing session or create a new one. A bogus resume id is a
    // hard error (don't silently start a fresh session — the user asked for a
    // specific one). The store's `get_session` surfaces pending drafts too,
    // but those never round-trip across restarts (drafts are in-memory only),
    // so a resumed id from `sessions list` is always a persisted row.
    let session_id = match resume {
        Some(id) => match store.get_session(&id) {
            Some(s) => {
                if !json {
                    eprintln!("Resumed: {}", sessions::resolve_label(&s));
                }
                s.id
            }
            None => {
                eprintln!("error: no session with id {id}");
                return ExitCode::FAILURE;
            }
        },
        None => store.create_session(None).id,
    };

    let matrix = PermissionMatrix::default();
    let mut tool_ctx = ToolContext::new(
        &registry,
        &workspace,
        &approver,
        turn.max_iterations,
        &matrix,
    );
    tool_ctx.mode = mode;
    // This is what makes the MCP seam above safe for a `LocalOnly` phenotype.
    apply_phenotype_scopes(&mut tool_ctx, active_pheno.as_ref());

    let stdin = std::io::stdin();
    chat_repl(
        provider.as_ref(),
        &turn,
        &skills,
        &store,
        &memory_store,
        memory_index.as_ref(),
        &tool_ctx,
        &session_id,
        json,
        mode,
        &mcp_guidance,
        stdin.lock(),
    )
    .await
}

/// Core REPL loop with injectable input for testability. Reads lines from `input`,
/// dispatches each to [`run_turn`], and loops until EOF, `exit`, or `quit`.
///
/// Takes the resolved [`TurnInputs`] rather than a bare model id so the phenotype's
/// persona and active-skill set reach every turn's system prompt. Before #1208 this
/// function hardcoded `persona: None` / `active: &[]`, which made `--pheno` inert in
/// `chat` in a way no type error could catch.
#[allow(clippy::too_many_arguments)]
async fn chat_repl(
    provider: &dyn ff_llm::Provider,
    turn: &TurnInputs,
    skills: &ff_skills::SkillRegistry,
    store: &ff_session::SessionStore,
    memory_store: &std::sync::Arc<ff_memory::Memory>,
    memory_index: Option<&std::sync::Arc<dyn ff_memory::MemoryIndex>>,
    tool_ctx: &ToolContext<'_>,
    session_id: &str,
    json: bool,
    mode: Mode,
    mcp_guidance: &[ff_agent::McpGuidance],
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
        let system_prompt_inputs = ff_agent::SystemPromptInputs {
            persona: turn.persona.as_deref(),
            skills,
            active: &turn.active,
            user: &user_ctx,
            memory: memory.as_deref(),
            extra_instructions: None,
            goal: None,
            mode,
            mcp_guidance,
        };

        let cancel = CancelToken::new();
        let cancel_signal = cancel.clone();
        let ctrlc_handle = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancel_signal.cancel();
            }
        });

        let result = if json {
            run_session_turn(
                provider,
                store,
                tool_ctx,
                session_id,
                &turn.model,
                &system_prompt_inputs,
                true,
                ReasoningVisibility::All,
                cancel,
                |event| json_events::emit_line(&event),
            )
            .await
        } else {
            run_session_turn(
                provider,
                store,
                tool_ctx,
                session_id,
                &turn.model,
                &system_prompt_inputs,
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
        AgentEvent::AttachmentsDropped { count, reason, .. } => {
            if let Some(reason) = reason {
                eprintln!(
                    "\n[attachments] {count} attachment{} omitted: {reason}.",
                    if count == 1 { "" } else { "s" }
                );
            } else {
                // Documents are universally supported as of the #338 follow-up
                // (Bedrock `DocumentBlock`; OpenAI/Ollama extraction fallback), so
                // the only kind that can be dropped in the host path is images —
                // name it specifically rather than the opaque "that attachment kind".
                eprintln!(
                    "\n[attachments] {count} image{} not sent -- this model cannot see images.",
                    if count == 1 { "" } else { "s" }
                );
            }
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
