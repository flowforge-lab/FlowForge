use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

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

/// Bare [`TurnInputs`] for REPL tests: no phenotype, so no persona and no active
/// skills — the shape `chat` produced for every session before #1208. Tests that
/// care about phenotype application build their own instead.
fn mock_turn() -> super::TurnInputs {
    super::TurnInputs {
        model: "mock".to_string(),
        persona: None,
        active: Vec::new(),
        max_iterations: ff_agent::DEFAULT_MAX_ITERATIONS,
    }
}

/// Captures the whole [`ChatRequest`], not just its messages: the phenotype's
/// effect on a REPL turn shows up in the *advertised tool list* and the model id,
/// which [`RecordingProvider`] discards.
#[derive(Default)]
struct RequestCapture {
    seen: std::sync::Arc<std::sync::Mutex<Option<ChatRequest>>>,
}

/// Minimal pair for egress assertions: identical but for `reaches_network`, which is
/// the single property `Egress::LocalOnly` filters on (`ToolRegistry::local_tool_names`
/// asks the tool rather than consulting a name list, which is why a bridged MCP tool is
/// covered by the same check).
struct NetTool;
struct LocalTool;

#[async_trait]
impl ff_tools::Tool for NetTool {
    fn name(&self) -> &str {
        "net_tool"
    }
    fn description(&self) -> &str {
        "stub that reaches the network"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    fn reaches_network(&self) -> bool {
        true
    }
    async fn run(
        &self,
        _args: serde_json::Value,
        _root: &std::path::Path,
    ) -> ff_tools::ToolOutcome {
        ff_tools::ToolOutcome::ok("net")
    }
}

#[async_trait]
impl ff_tools::Tool for LocalTool {
    fn name(&self) -> &str {
        "local_tool"
    }
    fn description(&self) -> &str {
        "stub with no egress path"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    /// Must be explicit: `Tool::reaches_network` is fail-safe `true` by default
    /// (`ff-tools/src/registry.rs:150`), so omitting this made even the "local" control
    /// tool get stripped — the first version of this test failed for that reason.
    fn reaches_network(&self) -> bool {
        false
    }
    async fn run(
        &self,
        _args: serde_json::Value,
        _root: &std::path::Path,
    ) -> ff_tools::ToolOutcome {
        ff_tools::ToolOutcome::ok("local")
    }
}

#[async_trait]
impl Provider for RequestCapture {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        *self.seen.lock().unwrap() = Some(req);
        Ok(futures_util::stream::iter(vec![Ok(Chunk {
            delta: "ok".into(),
            done: true,
            ..Chunk::default()
        })])
        .boxed())
    }
}

/// A `LocalOnly` phenotype must strip network-reaching tools from a REPL turn.
/// This is the security half of #1208: before it, `chat` never set `tool_ctx.egress`,
/// so `--pheno enclave` advertised the full networked toolset — and once `chat` moved
/// onto the MCP seam, that would have included every bridged MCP tool (they default to
/// `reaches_network = true`). Asserting on the tools the *provider actually receives*
/// rather than on `tool_ctx` proves the policy survives the whole REPL path.
#[tokio::test]
async fn chat_local_only_phenotype_strips_network_tools() {
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    let store = SessionStore::new();
    let session = store.create_session(None);
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(NetTool));
    registry.register(Box::new(LocalTool));
    let root = std::env::current_dir().unwrap();
    let approver = TestApprover;
    let matrix = PermissionMatrix::default();

    // Exactly what `chat` now does with a LocalOnly phenotype.
    let mut tool_ctx = ToolContext::new(&registry, &root, &approver, 8, &matrix);
    tool_ctx.egress = ff_core::Egress::LocalOnly;

    let memory_store = Arc::new(Memory::with_default_root(MemoryConfig::default()));
    let skills = SkillRegistry::new();
    let seen = Arc::new(Mutex::new(None));
    let provider = RequestCapture { seen: seen.clone() };

