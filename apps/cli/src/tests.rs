use std::process::ExitCode;

use super::json_events;
use super::{approval_mode, build_registry_with_memory, resolve_turn_inputs, Cli};
use crate::approver::ApprovalMode;
use async_trait::async_trait;
use clap::CommandFactory;
use clap::Parser;
use ff_agent::{run_turn, AgentEvent, Approver, CancelToken, ToolContext};
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
