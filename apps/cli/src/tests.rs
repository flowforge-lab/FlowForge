use std::process::ExitCode;

use super::json_events;
use super::{approval_mode, build_registry_with_memory, resolve_turn_inputs, Cli};
use crate::approver::ApprovalMode;
use crate::test_support::TestEnv;
use async_trait::async_trait;
use clap::CommandFactory;
use clap::Parser;
use ff_agent::{run_turn, AgentEvent, ApprovalOutcome, Approver, CancelToken, ToolContext};
use ff_core::{PermissionMatrix, Phenotype, ReasoningVisibility, Role};
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

/// The `config` subcommand (issue #724) parses through the real clap tree
/// — guards against accidental rename or move of the `Config` variant.
#[test]
fn config_subcommand_parses() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["flowforge", "config", "list"]).expect("config list");
    match cli.command.expect("config present") {
        super::Command::Config { .. } => {}
        other => panic!("expected Config, got {other:?}"),
    }
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
            observer_intent: None,
        },
        AgentEvent::Done {
            message_id: "m1".into(),
            final_message: Some("Hello world!".into()),
            stop_reason: None,
            turns: Some(2),
            token_count: None,
            prefill_estimates: None,
            prompt_latency_ms: None,
            tier2_ms: None,
            tier1_fires: None,
            tier2_fires: None,
            retrieve_calls: None,
            cache_hit_tokens: None,
            cache_miss_tokens: None,
            breakdown: None,
            usage: None,
            budget_tokens: None,
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
    ) -> ApprovalOutcome {
        ApprovalOutcome::Allowed
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
    let matrix = PermissionMatrix::default();
    let tool_ctx = ToolContext::new(&registry, &root, &approver, 8, &matrix);

    let mut stdout = Vec::new();
    let msg = run_turn(
        &JsonTextProvider,
        &store,
        &tool_ctx,
        &session.id,
        "mock",
        None,
        false,
        ReasoningVisibility::WrapUp,
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
    let matrix = PermissionMatrix::default();
    let tool_ctx = ToolContext::new(&registry, &root, &approver, 8, &matrix);

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
        None,
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
    let matrix = PermissionMatrix::default();
    let tool_ctx = ToolContext::new(&registry, &root, &approver, 8, &matrix);

    let memory_store = Arc::new(Memory::with_default_root(MemoryConfig::default()));
    let skills = SkillRegistry::new();

    let code = super::chat_repl(
        &JsonTextProvider,
        "mock",
        &skills,
        &store,
        &memory_store,
        None,
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
    let matrix = PermissionMatrix::default();
    let tool_ctx = ToolContext::new(&registry, &root, &approver, 8, &matrix);

    let memory_store = Arc::new(Memory::with_default_root(MemoryConfig::default()));
    let skills = SkillRegistry::new();

    for cmd in ["exit\n", "quit\n"] {
        let code = super::chat_repl(
            &JsonTextProvider,
            "mock",
            &skills,
            &store,
            &memory_store,
            None,
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
        provider: None,
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::Open,
        preheat: Vec::new(),
        search_sources: None,
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
    let inputs = resolve_turn_inputs("default-model", &reg, Some("flag-model"), &[], None).unwrap();
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
    let (registry, _memory, _index) = build_registry_with_memory();
    assert!(
        registry.get("web_search").is_some(),
        "web_search must be registered in the CLI tool registry (#241)"
    );
}

// -- memory subcommand tests (issue #1081) --------------------------------
/// `memory` parses through the real clap tree — guards against accidental
/// rename or move of the `Memory` variant, mirroring `config_subcommand_parses`.
#[test]
fn memory_subcommand_parses() {
    fn expect_memory(argv: &[&str]) {
        let cli = Cli::try_parse_from(argv.to_vec())
            .unwrap_or_else(|e| panic!("parse failed for {argv:?}: {e}"));
        match cli.command.expect("memory present") {
            super::Command::Memory { .. } => {}
            other => panic!("expected Memory, got {other:?}"),
        }
    }
    expect_memory(&["flowforge", "memory", "search", "rust preferences"]);
    expect_memory(&["flowforge", "memory", "search", "x", "--limit", "3"]);
    expect_memory(&["flowforge", "memory", "get", "MEMORY.md"]);
    expect_memory(&["flowforge", "memory", "get", "MEMORY.md", "--lines", "1:20"]);
    expect_memory(&["flowforge", "memory", "write", "shipped m5.1"]);
    expect_memory(&[
        "flowforge",
        "memory",
        "write",
        "L5 SDE",
        "--curated",
        "--stratum",
        "identity",
    ]);
}

/// `--stratum` and `--daily` are mutually exclusive (stratum implies curated),
/// mirroring the runtime conflict check in `MemoryWriteTool::run`. Clap rejects
/// the combination at parse time with `ArgumentConflict`.
#[test]
fn memory_write_stratum_and_daily_conflict() {
    let err = match Cli::try_parse_from([
        "flowforge",
        "memory",
        "write",
        "x",
        "--daily",
        "--stratum",
        "identity",
    ]) {
        Ok(_) => panic!("--stratum and --daily must be mutually exclusive"),
        Err(err) => err,
    };
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

/// `--daily` and `--curated` are mutually exclusive targets.
#[test]
fn memory_write_daily_and_curated_conflict() {
    let err =
        match Cli::try_parse_from(["flowforge", "memory", "write", "x", "--daily", "--curated"]) {
            Ok(_) => panic!("--daily and --curated must be mutually exclusive"),
            Err(err) => err,
        };
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

// -- goal subcommand parsing (issue #1082) --------------------------------

#[test]
fn goal_objective_parses() {
    let cli = Cli::try_parse_from(["flowforge", "goal", "ship the feature"]).unwrap();
    match cli.command.expect("goal present") {
        super::Command::Goal(args) => {
            assert_eq!(args.objective.as_deref(), Some("ship the feature"));
            assert!(args.command.is_none());
        }
        other => panic!("expected Goal, got {other:?}"),
    }
}

#[test]
fn goal_with_session_flag_parses() {
    let cli =
        Cli::try_parse_from(["flowforge", "goal", "do thing", "--session", "sess-1"]).unwrap();
    match cli.command.expect("goal present") {
        super::Command::Goal(args) => {
            assert_eq!(args.objective.as_deref(), Some("do thing"));
            assert_eq!(args.session.as_deref(), Some("sess-1"));
        }
        other => panic!("expected Goal, got {other:?}"),
    }
}

#[test]
fn goal_list_subcommand_parses() {
    let cli = Cli::try_parse_from(["flowforge", "goal", "list"]).unwrap();
    match cli.command.expect("goal present") {
        super::Command::Goal(args) => {
            assert!(args.objective.is_none());
            match args.command {
                Some(super::goal::GoalSubCommand::List) => {}
                other => panic!("expected List, got {other:?}"),
            }
        }
        other => panic!("expected Goal, got {other:?}"),
    }
}

#[test]
fn goal_resume_subcommand_parses() {
    let cli = Cli::try_parse_from(["flowforge", "goal", "resume", "sess-1"]).unwrap();
    match cli.command.expect("goal present") {
        super::Command::Goal(args) => {
            assert!(args.objective.is_none());
            match args.command {
                Some(super::goal::GoalSubCommand::Resume { session }) => {
                    assert_eq!(session, "sess-1");
                }
                other => panic!("expected Resume, got {other:?}"),
            }
        }
        other => panic!("expected Goal, got {other:?}"),
    }
}

#[test]
fn goal_cancel_subcommand_parses() {
    let cli = Cli::try_parse_from(["flowforge", "goal", "cancel", "sess-1"]).unwrap();
    match cli.command.expect("goal present") {
        super::Command::Goal(args) => {
            assert!(args.objective.is_none());
            match args.command {
                Some(super::goal::GoalSubCommand::Cancel { session }) => {
                    assert_eq!(session, "sess-1");
                }
                other => panic!("expected Cancel, got {other:?}"),
            }
        }
        other => panic!("expected Goal, got {other:?}"),
    }
}

// -- #1080: session persistence ------------------------------------------

/// The `--ephemeral` flag parses on `run` (the escape hatch for one-shot runs).
#[test]
fn run_accepts_ephemeral_flag() {
    let cli = Cli::try_parse_from(["flowforge", "run", "hi", "--ephemeral"]).expect("parses");
    match cli.command.expect("run present") {
        super::Command::Run { ephemeral, .. } => assert!(ephemeral),
        other => panic!("expected Run, got {other:?}"),
    }
}

/// The `--ephemeral` flag parses on `chat`.
#[test]
fn chat_accepts_ephemeral_flag() {
    let cli = Cli::try_parse_from(["flowforge", "chat", "--ephemeral"]).expect("parses");
    match cli.command.expect("chat present") {
        super::Command::Chat { ephemeral, .. } => assert!(ephemeral),
        other => panic!("expected Chat, got {other:?}"),
    }
}

/// The `--resume <ID>` flag parses on `chat`.
#[test]
fn chat_accepts_resume_flag() {
    let cli = Cli::try_parse_from(["flowforge", "chat", "--resume", "abc-123"]).expect("parses");
    match cli.command.expect("chat present") {
        super::Command::Chat { resume, .. } => {
            assert_eq!(resume.as_deref(), Some("abc-123"));
        }
        other => panic!("expected Chat, got {other:?}"),
    }
}

/// `ff sessions list` parses through the real clap tree.
#[test]
fn sessions_list_subcommand_parses() {
    let cli = Cli::try_parse_from(["flowforge", "sessions", "list"]).expect("parses");
    match cli.command.expect("sessions present") {
        super::Command::Sessions { command } => {
            assert!(matches!(command, super::SessionsCommand::List));
        }
        other => panic!("expected Sessions, got {other:?}"),
    }
}

/// `ff fork <id>` parses through the real clap tree.
#[test]
fn fork_subcommand_parses() {
    let cli = Cli::try_parse_from(["flowforge", "fork", "src-id"]).expect("parses");
    match cli.command.expect("fork present") {
        super::Command::Fork { id } => assert_eq!(id, "src-id"),
        other => panic!("expected Fork, got {other:?}"),
    }
}

/// Default `chat` (no subcommand) yields `None` — the persistent default is
/// applied in `main()`. Explicitly invoking `chat` (no flags) defaults to
/// persistent + no resume, the acceptance contract from #1080.
#[test]
fn default_chat_is_persistent_no_resume() {
    // No subcommand: clap yields `command: None`; main() unwraps to the Chat
    // default. Verify the explicit `chat` invocation instead.
    let cli = Cli::try_parse_from(["flowforge", "chat"]).expect("parses");
    match cli.command.expect("chat present") {
        super::Command::Chat {
            ephemeral, resume, ..
        } => {
            assert!(!ephemeral, "default must be persistent");
            assert!(resume.is_none(), "default has no resume id");
        }
        other => panic!("expected Chat, got {other:?}"),
    }
}

/// An on-disk session store survives a reopen — the foundation of #1080.
/// Mirrors `session_db_survives_restart` in the desktop's `state/tests.rs` and
/// `edit_user_message_truncation_survives_reopen` in `ff-session/src/tests.rs`.
#[test]
fn session_db_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let session_id;
    {
        let store = SessionStore::open(&path).unwrap();
        let s = store.create_session(None);
        session_id = s.id.clone();
        store.add_message(&s.id, Role::User, "hello".into());
        store.add_message(&s.id, Role::Assistant, "world".into());
    }
    // Reopen and assert the transcript survived.
    let store = SessionStore::open(&path).unwrap();
    let msgs = store.get_messages(&session_id);
    assert_eq!(msgs.len(), 2, "both messages should survive reopen");
    assert_eq!(msgs[0].role, Role::User);
    assert_eq!(msgs[0].content, "hello");
    assert_eq!(msgs[1].role, Role::Assistant);
    assert_eq!(msgs[1].content, "world");
    // The session row itself survived too.
    assert!(store.get_session(&session_id).is_some());
}

/// The acceptance round-trip from #1080: run `chat` → exit → reopen → resume
/// and assert the message history survives. Uses `chat_repl` with an injected
/// on-disk store so the test doesn't touch the real config dir. Mirrors
/// `chat_multi_turn_context_persists` but split across two process-equivalent
/// sessions (drop + reopen the store between them).
#[tokio::test]
async fn chat_persists_across_reopen_and_resume() {
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let session_id;

    // Turn 1: open the store, create a session, send one message, exit.
    {
        let store = SessionStore::open(&path).unwrap();
        let s = store.create_session(None);
        session_id = s.id.clone();
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approver = TestApprover;
        let matrix = PermissionMatrix::default();
        let tool_ctx = ToolContext::new(&registry, &root, &approver, 8, &matrix);
        let memory_store = Arc::new(Memory::with_default_root(MemoryConfig::default()));
        let skills = SkillRegistry::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider = RecordingProvider { seen: seen.clone() };

        let code = super::chat_repl(
            &provider,
            "mock",
            &skills,
            &store,
            &memory_store,
            None,
            &tool_ctx,
            &session_id,
            false,
            ff_core::Mode::Auto,
            Cursor::new(b"first question\nexit\n"),
        )
        .await;
        assert_eq!(code, ExitCode::SUCCESS);

        // The first turn's messages must be in the store before we drop it.
        let msgs = store.get_messages(&session_id);
        assert_eq!(msgs.len(), 2, "user + assistant after one turn");
        assert_eq!(msgs[0].content, "first question");
    }

    // Store handle dropped here; the db file persists on disk.

    // Turn 2: reopen the store, resume the same session, send a second message.
    let store = SessionStore::open(&path).unwrap();
    // The resumed session's history must be intact.
    let history = store.get_messages(&session_id);
    assert_eq!(
        history.len(),
        2,
        "resumed session must see the first turn's history"
    );
    assert_eq!(history[0].content, "first question");

    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approver = TestApprover;
    let matrix = PermissionMatrix::default();
    let tool_ctx = ToolContext::new(&registry, &root, &approver, 8, &matrix);
    let memory_store = Arc::new(Memory::with_default_root(MemoryConfig::default()));
    let skills = SkillRegistry::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let provider = RecordingProvider { seen: seen.clone() };

    let code = super::chat_repl(
        &provider,
        "mock",
        &skills,
        &store,
        &memory_store,
        None,
        &tool_ctx,
        &session_id, // resume the same session
        false,
        ff_core::Mode::Auto,
        Cursor::new(b"second question\nexit\n"),
    )
    .await;
    assert_eq!(code, ExitCode::SUCCESS);

    // The second turn's provider request must include the first turn's
    // messages — proving resumed context.
    let msgs = seen.lock().unwrap();
    let contents: Vec<Option<&str>> = msgs.iter().map(|m| m.content.as_deref()).collect();
    assert!(
        contents.contains(&Some("first question")),
        "resumed turn must see first question: {contents:?}"
    );
    assert!(
        contents.contains(&Some("second question")),
        "resumed turn must see second question: {contents:?}"
    );

    // And the store now has all 4 messages.
    let all = store.get_messages(&session_id);
    assert_eq!(all.len(), 4, "two user + two assistant after two turns");
}

/// `ff fork <id>` with a `TestEnv`-isolated store: forks a titled session and
/// stamps the `(Fork 1)` title for desktop parity (#1069).
#[test]
fn fork_assigns_fork_1_title_to_persisted_store() {
    let _env = TestEnv::new();
    let store = crate::host::build_session_store(false);
    let s = store.create_session(None);
    store.set_title(&s.id, "Refactor auth".into());
    store.add_message(&s.id, Role::User, "let's go".into());

    let code = super::fork_session_cmd(&s.id);
    assert_eq!(code, ExitCode::SUCCESS);

    let all = store.list_sessions();
    let forked = all
        .iter()
        .find(|x| x.id != s.id)
        .expect("forked session exists");
    assert_eq!(
        forked.title.as_deref(),
        Some("Refactor auth (Fork 1)"),
        "forked title must be (Fork 1) for desktop parity"
    );
    // The forked transcript carries the source's messages.
    let msgs = store.get_messages(&forked.id);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "let's go");
}

/// `ff fork` on an untitled session (no messages → no auto-title) still
/// succeeds and keeps the store's copy naming (no `(Fork N)` since there's no
/// title to base it on — mirroring the desktop's `if (session.title)` guard).
#[test]
fn fork_untitled_session_succeeds_without_fork_n_title() {
    let _env = TestEnv::new();
    let store = crate::host::build_session_store(false);
    let s = store.create_session(None);
    // No messages → no auto-title → source.title stays None.

    let code = super::fork_session_cmd(&s.id);
    assert_eq!(code, ExitCode::SUCCESS);

    let all = store.list_sessions();
    let forked = all.iter().find(|x| x.id != s.id).expect("forked exists");
    // Untitled source → fork_session stamps None for the title; no (Fork N).
    assert!(
        forked.title.is_none(),
        "untitled fork must not get a title: {:?}",
        forked.title
    );
}

/// `ff fork` on a second fork of the same base gets `(Fork 2)`.
#[test]
fn fork_increments_to_fork_2() {
    let _env = TestEnv::new();
    let store = crate::host::build_session_store(false);
    let s = store.create_session(None);
    store.set_title(&s.id, "Refactor auth".into());
    store.add_message(&s.id, Role::User, "msg".into());

    // First fork → (Fork 1)
    super::fork_session_cmd(&s.id);
    // Second fork from the same source → (Fork 2)
    let code = super::fork_session_cmd(&s.id);
    assert_eq!(code, ExitCode::SUCCESS);

    let all = store.list_sessions();
    let forks: Vec<_> = all
        .iter()
        .filter(|x| {
            x.title
                .as_deref()
                .unwrap_or("")
                .starts_with("Refactor auth (Fork")
        })
        .collect();
    assert_eq!(forks.len(), 2, "two forks should exist");
    let titles: Vec<&str> = forks.iter().map(|f| f.title.as_deref().unwrap()).collect();
    assert!(titles.contains(&"Refactor auth (Fork 1)"), "{titles:?}");
    assert!(titles.contains(&"Refactor auth (Fork 2)"), "{titles:?}");
}

/// `ff fork <bogus-id>` errors cleanly.
#[test]
fn fork_unknown_id_errors() {
    let _env = TestEnv::new();
    let code = super::fork_session_cmd("no-such-session");
    assert_eq!(code, ExitCode::FAILURE);
}

/// `ff sessions list` prints persisted sessions from the store. Uses `TestEnv`
/// to isolate the store so the test doesn't touch the real config dir.
#[test]
fn sessions_list_prints_persisted_sessions() {
    let _env = TestEnv::new();
    let store = crate::host::build_session_store(false);
    let s1 = store.create_session(None);
    store.set_title(&s1.id, "First session".into());
    store.add_message(&s1.id, Role::User, "hi".into());
    let s2 = store.create_session(None);
    store.add_message(&s2.id, Role::User, "yo".into()); // auto-titled from first msg

    let code = super::sessions_list();
    assert_eq!(code, ExitCode::SUCCESS);
    // The store-side assertion that both sessions are listed (the rendering
    // itself is unit-tested in sessions/tests.rs::render_list_*).
    let all = store.list_sessions();
    assert_eq!(all.len(), 2, "both sessions persisted");
}

/// `ff sessions list` on an empty store exits 0 (with a stderr hint).
#[test]
fn sessions_list_empty_exits_success() {
    let _env = TestEnv::new();
    let code = super::sessions_list();
    assert_eq!(code, ExitCode::SUCCESS);
}

/// `host::build_session_store(false)` uses the `TestEnv`-isolated config dir,
/// so two calls within the same `TestEnv` share one on-disk db.
#[test]
fn build_session_store_persists_within_test_env() {
    let _env = TestEnv::new();
    let id;
    {
        let store = crate::host::build_session_store(false);
        let s = store.create_session(None);
        id = s.id.clone();
        store.add_message(&s.id, Role::User, "persisted".into());
    }
    // A second build reopens the same on-disk db (TestEnv isolates the path).
    let store = crate::host::build_session_store(false);
    assert!(
        store.get_session(&id).is_some(),
        "session must survive reopen"
    );
    let msgs = store.get_messages(&id);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "persisted");
}

/// `host::build_session_store(true)` is always in-memory regardless of `TestEnv`.
#[test]
fn build_session_store_ephemeral_is_in_memory() {
    let _env = TestEnv::new();
    let id;
    {
        let store = crate::host::build_session_store(true);
        let s = store.create_session(None);
        id = s.id.clone();
        store.add_message(&s.id, Role::User, "ephemeral".into());
    }
    // A second ephemeral build gets a fresh in-memory db — nothing persists.
    let store = crate::host::build_session_store(true);
    assert!(
        store.get_session(&id).is_none(),
        "ephemeral must not persist"
    );
}