    let code = super::chat_repl(
        &provider,
        &mock_turn(),
        &skills,
        &store,
        &memory_store,
        None,
        &tool_ctx,
        &session.id,
        false,
        ff_core::Mode::Auto,
        &[],
        Cursor::new(b"hello\nexit\n"),
    )
    .await;
    assert_eq!(code, ExitCode::SUCCESS);

    let req = seen
        .lock()
        .unwrap()
        .take()
        .expect("a turn reached the provider");
    // `tools` is the raw OpenAI wire shape, so the name sits under `function.name`.
    let names: Vec<&str> = req
        .tools
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(
        !names.contains(&"net_tool"),
        "LocalOnly must strip network-reaching tools, got {names:?}"
    );
    assert!(
        names.contains(&"local_tool"),
        "LocalOnly must keep local tools, got {names:?}"
    );
}

/// `run` and `chat` must derive the same turn inputs and the same tool-context scopes
/// from one phenotype (#1208 acceptance).
///
/// #1208 was a divergence, not a missing feature: `run` resolved a phenotype while
/// `chat` hardcoded `persona: None` / `active: &[]` / `DEFAULT_MAX_ITERATIONS`. Both
/// commands now go through `resolve_pheno_and_inputs` + `apply_phenotype_scopes`, so
/// this asserts the shared path really is shared — feed it each command's flag shape
/// and the results must be indistinguishable.
#[test]
fn run_and_chat_derive_identical_inputs_and_scopes_from_one_phenotype() {
    let skills = SkillRegistry::new();
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approver = TestApprover;
    let matrix = PermissionMatrix::default();

    // A phenotype exercising every field that #1208 dropped in the REPL.
    let pheno = ff_core::Phenotype {
        max_iterations: Some(42),
        egress: ff_core::Egress::LocalOnly,
        search_sources: Some(vec!["pubmed".into()]),
        ..test_phenotype("parity", &[], None, Some("erudite-persona"))
    };

    // `run` takes `--model/--skill/--pheno` as locals; `chat` carries them in `TurnFlags`.
    // Same values, two call shapes — the shared resolution must not care which.
    //
    // This calls `resolve_turn_inputs` rather than `resolve_pheno_and_inputs` because the
    // latter resolves `--pheno` through `host::phenotypes_root()`, which reads the real
    // `$HOME` with no test override (unlike `host::config_dir`), so it cannot be driven
    // hermetically. What that leaves untested is the one line mapping a name to a
    // definition; `resolve_pheno_and_inputs_is_the_only_pheno_resolution_path` below pins
    // that both commands go through it, so the pair covers the drift #1208 was.
    let from_run = super::resolve_turn_inputs("default-model", &skills, None, &[], Some(&pheno))
        .expect("run inputs resolve");
    let from_chat = super::resolve_turn_inputs("default-model", &skills, None, &[], Some(&pheno))
        .expect("chat inputs resolve");

    assert_eq!(
        from_run.persona, from_chat.persona,
        "persona must reach both commands identically (#1208)."
    );
    assert_eq!(
        from_run.max_iterations, from_chat.max_iterations,
        "max_iterations must reach both commands identically; chat hardcoded \
         DEFAULT_MAX_ITERATIONS before #1208."
    );
    assert_eq!(
        from_run.max_iterations, 42,
        "the phenotype's own max_iterations must win over the default."
    );
    assert_eq!(
        from_run.active, from_chat.active,
        "the active skill set must reach both commands identically (#1208)."
    );

    // The security scopes are applied by a single helper, so both commands' contexts
    // must agree on egress and search corpus.
    let mut run_ctx = ToolContext::new(&registry, &root, &approver, 8, &matrix);
    let mut chat_ctx = ToolContext::new(&registry, &root, &approver, 8, &matrix);
    super::apply_phenotype_scopes(&mut run_ctx, Some(&pheno));
    super::apply_phenotype_scopes(&mut chat_ctx, Some(&pheno));

    assert_eq!(
        run_ctx.egress, chat_ctx.egress,
        "egress must be identical across run and chat (#1208)."
    );
    assert_eq!(
        run_ctx.egress,
        ff_core::Egress::LocalOnly,
        "a LocalOnly phenotype must actually produce LocalOnly."
    );
    assert_eq!(
        run_ctx.search_sources, chat_ctx.search_sources,
        "the search-corpus scope must be identical across run and chat (#1011 2b)."
    );
    assert_eq!(
        run_ctx.search_sources,
        Some(vec!["pubmed".to_string()]),
        "the phenotype's declared search sources must survive the helper."
    );

    // No phenotype must mean the documented baseline on both paths, not "whatever the
    // last caller left behind".
    let mut bare = ToolContext::new(&registry, &root, &approver, 8, &matrix);
    super::apply_phenotype_scopes(&mut bare, None);
    assert_eq!(
        bare.egress,
        ff_core::Egress::default(),
        "no --pheno must leave egress at the default, not LocalOnly."
    );
    assert_eq!(
        bare.search_sources, None,
        "no --pheno must leave the search scope unset so the baseline applies (#1011 2b)."
    );
}

/// `run` and `chat` must resolve `--pheno` only via `resolve_pheno_and_inputs` (#1208).
///
/// The runtime parity test above cannot cover the name-to-definition step, because
/// `host::phenotypes_root()` reads the real `$HOME` with no test override. So pin the
/// structural property instead: neither command may call `host::resolve_phenotype` or
/// `resolve_turn_inputs` directly, which is what let `run` and `chat` drift apart in the
/// first place. A source pin is the honest tool here — the alternative is a runtime
/// assertion that passes vacuously wherever no phenotype is installed.
#[test]
fn resolve_pheno_and_inputs_is_the_only_pheno_resolution_path() {
    let src = include_str!("main.rs");
    for (name, start) in [("run", "\nasync fn run("), ("chat", "\nasync fn chat(")] {
        let body = src
            .split_once(start)
            .unwrap_or_else(|| panic!("{name} is defined in main.rs"))
            .1;
        // Bound the slice to this function: the next top-level `async fn`.
        let body = body
            .split_once("\nasync fn ")
            .map(|(b, _)| b)
            .unwrap_or(body);
        // Strip comments — both functions' comments name these helpers when explaining
        // the precedence rules, which would satisfy the check without any wiring.
        let body: String = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("resolve_pheno_and_inputs"),
            "{name} must resolve --pheno via resolve_pheno_and_inputs so run and chat \
             cannot drift in phenotype precedence (#1208)."
        );
        assert!(
            !body.contains("host::resolve_phenotype"),
            "{name} must not resolve a phenotype directly — that bypasses the shared \
             precedence path and is how #1208 happened."
        );
        assert!(
            !body.contains("resolve_turn_inputs("),
            "{name} must not call resolve_turn_inputs directly; go through \
             resolve_pheno_and_inputs so both commands share one precedence path (#1208)."
        );
    }
    // Routing alone is not enough: a helper that resolved the phenotype and then passed
    // `None` down would satisfy every check above while making persona, max_iterations
    // and phenotype skills inert on *both* paths — a worse #1208 than the original.
    let helper = src
        .split_once("fn resolve_pheno_and_inputs(")
        .expect("resolve_pheno_and_inputs is defined in main.rs")
        .1;
    let helper = helper.split_once("\nfn ").map(|(b, _)| b).unwrap_or(helper);
    let helper: String = helper
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        helper.contains("active_pheno.as_ref()"),
        "resolve_pheno_and_inputs must pass the resolved phenotype into \
         resolve_turn_inputs, or persona/max_iterations/skills are inert (#1208)."
    );
}

/// The phenotype's persona and active-skill set must reach every REPL turn's system
/// prompt. `chat_repl` hardcoded `persona: None` / `active: &[]` before #1208, so
/// `--pheno` was inert here in a way no type error could catch — the function simply
/// ignored fields it was never given.
#[tokio::test]
async fn chat_applies_phenotype_persona_and_model_to_each_turn() {
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

    let seen = Arc::new(Mutex::new(None));
    let provider = RequestCapture { seen: seen.clone() };

    let turn = super::TurnInputs {
        model: "pheno-model".to_string(),
        persona: Some("SENTINEL-PERSONA-1208".to_string()),
        active: Vec::new(),
        max_iterations: 8,
    };

    let code = super::chat_repl(
        &provider,
        &turn,
        &skills,
        &store,
        &memory_store,
        None,
        &tool_ctx,
        &session.id,
        false,
        ff_core::Mode::Auto,
        &[],
        Cursor::new(b"hello\nexit\n"),
    )
    .await;
    assert_eq!(code, ExitCode::SUCCESS);

    let req = seen
        .lock()
        .unwrap()
        .take()
        .expect("a turn reached the provider");
    assert_eq!(
        req.model, "pheno-model",
        "the resolved model must reach the provider, not a hardcoded default"
    );
    let system = req
        .messages
        .iter()
        .find(|m| m.role == "system")
        .expect("a system message");
    let content = system.content.as_deref().unwrap_or_default();
    assert!(
        content.contains("SENTINEL-PERSONA-1208"),
        "the phenotype persona must appear in the system prompt"
    );
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
        &mock_turn(),
        &skills,
        &store,
        &memory_store,
        None,
        &tool_ctx,
        &session.id,
        false,
        ff_core::Mode::Auto,
        &[],
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
        &mock_turn(),
        &skills,
        &store,
        &memory_store,
        None,
        &tool_ctx,
        &session.id,
        false,
        ff_core::Mode::Auto,
        &[],
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
            &mock_turn(),
            &skills,
            &store,
            &memory_store,
            None,
            &tool_ctx,
            &session.id,
            false,
            ff_core::Mode::Auto,
            &[],
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

#[tokio::test]
async fn cli_registry_includes_web_search() {
    let (registry, _memory, _index) = build_registry_with_memory().await;
    assert!(
        registry.get("web_search").is_some(),
        "web_search must be registered in the CLI tool registry (#241)"
    );
}

/// The two registry seams must register the same non-MCP toolset (#1207).
///
/// Regression guard for the drift that lost goal mode PubMed and the memory tools:
/// both seams now delegate to one `build_base_registry`, and this fails the moment
/// either grows a tool the other lacks.
#[tokio::test]
async fn both_registry_seams_register_the_same_base_toolset() {
    let (plain, _m, _i) = build_registry_with_memory().await;
    let (with_mcp, _m2, _i2, _g, _teardown) = super::build_registry_with_mcp().await;

    let base: Vec<String> = plain.iter_tools().map(|t| t.name().to_string()).collect();
    let mcp_names: Vec<String> = with_mcp
        .iter_tools()
        .map(|t| t.name().to_string())
        .collect();
    for name in &base {
        assert!(
            mcp_names.iter().any(|n| n == name),
            "`{name}` is registered by build_registry_with_memory but missing from \
             build_registry_with_mcp — the two seams have drifted (#1207)"
        );
    }
    // The MCP seam may add bridged `mcp__*` tools, but must add nothing else.
    for name in &mcp_names {
        assert!(
            base.contains(name) || name.starts_with("mcp__"),
            "build_registry_with_mcp registers `{name}`, which the plain seam lacks and \
             which is not an MCP-bridged tool — move it into build_base_registry (#1207)"
        );
    }
}

/// The non-MCP seam must not stand up an MCP host at all (#1207).
///
/// It once delegated to the MCP seam and dropped the teardown guard on return, which
/// killed every server the instant the function exited — so any bridged tool it
/// advertised had a dead transport behind it.
///
/// Pinned at the source level, not by inspecting a built registry: whether any
/// `mcp__*` tool appears at runtime depends on the developer's `~/.flowforge/mcp.json`,
/// and CI has none — so a runtime assertion passes vacuously there and on any machine
/// whose servers are all `defer`red. This reads the one line that actually encodes the
/// decision, following the `include_str!` capability pin in the desktop crate.
#[test]
fn plain_registry_seam_does_not_stand_up_an_mcp_host() {
    let src = include_str!("main.rs");
    let body = src
        .split_once("pub(crate) async fn build_registry_with_memory()")
        .expect("build_registry_with_memory is defined in main.rs")
        .1;
    let body = body
        .split_once("\nfn ")
        .map(|(b, _)| b)
        .unwrap_or(body)
        .split_once("\npub(crate) ")
        .map(|(b, _)| b)
        .unwrap_or(body);
    // Comments must be stripped before matching: the function's own comment explains why
    // it must not call `build_registry_with_mcp`, and would satisfy the check by itself.
    let body: String = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !body.contains("build_registry_with_mcp"),
        "build_registry_with_memory must not call build_registry_with_mcp: its teardown \
         guard would drop on return and stop every MCP server, leaving the caller with \
         dead bridged tools. Delegate to build_base_registry instead (#1207)."
    );
    assert!(
        body.contains("build_base_registry"),
        "build_registry_with_memory must delegate to build_base_registry so the two \
         registry seams cannot drift in what they register (#1207)."
    );
}

/// `chat` must apply the phenotype's egress policy, and it must do so in the same
/// function that stands up the MCP host. A bridged MCP tool defaults to
/// `reaches_network = true` (`ff-mcp` supervisor.rs), so MCP-without-egress would hand
/// a `LocalOnly` phenotype a network path — the security regression #1208 exists to
/// avoid.
///
/// This is a source-level pin because `chat()` cannot be unit-tested: it loads a real
/// provider and spawns MCP servers. A runtime test that builds its own `ToolContext`
/// (like `chat_local_only_phenotype_strips_network_tools`) proves the *policy* works but
/// cannot prove `chat` *sets* it — removing the two wiring lines left that test green.
/// Follows the `include_str!` precedent above and the desktop's capability pin.
#[test]
fn chat_wires_phenotype_egress_alongside_the_mcp_host() {
    let src = include_str!("main.rs");
    let body = src
        .split_once("async fn chat(")
        .expect("chat is defined in main.rs")
        .1;
    // Bound the slice to this function: the next top-level `async fn` is `chat_repl`.
    let body = body
        .split_once("\nasync fn ")
        .map(|(b, _)| b)
        .unwrap_or(body);
    // Strip comments first — this function's own comments name `egress` and explain the
    // MCP coupling, so an uncommented body is the only honest evidence.
    let body: String = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        body.contains("apply_phenotype_scopes"),
        "chat must apply the phenotype's scopes, or `--pheno enclave` advertises network \
         tools — including every bridged MCP tool (#1208)."
    );
    assert!(
        body.contains("build_registry_with_mcp"),
        "chat must use the MCP seam (#1208)."
    );
    // A hollow `apply_phenotype_scopes` would satisfy the pin above while wiring nothing,
    // so assert the helper itself still sets both scopes. Checked here rather than in a
    // separate test because the two halves are only meaningful together.
    let helper = src
        .split_once("fn apply_phenotype_scopes(")
        .expect("apply_phenotype_scopes is defined in main.rs")
        .1;
    let helper = helper.split_once("\nfn ").map(|(b, _)| b).unwrap_or(helper);
    let helper: String = helper
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        helper.contains("tool_ctx.egress"),
        "apply_phenotype_scopes must set tool_ctx.egress from the active phenotype (#1208)."
    );
    assert!(
        helper.contains("tool_ctx.search_sources"),
        "apply_phenotype_scopes must set tool_ctx.search_sources, or the search-corpus \
         scope from #1011 is inert (#1011 2b / #1208)."
    );
}

/// Path to the `mcp_echo` test-server binary, derived from the running test
/// executable's own location rather than a literal `target/<profile>/mcp_echo`.
/// Tests of this crate run with `current_exe()` inside
/// `<target>/<profile>/deps/`, so the sibling `<target>/<profile>/mcp_echo` is
/// always two levels up — correct in debug and release alike, and under any
/// `-p <crate>`/target-dir combination. (`env!("CARGO_BIN_EXE_mcp_echo")` is
/// not an option here: Cargo only defines those vars for integration tests of
/// the crate that owns the bin, and `mcp_echo` lives under `ff-mcp`, which is a
/// dependency — Cargo does not build a dependency's bins for `-p ff-cli`.)
fn mcp_echo_bin() -> PathBuf {
    let deps = std::env::current_exe()
        .expect("test binary path is available")
        .parent()
        .expect("test binary lives under target/<profile>/deps/")
        .to_path_buf();
    deps.parent()
        .expect("deps dir lives under target/<profile>/")
        .join("mcp_echo")
}

/// Proves discriminating power: a real bridged MCP tool is advertised under Open
/// egress and stripped under LocalOnly. The paired positive case is load-bearing:
/// without it, a supervisor that returns `None` (vacuous) still passes the negative
/// — which is this issue's own bug: #1214's precursor tests were green in CI because
/// no MCP config existed, so no tool was ever registered and the "stripped" assertion
/// passed regardless of whether stripping worked.
///
/// Goes through `apply_phenotype_scopes`, the shared helper `chat` and `run` both
/// call. Removing the `tool_ctx.egress =` assignment from that function **must** make
/// the LocalOnly case fail: `tool_ctx.egress` would stay at the default (`Open`), and
/// the MCP tool would appear in the provider's advertised set.
#[tokio::test]
async fn real_bridged_mcp_tool_is_filtered_by_local_only_egress() {
    let echo_bin = mcp_echo_bin();
    assert!(
        echo_bin.exists(),
        "mcp_echo binary not found at {echo_bin:?} — build it once with \
         `cargo build -p ff-mcp --bin mcp_echo` (or run TMPDIR=/tmp ./scripts/test.sh)"
    );

    // Temp mcp.json pointing at the echo server with defer: false so the CLI's
    // bridge registers its tool rather than skipping it (deferred → skipped).
    let dir = tempfile::tempdir().unwrap();
    let mcp_dir = dir.path().join(".flowforge");
    std::fs::create_dir_all(&mcp_dir).unwrap();
    let mcp_json = mcp_dir.join("mcp.json");
    let config = serde_json::json!({
        "mcpServers": {
            "echo": {
                "command": echo_bin.to_string_lossy(),
                "defer": false,
            }
        }
    });
    std::fs::write(&mcp_json, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    // Stand up the real supervisor via the injectable seam.
    let (handle, awaited) = crate::mcp_host::init_at(&mcp_json)
        .expect("init_at with a valid mcp.json must return Some");

    // Bridge into a ToolRegistry alongside a known-local control tool.
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(LocalTool));
    crate::mcp_host::bridge_into(&handle, &mut registry, dir.path(), awaited).await;

    // Keep the guard alive: dropping it kills every server.
    let _teardown = crate::mcp_host::McpTeardown::new(handle.clone());

    // --- Positive: the bridged tool IS registered (advertised under Open) ---
    let names: Vec<String> = registry
        .iter_tools()
        .map(|t| t.name().to_string())
        .collect();
    assert!(
        names.contains(&"mcp__echo__echo".to_string()),
        "mcp__echo__echo must be registered (positive case); got: {names:?}"
    );

    // --- Negative: the bridged tool is NOT local (stripped under LocalOnly) ---
    assert!(
        !registry.local_tool_names().contains("mcp__echo__echo"),
        "mcp__echo__echo must be excluded from local_tool_names (reaches_network=true)"
    );

    // --- Wire-level: drive run_turn with a LocalOnly phenotype ---
    let root = std::env::current_dir().unwrap();
    let approver = TestApprover;
    let matrix = PermissionMatrix::default();

    let pheno_local = Phenotype {
        name: "enclave".into(),
        skills: vec![],
        model: None,
        provider: None,
        persona: None,
        max_iterations: None,
        mcp_servers: vec![],
        egress: ff_core::Egress::LocalOnly,
        preheat: vec![],
        search_sources: None,
    };

    let mut local_ctx = ToolContext::new(&registry, &root, &approver, 8, &matrix);
    super::apply_phenotype_scopes(&mut local_ctx, Some(&pheno_local));

    let store_local = SessionStore::new();
    let session_local = store_local.create_session(None);
    store_local.add_message(&session_local.id, Role::User, "list tools".into());

    let seen_local = Arc::new(Mutex::new(None));
    let local_provider = RequestCapture {
        seen: seen_local.clone(),
    };

    let _ = run_turn(
        &local_provider,
        &store_local,
        &local_ctx,
        &session_local.id,
        "mock",
        None,
        false,
        ReasoningVisibility::WrapUp,
        CancelToken::new(),
        |_| {},
    )
    .await;

    let req_local = seen_local
        .lock()
        .unwrap()
        .take()
        .expect("a turn reached the provider under LocalOnly");
    let local_names: Vec<&str> = req_local
        .tools
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(
        !local_names.contains(&"mcp__echo__echo"),
        "LocalOnly must strip mcp__echo__echo; got: {local_names:?}"
    );
    assert!(
        local_names.contains(&"local_tool"),
        "LocalOnly must keep local_tool; got: {local_names:?}"
    );

    // --- Wire-level: drive run_turn with Open egress (no phenotype) ---
    let mut open_ctx = ToolContext::new(&registry, &root, &approver, 8, &matrix);
    super::apply_phenotype_scopes(&mut open_ctx, None);

    let store_open = SessionStore::new();
    let session_open = store_open.create_session(None);
    store_open.add_message(&session_open.id, Role::User, "list tools".into());

    let seen_open = Arc::new(Mutex::new(None));
    let open_provider = RequestCapture {
        seen: seen_open.clone(),
    };

    let _ = run_turn(
        &open_provider,
        &store_open,
        &open_ctx,
        &session_open.id,
        "mock",
        None,
        false,
        ReasoningVisibility::WrapUp,
        CancelToken::new(),
        |_| {},
    )
    .await;

    let req_open = seen_open
        .lock()
        .unwrap()
        .take()
        .expect("a turn reached the provider under Open");
    let open_names: Vec<&str> = req_open
        .tools
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(
        open_names.contains(&"mcp__echo__echo"),
        "Open egress must advertise mcp__echo__echo; got: {open_names:?}"
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

/// `--pheno`/`--model`/`--skill` parse on `chat`, matching `run`'s surface. Before
/// #1208 `chat` had no such flags at all, so a phenotype could not be selected
/// interactively — the ticket's first acceptance criterion is a clap-surface change,
/// not just plumbing.
#[test]
fn chat_accepts_pheno_model_and_skill_flags() {
    let cli = Cli::try_parse_from([
        "flowforge",
        "chat",
        "--pheno",
        "enclave",
        "--model",
        "m1",
        "--skill",
        "a",
        "--skill",
        "b",
    ])
    .expect("parses");
    match cli.command.expect("chat present") {
        super::Command::Chat {
            pheno,
            model,
            skill,
            ..
        } => {
            assert_eq!(pheno.as_deref(), Some("enclave"));
            assert_eq!(model.as_deref(), Some("m1"));
            assert_eq!(skill, vec!["a".to_string(), "b".to_string()]);
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
            &mock_turn(),
            &skills,
            &store,
            &memory_store,
            None,
            &tool_ctx,
            &session_id,
            false,
            ff_core::Mode::Auto,
            &[],
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
        &mock_turn(),
        &skills,
        &store,
        &memory_store,
        None,
        &tool_ctx,
        &session_id, // resume the same session
        false,
        ff_core::Mode::Auto,
        &[],
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
