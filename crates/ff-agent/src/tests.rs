use super::*;
use async_trait::async_trait;
use ff_llm::{Chunk, ChunkStream, LlmError, ToolCallDelta};
use std::sync::atomic::{AtomicBool, AtomicUsize};

#[test]
fn cancel_token_ptr_eq_distinguishes_clone_from_fresh() {
    let token = CancelToken::new();
    let clone = token.clone();
    assert!(token.ptr_eq(&clone), "a clone shares the underlying flag");
    assert!(clone.ptr_eq(&token), "ptr_eq is symmetric");
    let other = CancelToken::new();
    assert!(
        !token.ptr_eq(&other),
        "two independently-created tokens are distinct"
    );
}

#[test]
fn marker_keys_extracts_one_many_zero_and_skips_unterminated() {
    // #469: re-homing depends on pulling every retrieve key out of a sub-agent
    // summary. Single, multiple-in-order, none, empty-key, and a marker missing
    // its closing `]` must all behave.
    assert_eq!(
        marker_keys("see report\n[compacted; retrieve key=4f441c46bdb87160]"),
        vec!["4f441c46bdb87160"]
    );
    assert_eq!(
        marker_keys("a [compacted; retrieve key=aaa] mid b [compacted; retrieve key=bbb] end"),
        vec!["aaa", "bbb"]
    );
    assert!(marker_keys("a plain summary with no markers").is_empty());
    assert!(marker_keys("[compacted; retrieve key=]").is_empty());
    assert!(marker_keys("dangling [compacted; retrieve key=ccc no bracket").is_empty());
}

#[test]
fn to_chat_carries_reasoning_from_persisted_message() {
    // #375 PR-2: ff-agent must lift Message.reasoning into ChatMessage.reasoning
    // so the OpenAI-compatible provider can re-inject it under the gateway's
    // field name on the next tool-call turn.
    let msg = ff_core::Message {
        id: "m1".into(),
        session_id: "s1".into(),
        role: Role::Assistant,
        content: String::new(),
        tool_calls: Some(vec![ff_core::ToolCall {
            id: "call_1".into(),
            name: "search".into(),
            arguments: "{}".into(),
        }]),
        tool_call_id: None,
        attachments: None,
        reasoning: Some("because A then B".into()),
        stop_reason: None,
        author_name: None,
        created_at: 0,
    };
    let out = to_chat(std::slice::from_ref(&msg));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].reasoning.as_deref(), Some("because A then B"));
}

#[test]
fn to_chat_caps_reasoning_replay_to_last_n_tool_turns() {
    // C1: a transcript with more than REASONING_REPLAY_KEEP reasoning-bearing
    // tool-call turns keeps reasoning only on the most-recent `keep` of them;
    // older CoT is dropped from the wire (the store keeps it verbatim).
    let tool_turn = |id: &str, cot: &str| ff_core::Message {
        id: id.into(),
        session_id: "s1".into(),
        role: Role::Assistant,
        content: String::new(),
        tool_calls: Some(vec![ff_core::ToolCall {
            id: format!("call_{id}"),
            name: "search".into(),
            arguments: "{}".into(),
        }]),
        tool_call_id: None,
        attachments: None,
        reasoning: Some(cot.into()),
        stop_reason: None,
        author_name: None,
        created_at: 0,
    };
    let history = vec![
        tool_turn("m1", "oldest"),
        tool_turn("m2", "middle"),
        tool_turn("m3", "newest"),
    ];
    assert_eq!(REASONING_REPLAY_KEEP, 2);
    let out = to_chat(&history);
    assert_eq!(out[0].reasoning, None, "oldest CoT dropped from wire");
    assert_eq!(out[1].reasoning.as_deref(), Some("middle"));
    assert_eq!(out[2].reasoning.as_deref(), Some("newest"));
}

#[test]
fn should_reason_wrapup_only_on_planning_and_wrapup_steps() {
    use ReasoningVisibility::{All, WrapUp};
    // WrapUp (#449): reason on the first iteration and the wrap-up step; skip mid-loop.
    let max_iter = 25usize;
    assert!(should_reason(0, max_iter, WrapUp)); // planning
    assert!(!should_reason(1, max_iter - 1, WrapUp)); // mid-loop
    assert!(!should_reason(10, max_iter - 10, WrapUp));
    assert!(should_reason(max_iter - 1, WRAP_UP_AT_REMAINING, WrapUp)); // wrap-up
                                                                        // All (#549): every step reasons, including the natural mid/final ones.
    assert!(should_reason(0, max_iter, All));
    assert!(should_reason(1, max_iter - 1, All));
    assert!(should_reason(10, max_iter - 10, All));
    assert!(should_reason(max_iter - 1, WRAP_UP_AT_REMAINING, All));
}

#[test]
fn plan_mode_advertises_read_capable_and_sensitive_tools() {
    // #793: Plan advertises tools with a read-only *floor* (bash, github — their
    // list/read calls are ReadOnly) plus tools whose ceiling the Plan matrix row
    // does not Deny. The default Plan row has Sensitive = Ask, so the read-shaped
    // network tools (web_fetch) and the read-inheriting sub-agent are surfaced
    // behind an approval prompt; the per-call safety gate rejects any mutating
    // invocation of the visible tools.
    let reg = ToolRegistry::with_defaults();
    let matrix = PermissionMatrix::default();
    let advertised =
        advertised_tools(Mode::Plan, Egress::Open, &matrix, None, &reg).expect("Plan restricts");
    for name in [
        "view",
        "grep",
        "glob",
        "tree",
        "todo",
        "ask_user",
        "diagnostics",
        "bash",      // read-only floor (`bash ls`)
        "github",    // read-only floor (`pr_list`)
        "web_fetch", // Sensitive ceiling, Plan x Sensitive = Ask
        "agent",     // Sensitive; child inherits Plan (read-only)
    ] {
        assert!(advertised.contains(name), "Plan should advertise {name}");
    }
    // Pure Write/Dangerous tools with no read floor stay hidden.
    for name in ["python", "edit", "write", "apply_patch"] {
        assert!(!advertised.contains(name), "Plan must hide {name}");
    }
}

#[test]
fn plan_mode_hides_sensitive_tools_when_the_matrix_denies_sensitive() {
    // The matrix is the switch (#793): denying Plan x Sensitive drops the network
    // tools + sub-agent back out of the Plan schema, while the read-floor tools
    // (bash, github) remain (they are advertised via their ReadOnly floor).
    use ff_core::{PermissionCell, Safety};
    let reg = ToolRegistry::with_defaults();
    let mut matrix = PermissionMatrix::default();
    matrix.set_cell(Mode::Plan, Safety::Sensitive, PermissionCell::Deny);
    let advertised =
        advertised_tools(Mode::Plan, Egress::Open, &matrix, None, &reg).expect("Plan restricts");
    assert!(advertised.contains("bash"));
    assert!(advertised.contains("github"));
    for name in ["web_fetch", "agent"] {
        assert!(
            !advertised.contains(name),
            "denying Sensitive must hide {name}"
        );
    }
}

#[test]
fn plan_mode_intersects_with_subagent_allowlist() {
    let reg = ToolRegistry::with_defaults();
    let matrix = PermissionMatrix::default();
    // A sub-agent scoped to {view, edit}: Plan further drops the mutating `edit`.
    let allowed: std::collections::HashSet<String> =
        ["view", "edit"].iter().map(|s| s.to_string()).collect();
    let advertised =
        advertised_tools(Mode::Plan, Egress::Open, &matrix, Some(&allowed), &reg).unwrap();
    assert_eq!(advertised, ["view".to_string()].into_iter().collect());
}

#[test]
fn act_and_auto_pass_the_allowlist_through_unchanged() {
    let reg = ToolRegistry::with_defaults();
    let matrix = PermissionMatrix::default();
    assert_eq!(
        advertised_tools(Mode::Act, Egress::Open, &matrix, None, &reg),
        None
    );
    assert_eq!(
        advertised_tools(Mode::Auto, Egress::Open, &matrix, None, &reg),
        None
    );
    let allowed: std::collections::HashSet<String> =
        ["view", "edit"].iter().map(|s| s.to_string()).collect();
    assert_eq!(
        advertised_tools(Mode::Auto, Egress::Open, &matrix, Some(&allowed), &reg),
        Some(allowed)
    );
}

#[test]
fn local_only_egress_strips_network_tools_in_act() {
    // RFC 0013: under LocalOnly, Act/Auto (which would advertise all tools) is
    // reduced to the local-only set — network tools are stripped.
    let reg = ToolRegistry::with_defaults();
    let matrix = PermissionMatrix::default();
    let advertised = advertised_tools(Mode::Act, Egress::LocalOnly, &matrix, None, &reg)
        .expect("LocalOnly restricts even in Act");
    for name in ["view", "edit", "grep", "diagnostics", "agent"] {
        assert!(
            advertised.contains(name),
            "LocalOnly should keep local {name}"
        );
    }
    for name in ["bash", "python", "web_fetch", "web_search", "github"] {
        assert!(
            !advertised.contains(name),
            "LocalOnly must strip network tool {name}"
        );
    }
}

#[test]
fn local_only_composes_with_plan_mode() {
    // enclave + Plan = local AND read-capable. `edit` (local but not read-capable)
    // is dropped by the Plan pass; `web_fetch` (read-shaped but network) by egress.
    let reg = ToolRegistry::with_defaults();
    let matrix = PermissionMatrix::default();
    let advertised = advertised_tools(Mode::Plan, Egress::LocalOnly, &matrix, None, &reg).unwrap();
    assert!(advertised.contains("view"));
    assert!(advertised.contains("grep"));
    assert!(!advertised.contains("edit"), "Plan drops the mutating edit");
    assert!(
        !advertised.contains("web_fetch"),
        "egress drops the network tool"
    );
    assert!(
        !advertised.contains("bash"),
        "egress drops bash even w/ read floor"
    );
}

#[test]
fn local_only_composes_with_subagent_allowlist() {
    // allowlist ∩ local: a sub-agent scoped to {view, web_fetch} keeps only view.
    let reg = ToolRegistry::with_defaults();
    let matrix = PermissionMatrix::default();
    let allowed: std::collections::HashSet<String> = ["view", "web_fetch"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let advertised =
        advertised_tools(Mode::Auto, Egress::LocalOnly, &matrix, Some(&allowed), &reg).unwrap();
    assert_eq!(advertised, ["view".to_string()].into_iter().collect());
}

#[test]
fn open_egress_is_byte_identical_to_pre_rfc() {
    // Regression guard: Open must not change today's behaviour.
    let reg = ToolRegistry::with_defaults();
    let matrix = PermissionMatrix::default();
    assert_eq!(
        advertised_tools(Mode::Act, Egress::Open, &matrix, None, &reg),
        None
    );
}

/// Records whether its approval gate was ever consulted. A Plan-mode hard block
/// must reject before this is reached (#264 review).
struct RecordingApprover {
    consulted: Arc<AtomicBool>,
}
#[async_trait]
impl Approver for RecordingApprover {
    async fn approve(
        &self,
        _message_id: &str,
        _call_id: &str,
        _name: &str,
        _safety: Safety,
        _args: &serde_json::Value,
    ) -> bool {
        self.consulted.store(true, Ordering::SeqCst);
        true
    }
}

/// Names `python` on the first turn (a tool with no read-only floor, so it stays
/// hidden in Plan even after #793), then finishes with text.
struct HiddenToolThenText {
    calls: AtomicUsize,
}
#[async_trait]
impl Provider for HiddenToolThenText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_1".into()),
                    name: Some("python".into()),
                    arguments: r#"{"code":"print(1)"}"#.into(),
                }],
                done: true,
                ..Chunk::default()
            })]
        } else {
            vec![Ok(Chunk {
                delta: "done".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

#[tokio::test]
async fn plan_mode_hard_blocks_dispatch_of_a_hidden_tool() {
    // A Plan-mode model that names a hidden mutating tool (`python`) -- e.g. via
    // prompt injection -- must be hard-blocked at dispatch, *before* the approval
    // gate, not merely hidden from the schema (#264 review blocker). `python` has no
    // read-only floor, so it stays hidden in Plan (unlike bash/github after #793).
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "do it".into());
    let registry = ToolRegistry::with_defaults();
    let root = dir.path().to_path_buf();
    let consulted = Arc::new(AtomicBool::new(false));
    let approve = RecordingApprover {
        consulted: consulted.clone(),
    };
    let provider = HiddenToolThenText {
        calls: AtomicUsize::new(0),
    };

    let matrix = PermissionMatrix::default();
    let plan = ToolContext {
        registry: &registry,
        root: &root,
        approve: &approve,
        max_iterations: 8,
        depth: 0,
        max_depth: 1,
        allowed: None,
        mode: Mode::Plan,
        egress: Egress::default(),
        matrix: &matrix,
        abstractive: AbstractiveConfig::default(),
        compaction_model: None,
        compaction_budget: None,
        compaction_cache: None,
    };

    run_turn(
        &provider,
        &store,
        &plan,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    // The approver was never consulted -- the block is structural, independent of
    // model or approver behaviour.
    assert!(
        !consulted.load(Ordering::SeqCst),
        "Plan-mode dispatch must hard-block before the approval gate"
    );

    // The tool never ran; the model gets a clear, actionable Plan-mode error.
    let history = store.get_messages(&s.id);
    let tool_result = history
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("the blocked call still produces a tool result");
    assert!(
        tool_result.content.contains("not available in Plan mode"),
        "{}",
        tool_result.content
    );
    assert!(
        !tool_result.content.contains("wired"),
        "bash must not have executed: {}",
        tool_result.content
    );
}

struct AlwaysApprove;
#[async_trait]
impl Approver for AlwaysApprove {
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

struct AlwaysDeny;
#[async_trait]
impl Approver for AlwaysDeny {
    async fn approve(
        &self,
        _message_id: &str,
        _call_id: &str,
        _name: &str,
        _safety: Safety,
        _args: &serde_json::Value,
    ) -> bool {
        false
    }
}

/// A `Safety::ReadOnly` tool that sleeps before returning, so two concurrent
/// invocations finish in ~one sleep's wall-clock rather than two -- letting the
/// #A1 parallel-execution test prove concurrency by timing.
struct SlowRead;
#[async_trait]
impl ff_tools::Tool for SlowRead {
    fn name(&self) -> &str {
        "slow_read"
    }
    fn description(&self) -> &str {
        "test-only read tool that sleeps 150ms"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"k":{"type":"string"}}})
    }
    fn safety(&self, _args: &serde_json::Value) -> Safety {
        Safety::ReadOnly
    }
    fn max_safety(&self) -> Safety {
        Safety::ReadOnly
    }
    async fn run(&self, _args: serde_json::Value, _root: &Path) -> ff_tools::ToolOutcome {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        ff_tools::ToolOutcome::ok("read done")
    }
}

/// Approves, but cancels the turn first — to exercise the cancel-mid-loop path.
struct CancelOnApprove(CancelToken);
#[async_trait]
impl Approver for CancelOnApprove {
    async fn approve(
        &self,
        _message_id: &str,
        _call_id: &str,
        _name: &str,
        _safety: Safety,
        _args: &serde_json::Value,
    ) -> bool {
        self.0.cancel();
        true
    }
}

/// Yields once before approving, proving the loop actually awaits the decision.
struct YieldThenApprove;
#[async_trait]
impl Approver for YieldThenApprove {
    async fn approve(
        &self,
        _message_id: &str,
        _call_id: &str,
        _name: &str,
        _safety: Safety,
        _args: &serde_json::Value,
    ) -> bool {
        tokio::task::yield_now().await;
        true
    }
}

static TEST_MATRIX: std::sync::LazyLock<PermissionMatrix> =
    std::sync::LazyLock::new(PermissionMatrix::default);

fn ctx<'a>(
    registry: &'a ToolRegistry,
    root: &'a Path,
    approve: &'a dyn Approver,
) -> ToolContext<'a> {
    ToolContext::new(registry, root, approve, 8, &TEST_MATRIX)
}

/// Counts how many times the provider was hit *as the abstractive summarizer*
/// (2-message requests with no tools). Used to distinguish a run_turn that
/// reuses the cross-turn summary cache from one that re-summarizes first. The
/// flush + main-turn calls are intentionally ignored — they always fire and
/// are unrelated to the cache contract under test.
struct CountingProvider {
    summarizer_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for CountingProvider {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        // The Tier-2 summarizer sends a 2-message request (system prompt + the
        // cold block) with no tools. Filter for that shape, ignore everything
        // else (flush + main turn).
        if req.tools.is_empty() && req.messages.len() == 2 {
            self.summarizer_calls.fetch_add(1, Ordering::SeqCst);
        }
        Ok(futures_util::stream::iter(vec![Ok(Chunk {
            delta: "ok".into(),
            done: true,
            ..Chunk::default()
        })])
        .boxed())
    }
}

#[tokio::test]
async fn cross_turn_cache_seeds_summary_and_invalidate_forces_resummary() {
    // #764: the unit tests cover the map, but the wiring — that `run_turn`
    // actually reads from and writes to the cache — was only covered by the
    // desktop integration. Lock the contract at the agent layer.
    //
    // Phase 1 (cache primed): the seeded `last_summary` matches the wire's
    // post-Tier-1 length, so Tier 2 reuses it instead of re-summarizing — one
    // `chat_stream` call (main turn only).
    //
    // Phase 2 (after `invalidate`): the cache miss forces Tier 2 to call the
    // summarizer first, then the main turn runs — two `chat_stream` calls.
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);

    // Long enough that the post-Tier-1 wire stays over the Tier-2 fraction,
    // so the Tier-2 path is actually entered in both phases.
    for i in 0..30 {
        let line = format!("cold-{i} {}", "lorem ipsum dolor sit amet ".repeat(300));
        let role = if i % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        store.add_message(&s.id, role, line);
    }
    let recents = ["r0", "r1", "r2", "r3", "r4", "r5"];
    for (i, r) in recents.iter().enumerate() {
        let role = if i % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        store.add_message(&s.id, role, (*r).to_string());
    }
    let history = store.get_messages(&s.id);
    let wire_t1 =
        ExtractiveCompactor::default().compact_cold_collect(&history, KEEP_RECENT_VERBATIM);
    let pressure = ProxyTokenEstimator::default().assess(&wire_t1.messages, "mock");
    assert!(
        pressure.is_over(0.90),
        "post-Tier-1 transcript must exceed the Tier-2 fraction: fraction={}",
        pressure.fraction()
    );
    // `cold_end` is the wire index the summary would cover (everything before
    // the kept-recent tail). Required so the seeded summary's boundary matches
    // the actual cold-tail computation in `run_turn`.
    let cold_end = wire_t1
        .messages
        .len()
        .saturating_sub(KEEP_RECENT_VERBATIM + 1);

    let registry = ToolRegistry::new();
    let root = dir.path().to_path_buf();
    let approve = AlwaysApprove;
    let summarizer_calls = Arc::new(AtomicUsize::new(0));
    let provider = CountingProvider {
        summarizer_calls: summarizer_calls.clone(),
    };

    let cache = CompactionCache::new();
    let mut tctx = ctx(&registry, &root, &approve);
    tctx.abstractive = AbstractiveConfig {
        enabled: true,
        fire_at_fraction: 0.90,
        ..AbstractiveConfig::default()
    };
    tctx.compaction_cache = Some(&cache);

    // Phase 1: prime the cache so the seeded summary's boundary matches
    // `cold_end`. The summarizer's `summary_due` allows reuse here because the
    // transcript length has not grown since the cache was written.
    let seeded = Message {
        id: "seed".into(),
        session_id: s.id.clone(),
        role: Role::User,
        content: "seeded cold-prefix summary".into(),
        tool_calls: None,
        tool_call_id: None,
        attachments: None,
        reasoning: None,
        stop_reason: None,
        author_name: None,
        created_at: 0,
    };
    cache.put(&s.id, cold_end, seeded, history.len() as u64);

    run_turn(
        &provider,
        &store,
        &tctx,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(
        summarizer_calls.load(Ordering::SeqCst),
        0,
        "cache-primed run must skip the summarizer"
    );

    // Phase 2: invalidate and re-run. The Tier-2 path now has no `last_summary`
    // to reuse, so it must re-summarize, costing a summarizer call.
    cache.invalidate(&s.id);
    let before = summarizer_calls.load(Ordering::SeqCst);

    // #971: capture the Done event to assert the re-summarize is timed as tier2_ms.
    let tier2_ms_seen = std::sync::Mutex::new(None);
    run_turn(
        &provider,
        &store,
        &tctx,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if let AgentEvent::Done { tier2_ms, .. } = ev {
                *tier2_ms_seen.lock().unwrap() = Some(tier2_ms);
            }
        },
    )
    .await
    .unwrap();
    assert_eq!(
        summarizer_calls.load(Ordering::SeqCst) - before,
        1,
        "post-invalidate run must call the summarizer once"
    );
    assert!(
        cache.get(&s.id).is_some(),
        "the fresh summary is written through to the cache"
    );
    // #971: a turn that actually re-summarized must report tier2_ms.
    assert!(
        matches!(*tier2_ms_seen.lock().unwrap(), Some(Some(_))),
        "a re-summarize turn must populate tier2_ms"
    );

    // Mirror case: a `ToolContext` with no `compaction_cache` always
    // re-summarizes, so the seeded entry's presence/absence stays irrelevant.
    // Locks the None-branch of the seeding logic.
    cache.invalidate(&s.id);
    let before = summarizer_calls.load(Ordering::SeqCst);
    let mut no_cache_tctx = ctx(&registry, &root, &approve);
    no_cache_tctx.abstractive = tctx.abstractive.clone();
    no_cache_tctx.compaction_cache = None;
    run_turn(
        &provider,
        &store,
        &no_cache_tctx,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(
        summarizer_calls.load(Ordering::SeqCst) - before,
        1,
        "no-cache run must always re-summarize regardless of any stale entry"
    );
}

#[tokio::test]
async fn cross_turn_cache_invalidate_all_forces_resummary() {
    // #764 mirror case: provider/model change wipes every session's summary.
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);

    for i in 0..30 {
        let line = format!("cold-{i} {}", "lorem ipsum dolor sit amet ".repeat(300));
        let role = if i % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        store.add_message(&s.id, role, line);
    }
    for (i, r) in ["r0", "r1", "r2", "r3", "r4", "r5"].iter().enumerate() {
        let role = if i % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        store.add_message(&s.id, role, (*r).to_string());
    }
    let history = store.get_messages(&s.id);
    let wire_t1 =
        ExtractiveCompactor::default().compact_cold_collect(&history, KEEP_RECENT_VERBATIM);
    let cold_end = wire_t1
        .messages
        .len()
        .saturating_sub(KEEP_RECENT_VERBATIM + 1);

    let registry = ToolRegistry::new();
    let root = dir.path().to_path_buf();
    let approve = AlwaysApprove;
    let summarizer_calls = Arc::new(AtomicUsize::new(0));

    let cache = CompactionCache::new();
    let seeded = Message {
        id: "seed".into(),
        session_id: s.id.clone(),
        role: Role::User,
        content: "seeded cold-prefix summary".into(),
        tool_calls: None,
        tool_call_id: None,
        attachments: None,
        reasoning: None,
        stop_reason: None,
        author_name: None,
        created_at: 0,
    };
    cache.put(&s.id, cold_end, seeded, history.len() as u64);

    let mut tctx = ctx(&registry, &root, &approve);
    tctx.abstractive = AbstractiveConfig {
        enabled: true,
        fire_at_fraction: 0.90,
        ..AbstractiveConfig::default()
    };
    tctx.compaction_cache = Some(&cache);

    // Confirm the cache path is taken — no summarizer call.
    let provider = CountingProvider {
        summarizer_calls: summarizer_calls.clone(),
    };
    run_turn(
        &provider,
        &store,
        &tctx,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(
        summarizer_calls.load(Ordering::SeqCst),
        0,
        "primed run reuses the cache"
    );

    // Wipe everything (`upsert_connection` / provider change path).
    cache.invalidate_all();
    assert!(cache.get(&s.id).is_none());

    let before = summarizer_calls.load(Ordering::SeqCst);
    run_turn(
        &provider,
        &store,
        &tctx,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(
        summarizer_calls.load(Ordering::SeqCst) - before,
        1,
        "post-invalidate_all run must re-summarize"
    );
}

/// Delays before its first (and only) chunk, so a `run_turn` test can assert a
/// measurable, non-zero round-0 prompt latency (#960). The delay lands *inside*
/// the returned stream (before the first delta), which is exactly what
/// `prompt_latency_ms` measures — stream-return to first output-carrying chunk.
struct DelayedFirstToken {
    delay_ms: u64,
}

#[async_trait]
impl Provider for DelayedFirstToken {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let delay = std::time::Duration::from_millis(self.delay_ms);
        let fut = async move {
            tokio::time::sleep(delay).await;
            Ok(Chunk {
                delta: "hi".into(),
                done: true,
                ..Chunk::default()
            })
        };
        Ok(futures_util::stream::once(fut).boxed())
    }
}

struct TextProvider;

#[async_trait]
impl Provider for TextProvider {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let chunks = vec![
            Ok(Chunk {
                delta: "Hel".into(),
                ..Chunk::default()
            }),
            Ok(Chunk {
                delta: "lo".into(),
                done: true,
                ..Chunk::default()
            }),
        ];
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

/// Emits a reasoning stream then a text answer, to verify run_turn persists
/// the accumulated CoT onto the assistant message (#375 PR-1).
struct ReasoningThenText;

#[async_trait]
impl Provider for ReasoningThenText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let chunks = vec![
            Ok(Chunk {
                reasoning_delta: "let me ".into(),
                ..Chunk::default()
            }),
            Ok(Chunk {
                reasoning_delta: "think".into(),
                ..Chunk::default()
            }),
            Ok(Chunk {
                delta: "42".into(),
                done: true,
                ..Chunk::default()
            }),
        ];
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

/// First call requests a `bash` tool call; second call returns plain text.
struct ToolThenText {
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for ToolThenText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_1".into()),
                    name: Some("bash".into()),
                    arguments: r#"{"command":"echo wired"}"#.into(),
                }],
                done: true,
                ..Chunk::default()
            })]
        } else {
            vec![Ok(Chunk {
                delta: "done: wired".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

/// Like [`ToolThenText`] but the bash command is Write-classified (#680): `printf`
/// is not on the read-only allowlist, so the call runs on the serial pass where
/// live-output streaming is wired. Used to prove chunks stream before the finish.
struct StreamingToolThenText {
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for StreamingToolThenText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_1".into()),
                    name: Some("bash".into()),
                    arguments: r#"{"command":"printf 'wired\\n'"}"#.into(),
                }],
                done: true,
                ..Chunk::default()
            })]
        } else {
            vec![Ok(Chunk {
                delta: "done".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

/// First call streams a tool call the way SiliconFlow GLM-5.2 does (#374): the
/// name arrives only in the first fragment, then every continuation fragment
/// carries `name: Some("")` (an empty string, not `None`) alongside the argument
/// pieces. A blind overwrite would clobber the name to "" -> `unknown tool:`.
struct GlmFragmentedToolCall {
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for GlmFragmentedToolCall {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![
                Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("call_1".into()),
                        name: Some("bash".into()),
                        arguments: String::new(),
                    }],
                    ..Chunk::default()
                }),
                Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: None,
                        name: Some(String::new()),
                        arguments: r#"{"command":"#.into(),
                    }],
                    ..Chunk::default()
                }),
                Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: None,
                        name: Some(String::new()),
                        arguments: r#""echo wired"}"#.into(),
                    }],
                    done: true,
                    ..Chunk::default()
                }),
            ]
        } else {
            vec![Ok(Chunk {
                delta: "done: wired".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

/// First call streams a tool call whose name never arrives (the fragment carries
/// `name: None`), the way a model with no real OpenAI-compatible tool-calling
/// would (#374); the second call returns plain text so the turn can resume after
/// the actionable error result. Must fail with that message, not `unknown tool:`.
struct NamelessToolCall {
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for NamelessToolCall {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_x".into()),
                    name: None,
                    arguments: r#"{"command":"ls"}"#.into(),
                }],
                done: true,
                ..Chunk::default()
            })]
        } else {
            vec![Ok(Chunk {
                delta: "ok, switching approach".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

/// First call streams a tool call the way SiliconFlow does (#512): the delta
/// never carries an `id` (every fragment has `id: None`), so the accumulated
/// buffer id stays empty. The capture site must mint a stable id so the
/// persisted assistant tool_call and its tool result are not bound to "".
struct IdlessToolCall {
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for IdlessToolCall {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: None,
                    name: Some("bash".into()),
                    arguments: r#"{"command":"echo wired"}"#.into(),
                }],
                done: true,
                ..Chunk::default()
            })]
        } else {
            vec![Ok(Chunk {
                delta: "done: wired".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

/// First call requests an `ask_user` tool call; second call returns plain text.
struct AskThenText {
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for AskThenText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("ask_1".into()),
                    name: Some("ask_user".into()),
                    arguments: r#"{"question":"Which file?"}"#.into(),
                }],
                done: true,
                ..Chunk::default()
            })]
        } else {
            vec![Ok(Chunk {
                delta: "using main.rs".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

/// First call requests an `agent` (sub-agent) call; the child's call returns a
/// summary; the parent's final call returns plain text. One shared counter drives
/// parent and child turns through the same provider instance.
struct AgentThenText {
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for AgentThenText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = match n {
            0 => vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("agent_1".into()),
                    name: Some("agent".into()),
                    arguments: r#"{"task":"audit the foo module"}"#.into(),
                }],
                done: true,
                ..Chunk::default()
            })],
            1 => vec![Ok(Chunk {
                delta: "child: audit complete, 0 issues".into(),
                done: true,
                ..Chunk::default()
            })],
            _ => vec![Ok(Chunk {
                delta: "parent: delegated and done".into(),
                done: true,
                ..Chunk::default()
            })],
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

/// Answers an interactive `ask`; denies everything that needs approval (it should
/// never be asked to approve an interactive tool).
struct CannedAnswer(&'static str);
#[async_trait]
impl Approver for CannedAnswer {
    async fn approve(
        &self,
        _message_id: &str,
        _call_id: &str,
        _name: &str,
        _safety: Safety,
        _args: &serde_json::Value,
    ) -> bool {
        false
    }
    async fn ask(
        &self,
        _message_id: &str,
        _call_id: &str,
        args: &serde_json::Value,
    ) -> Option<String> {
        // The host receives the tool args and reads the `question` field.
        assert_eq!(args["question"], "Which file?");
        Some(self.0.to_string())
    }
}

/// #562: requests a *secret* `ask_user` (`secret: true`) first, then plain text.
struct AskSecretThenText {
    calls: AtomicUsize,
}
#[async_trait]
impl Provider for AskSecretThenText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("ask_1".into()),
                    name: Some("ask_user".into()),
                    arguments: r#"{"question":"Enter your sudo password:","secret":true}"#.into(),
                }],
                done: true,
                ..Chunk::default()
            })]
        } else {
            vec![Ok(Chunk {
                delta: "done".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

/// Answers any interactive `ask` with a fixed value; denies approvals.
struct CannedSecret(&'static str);
#[async_trait]
impl Approver for CannedSecret {
    async fn approve(
        &self,
        _message_id: &str,
        _call_id: &str,
        _name: &str,
        _safety: Safety,
        _args: &serde_json::Value,
    ) -> bool {
        false
    }
    async fn ask(
        &self,
        _message_id: &str,
        _call_id: &str,
        _args: &serde_json::Value,
    ) -> Option<String> {
        Some(self.0.to_string())
    }
}

#[tokio::test]
async fn streams_and_persists_text_turn() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "hi".into());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;

    let mut tokens = String::new();
    let mut done = false;
    let msg = run_turn(
        &TextProvider,
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| match ev {
            AgentEvent::Token { delta, .. } => tokens.push_str(&delta),
            AgentEvent::Done { .. } => done = true,
            AgentEvent::Error { .. } => panic!("unexpected error"),
            _ => {}
        },
    )
    .await
    .unwrap();

    assert_eq!(tokens, "Hello");
    assert!(done);
    assert_eq!(msg.content, "Hello");
}

/// A text provider that reports no vision support, for the #338 degrade notice.
struct NoVisionText;

#[async_trait]
impl Provider for NoVisionText {
    fn supports_vision(&self) -> bool {
        false
    }

    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        Ok(futures_util::stream::iter(vec![Ok(Chunk {
            delta: "ok".into(),
            done: true,
            ..Chunk::default()
        })])
        .boxed())
    }
}

fn one_image() -> Vec<ff_core::Attachment> {
    vec![ff_core::Attachment {
        kind: ff_core::AttachmentKind::Image,
        media_type: "image/png".into(),
        source: ff_core::AttachmentSource::Inline("aGk=".into()),
        name: None,
        bytes: 2,
    }]
}

#[tokio::test]
async fn no_vision_model_emits_one_attachments_dropped_notice() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message_with_attachments(&s.id, Role::User, "look at this".into(), one_image());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;

    let mut dropped: Vec<u32> = Vec::new();
    run_turn(
        &NoVisionText,
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if let AgentEvent::AttachmentsDropped { count, .. } = ev {
                dropped.push(count);
            }
        },
    )
    .await
    .unwrap();

    assert_eq!(
        dropped,
        vec![1],
        "a non-vision model emits exactly one notice carrying the dropped count"
    );
}

#[tokio::test]
async fn vision_model_does_not_emit_attachments_dropped() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message_with_attachments(&s.id, Role::User, "look at this".into(), one_image());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;

    let mut emitted = false;
    run_turn(
        &TextProvider,
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if matches!(ev, AgentEvent::AttachmentsDropped { .. }) {
                emitted = true;
            }
        },
    )
    .await
    .unwrap();

    assert!(
        !emitted,
        "a vision-capable model keeps attachments, so no drop notice fires"
    );
}

#[tokio::test]
async fn persists_reasoning_onto_assistant_message() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "what is the answer?".into());
    let registry = ToolRegistry::with_defaults();
    let approve = AlwaysApprove;
    let provider = ReasoningThenText;

    let mut reasoning_seen = String::new();
    let msg = run_turn(
        &provider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        true,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if let AgentEvent::Reasoning { delta, .. } = ev {
                reasoning_seen.push_str(&delta);
            }
        },
    )
    .await
    .unwrap();

    // Still streamed to the FE...
    assert_eq!(reasoning_seen, "let me think");
    assert_eq!(msg.content, "42");
    // ...and now also persisted on the message for later round-tripping.
    assert_eq!(msg.reasoning.as_deref(), Some("let me think"));
    let history = store.get_messages(&s.id);
    assert_eq!(
        history.last().unwrap().reasoning.as_deref(),
        Some("let me think")
    );
}

/// Step 0 returns a tool call (no reasoning emitted there); step 1 — the
/// *natural* final-answer step, well before any cap — emits reasoning then
/// text. Models the #549 gap: a turn that finishes naturally must still show
/// and persist a Thought block for its answer.
struct ToolThenReasonedText {
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for ToolThenReasonedText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_1".into()),
                    name: Some("bash".into()),
                    arguments: r#"{"command":"echo hi"}"#.into(),
                }],
                done: true,
                ..Chunk::default()
            })]
        } else {
            vec![
                Ok(Chunk {
                    reasoning_delta: "the output ".into(),
                    ..Chunk::default()
                }),
                Ok(Chunk {
                    reasoning_delta: "says hi".into(),
                    ..Chunk::default()
                }),
                Ok(Chunk {
                    delta: "It printed hi.".into(),
                    done: true,
                    ..Chunk::default()
                }),
            ]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

#[tokio::test]
async fn all_visibility_persists_reasoning_on_natural_final_answer() {
    // #549: with All, the natural synthesis step (step 1, not a cap wrap-up)
    // carries reasoning, and it is persisted on the assistant message.
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "run echo".into());
    let registry = ToolRegistry::with_defaults();
    let approve = AlwaysApprove;
    let provider = ToolThenReasonedText {
        calls: AtomicUsize::new(0),
    };

    let msg = run_turn(
        &provider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        true,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    assert_eq!(msg.content, "It printed hi.");
    assert_eq!(msg.reasoning.as_deref(), Some("the output says hi"));
}

#[tokio::test]
async fn wrapup_visibility_skips_reasoning_on_natural_final_answer() {
    // The contrast: under WrapUp the same step-1 synthesis runs with reasoning
    // OFF (it is neither the planning step nor a cap wrap-up), so nothing is
    // persisted — the #449 latency optimization, now opt-in.
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "run echo".into());
    let registry = ToolRegistry::with_defaults();
    let approve = AlwaysApprove;
    let provider = ToolThenReasonedText {
        calls: AtomicUsize::new(0),
    };

    let msg = run_turn(
        &provider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        true,
        ReasoningVisibility::WrapUp,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    assert_eq!(msg.content, "It printed hi.");
    assert_eq!(msg.reasoning, None);
}

#[tokio::test]
async fn no_reasoning_leaves_column_null() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "hi".into());
    let registry = ToolRegistry::with_defaults();
    let approve = AlwaysApprove;
    // TextProvider emits no reasoning; even with reasoning enabled the column
    // must stay NULL (skip-empty guard).
    let msg = run_turn(
        &TextProvider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        true,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert!(msg.reasoning.is_none());
}

#[tokio::test]
async fn executes_tool_then_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "run echo".into());
    let registry = ToolRegistry::with_defaults();
    let approve = AlwaysApprove;
    let provider = ToolThenText {
        calls: AtomicUsize::new(0),
    };

    let mut started = 0;
    let mut finished_ok = false;
    let mut final_text = String::new();
    let msg = run_turn(
        &provider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| match ev {
            AgentEvent::ToolCallStarted { name, .. } => {
                assert_eq!(name, "bash");
                started += 1;
            }
            AgentEvent::ToolCallFinished {
                success, result, ..
            } => {
                finished_ok = success;
                assert!(result.contains("wired"));
            }
            AgentEvent::Token { delta, .. } => final_text.push_str(&delta),
            AgentEvent::Reasoning { .. } => {}
            AgentEvent::Error { message } => panic!("error: {message}"),
            AgentEvent::Done { .. } => {}
            AgentEvent::MemoryFlushed { .. } => {}
            AgentEvent::AttachmentsDropped { .. } => {}
            AgentEvent::ToolOutputChunk { .. } => {}
            AgentEvent::Reconnecting { .. } => {}
            AgentEvent::ConnectionFailed { message, .. } => {
                panic!("connection failed: {message}")
            }
        },
    )
    .await
    .unwrap();

    assert_eq!(started, 1);
    assert!(finished_ok);
    assert_eq!(final_text, "done: wired");
    assert_eq!(msg.content, "done: wired");

    // History should be: user, assistant(tool_calls), tool(result), assistant(final).
    let history = store.get_messages(&s.id);
    assert_eq!(history.len(), 4);
    assert_eq!(history[1].role, Role::Assistant);
    assert!(history[1].tool_calls.is_some());
    assert_eq!(history[2].role, Role::Tool);
    assert_eq!(history[2].tool_call_id.as_deref(), Some("call_1"));
}

#[tokio::test]
async fn streaming_tool_emits_output_chunks_before_finish() {
    // #680: a bash call streams live output. The loop must forward at least one
    // ToolOutputChunk for the call *before* its ToolCallFinished, and every chunk
    // must carry the same call_id as the finish.
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "run printf".into());
    let registry = ToolRegistry::with_defaults();
    let approve = AlwaysApprove;
    let provider = StreamingToolThenText {
        calls: AtomicUsize::new(0),
    };

    let mut order: Vec<&'static str> = Vec::new();
    let mut chunk_call_id = String::new();
    let mut finish_call_id = String::new();
    run_turn(
        &provider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| match ev {
            AgentEvent::ToolOutputChunk { call_id, .. } => {
                order.push("chunk");
                chunk_call_id = call_id;
            }
            AgentEvent::ToolCallFinished { call_id, .. } => {
                order.push("finish");
                finish_call_id = call_id;
            }
            _ => {}
        },
    )
    .await
    .unwrap();

    let first_chunk = order.iter().position(|e| *e == "chunk");
    let finish = order.iter().position(|e| *e == "finish");
    assert!(first_chunk.is_some(), "at least one output chunk streamed");
    assert!(
        first_chunk < finish,
        "chunks precede the finish event: {order:?}"
    );
    assert_eq!(
        chunk_call_id, finish_call_id,
        "chunk and finish share the call id"
    );
}

#[tokio::test]
async fn idless_tool_call_gets_synthesized_id_matched_to_its_result() {
    // #512: SiliconFlow streams tool calls without an id. The capture site must
    // mint a stable id so the persisted assistant tool_call and its tool result
    // share a non-empty id; an empty id is what the gateway later rejects (400).
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "run echo".into());
    let registry = ToolRegistry::with_defaults();
    let approve = AlwaysApprove;
    let provider = IdlessToolCall {
        calls: AtomicUsize::new(0),
    };
    run_turn(
        &provider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    let history = store.get_messages(&s.id);
    let call_id = history[1].tool_calls.as_ref().unwrap()[0].id.clone();
    assert!(
        !call_id.is_empty(),
        "assistant tool_call id must be synthesized"
    );
    assert_eq!(
        history[2].tool_call_id.as_deref(),
        Some(call_id.as_str()),
        "tool result must bind to the same synthesized id"
    );
}

#[test]
fn repair_binds_persisted_empty_ids_in_fifo_order() {
    // #512 salvage path: a session recorded before the capture-site fix has
    // empty ids on both the assistant tool_call and its tool result. to_chat
    // must mint matching non-empty ids so the replayed turn is accepted.
    let assistant = ff_core::Message {
        id: "m1".into(),
        session_id: "s1".into(),
        role: Role::Assistant,
        content: String::new(),
        tool_calls: Some(vec![ff_core::ToolCall {
            id: String::new(),
            name: "bash".into(),
            arguments: "{}".into(),
        }]),
        tool_call_id: None,
        attachments: None,
        reasoning: None,
        stop_reason: None,
        author_name: None,
        created_at: 0,
    };
    let tool = ff_core::Message {
        id: "m2".into(),
        session_id: "s1".into(),
        role: Role::Tool,
        content: "ok".into(),
        tool_calls: None,
        tool_call_id: Some(String::new()),
        attachments: None,
        reasoning: None,
        stop_reason: None,
        author_name: None,
        created_at: 1,
    };
    let out = to_chat(&[assistant, tool]);
    let call_id = out[0].tool_calls.as_ref().unwrap()[0].id.clone();
    assert!(!call_id.is_empty(), "assistant id must be repaired");
    assert_eq!(
        out[1].tool_call_id.as_deref(),
        Some(call_id.as_str()),
        "tool result must be bound to the repaired id"
    );
}

#[test]
fn repair_binds_multiple_empty_ids_in_one_message_in_fifo_order() {
    // Depth >1: two id-less calls in a single assistant message, then their two
    // results. The minted ids must be distinct and each result must bind to its
    // call in order -- locks the VecDeque FIFO contract beyond the single-call path.
    let assistant = ff_core::Message {
        id: "m1".into(),
        session_id: "s1".into(),
        role: Role::Assistant,
        content: String::new(),
        tool_calls: Some(vec![
            ff_core::ToolCall {
                id: String::new(),
                name: "bash".into(),
                arguments: "{}".into(),
            },
            ff_core::ToolCall {
                id: String::new(),
                name: "view".into(),
                arguments: "{}".into(),
            },
        ]),
        tool_call_id: None,
        attachments: None,
        reasoning: None,
        stop_reason: None,
        author_name: None,
        created_at: 0,
    };
    let result = |mid: &str, ts: i64| ff_core::Message {
        id: mid.into(),
        session_id: "s1".into(),
        role: Role::Tool,
        content: "ok".into(),
        tool_calls: None,
        tool_call_id: Some(String::new()),
        attachments: None,
        reasoning: None,
        stop_reason: None,
        author_name: None,
        created_at: ts,
    };
    let out = to_chat(&[assistant, result("m2", 1), result("m3", 2)]);
    let calls = out[0].tool_calls.as_ref().unwrap();
    let (id0, id1) = (calls[0].id.clone(), calls[1].id.clone());
    assert!(!id0.is_empty() && !id1.is_empty());
    assert_ne!(id0, id1, "minted ids must be distinct");
    assert_eq!(out[1].tool_call_id.as_deref(), Some(id0.as_str()));
    assert_eq!(out[2].tool_call_id.as_deref(), Some(id1.as_str()));
}

#[test]
fn repair_leaves_valid_tool_call_ids_untouched() {
    let assistant = ff_core::Message {
        id: "m1".into(),
        session_id: "s1".into(),
        role: Role::Assistant,
        content: String::new(),
        tool_calls: Some(vec![ff_core::ToolCall {
            id: "call_real".into(),
            name: "bash".into(),
            arguments: "{}".into(),
        }]),
        tool_call_id: None,
        attachments: None,
        reasoning: None,
        stop_reason: None,
        author_name: None,
        created_at: 0,
    };
    let tool = ff_core::Message {
        id: "m2".into(),
        session_id: "s1".into(),
        role: Role::Tool,
        content: "ok".into(),
        tool_calls: None,
        tool_call_id: Some("call_real".into()),
        attachments: None,
        reasoning: None,
        stop_reason: None,
        author_name: None,
        created_at: 1,
    };
    let out = to_chat(&[assistant, tool]);
    assert_eq!(out[0].tool_calls.as_ref().unwrap()[0].id, "call_real");
    assert_eq!(out[1].tool_call_id.as_deref(), Some("call_real"));
}

/// #374: GLM-5.2 streams `name: ""` on every continuation fragment. The
/// accumulator must keep the name from the first fragment and still assemble the
/// arguments, so the call dispatches to `bash` -- not a clobbered `unknown tool:`.
#[tokio::test]
async fn glm_empty_string_name_fragments_do_not_clobber_the_name() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "run echo".into());
    let registry = ToolRegistry::with_defaults();
    let approve = AlwaysApprove;
    let provider = GlmFragmentedToolCall {
        calls: AtomicUsize::new(0),
    };

    let mut started_name = String::new();
    let mut finished_ok = false;
    let mut result = String::new();
    let mut final_text = String::new();
    let msg = run_turn(
        &provider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| match ev {
            AgentEvent::ToolCallStarted { name, .. } => started_name = name,
            AgentEvent::ToolCallFinished {
                success, result: r, ..
            } => {
                finished_ok = success;
                result = r;
            }
            AgentEvent::Token { delta, .. } => final_text.push_str(&delta),
            AgentEvent::Reasoning { .. } => {}
            AgentEvent::Error { message } => panic!("error: {message}"),
            AgentEvent::Done { .. } => {}
            AgentEvent::MemoryFlushed { .. } => {}
            AgentEvent::AttachmentsDropped { .. } => {}
            AgentEvent::ToolOutputChunk { .. } => {}
            AgentEvent::Reconnecting { .. } => {}
            AgentEvent::ConnectionFailed { message, .. } => {
                panic!("connection failed: {message}")
            }
        },
    )
    .await
    .unwrap();

    assert_eq!(
        started_name, "bash",
        "name must survive the empty-string frags"
    );
    assert!(
        finished_ok,
        "the bash call must run, not fail as unknown tool"
    );
    assert!(
        result.contains("wired"),
        "args must assemble across fragments"
    );
    assert_eq!(msg.content, "done: wired");
    assert!(
        !result.contains("unknown tool"),
        "must not regress to the clobbered-name failure"
    );
}

/// #374: a model that never sends a tool name at all must fail with an actionable
/// message, not the cryptic `unknown tool:` from dispatching an empty name.
#[tokio::test]
async fn nameless_tool_call_fails_with_actionable_message() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "do something".into());
    let registry = ToolRegistry::with_defaults();
    let approve = AlwaysApprove;

    let mut finished_ok = true;
    let mut result = String::new();
    let provider = NamelessToolCall {
        calls: AtomicUsize::new(0),
    };
    run_turn(
        &provider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| match ev {
            AgentEvent::ToolCallFinished {
                success, result: r, ..
            } => {
                finished_ok = success;
                result = r;
            }
            AgentEvent::Error { message } => panic!("error: {message}"),
            _ => {}
        },
    )
    .await
    .unwrap();

    assert!(!finished_ok, "a nameless call is a failed tool result");
    assert!(
        result.contains("no name"),
        "must explain the model returned a tool call with no name, got: {result}"
    );
    assert!(
        !result.contains("unknown tool"),
        "must not surface the cryptic unknown-tool error"
    );
}

/// #44: an `ask_user` call routes to `Approver::ask`; the answer becomes the tool
/// result and the turn resumes, with well-formed history.
#[tokio::test]
async fn ask_user_round_trips_answer_as_tool_result() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "edit the file".into());
    let registry = ToolRegistry::with_defaults();
    let approve = CannedAnswer("main.rs");
    let provider = AskThenText {
        calls: AtomicUsize::new(0),
    };

    let mut started_name = String::new();
    let mut result = String::new();
    let mut ok = false;
    let mut final_text = String::new();
    let msg = run_turn(
        &provider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| match ev {
            AgentEvent::ToolCallStarted { name, .. } => started_name = name,
            AgentEvent::ToolCallFinished {
                success, result: r, ..
            } => {
                ok = success;
                result = r;
            }
            AgentEvent::Token { delta, .. } => final_text.push_str(&delta),
            AgentEvent::Reasoning { .. } => {}
            AgentEvent::Error { message } => panic!("error: {message}"),
            AgentEvent::Done { .. } => {}
            AgentEvent::MemoryFlushed { .. } => {}
            AgentEvent::AttachmentsDropped { .. } => {}
            AgentEvent::ToolOutputChunk { .. } => {}
            AgentEvent::Reconnecting { .. } => {}
            AgentEvent::ConnectionFailed { message, .. } => {
                panic!("connection failed: {message}")
            }
        },
    )
    .await
    .unwrap();

    assert_eq!(started_name, "ask_user");
    assert!(ok, "an answered question is a successful tool result");
    assert_eq!(result, "main.rs");
    assert_eq!(final_text, "using main.rs");
    assert_eq!(msg.content, "using main.rs");

    // History: user, assistant(tool_calls), tool(answer), assistant(final).
    let history = store.get_messages(&s.id);
    assert_eq!(history.len(), 4);
    assert_eq!(history[2].role, Role::Tool);
    assert_eq!(history[2].tool_call_id.as_deref(), Some("ask_1"));
    assert_eq!(history[2].content, "main.rs");
}

/// #562: a `secret: true` answer is redacted everywhere downstream — both the
/// emitted `ToolCallFinished` event (which reaches the UI) and the persisted
/// transcript row carry the placeholder, never the cleartext.
#[tokio::test]
async fn secret_ask_redacts_answer_from_both_event_and_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "run the install".into());
    let registry = ToolRegistry::with_defaults();
    let approve = CannedSecret("hunter2");
    let provider = AskSecretThenText {
        calls: AtomicUsize::new(0),
    };

    let mut result = String::new();
    let mut ok = false;
    run_turn(
        &provider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| match ev {
            AgentEvent::ToolCallFinished {
                success, result: r, ..
            } => {
                ok = success;
                result = r;
            }
            AgentEvent::Error { message } => panic!("error: {message}"),
            _ => {}
        },
    )
    .await
    .unwrap();

    // The emitted event (the UI's source) carries the placeholder, not the
    // cleartext — this is the leak vector the FE renders in its OutputBlock.
    assert!(
        ok,
        "an answered secret question is a successful tool result"
    );
    assert_eq!(result, SECRET_ANSWER_PLACEHOLDER);

    // …and the persisted transcript row is likewise the placeholder.
    let history = store.get_messages(&s.id);
    assert_eq!(history[2].role, Role::Tool);
    assert_eq!(history[2].tool_call_id.as_deref(), Some("ask_1"));
    assert_eq!(history[2].content, SECRET_ANSWER_PLACEHOLDER);
    assert!(
        !history.iter().any(|m| m.content.contains("hunter2")),
        "the cleartext secret must not appear anywhere in the transcript"
    );
}

/// #44: a dismissed question (the default `ask` returns `None`) still emits a
/// matching tool result, so history never goes malformed.
#[tokio::test]
async fn dismissed_ask_emits_tool_result_not_hang() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "edit the file".into());
    let registry = ToolRegistry::with_defaults();
    // AlwaysDeny uses the default `ask` (returns None) -> dismissed.
    let approve = AlwaysDeny;
    let provider = AskThenText {
        calls: AtomicUsize::new(0),
    };

    let mut result = String::new();
    let msg = run_turn(
        &provider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if let AgentEvent::ToolCallFinished { result: r, .. } = ev {
                result = r;
            }
        },
    )
    .await
    .unwrap();

    assert!(result.contains("no answer"));
    let history = store.get_messages(&s.id);
    assert_eq!(history[2].role, Role::Tool);
    assert_eq!(history[2].tool_call_id.as_deref(), Some("ask_1"));
    // Turn still completed with the follow-up assistant text.
    assert_eq!(msg.content, "using main.rs");
}

/// Cancelling mid-execution must still leave a matching tool result for every
/// requested call, so the next turn's history stays well-formed.
#[tokio::test]
async fn cancel_mid_loop_backfills_tool_results() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "do two things".into());
    let registry = ToolRegistry::with_defaults();

    struct TwoCalls;
    #[async_trait]
    impl Provider for TwoCalls {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let chunks = vec![Ok(Chunk {
                tool_calls: vec![
                    ToolCallDelta {
                        index: 0,
                        id: Some("call_a".into()),
                        name: Some("bash".into()),
                        arguments: r#"{"command":"touch a"}"#.into(),
                    },
                    ToolCallDelta {
                        index: 1,
                        id: Some("call_b".into()),
                        name: Some("bash".into()),
                        arguments: r#"{"command":"touch b"}"#.into(),
                    },
                ],
                done: true,
                ..Chunk::default()
            })];
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    // Approving the first (write) call cancels the turn, so the second is skipped.
    let cancel = CancelToken::new();
    let approve = CancelOnApprove(cancel.clone());

    let msg = run_turn(
        &TwoCalls,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        cancel,
        |_| {},
    )
    .await
    .unwrap();

    // Every requested tool call must have a matching Role::Tool reply.
    let history = store.get_messages(&s.id);
    let assistant = history
        .iter()
        .find(|m| m.tool_calls.is_some())
        .expect("assistant tool-call message");
    let requested: Vec<&str> = assistant
        .tool_calls
        .as_ref()
        .unwrap()
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    let replied: Vec<String> = history
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter_map(|m| m.tool_call_id.clone())
        .collect();
    for id in &requested {
        assert!(
            replied.iter().any(|r| r == id),
            "missing tool result for {id}"
        );
    }
    // The skipped call is recorded as cancelled.
    assert!(history
        .iter()
        .any(|m| m.role == Role::Tool && m.content == "[cancelled]"));
    // The final bubble is never empty.
    assert!(!msg.content.is_empty());
}

/// #A1: two read-only tool calls in one turn run concurrently (timed), and each
/// requested call id gets exactly one tool result.
#[tokio::test]
async fn parallel_readonly_calls_run_concurrently() {
    struct TwoReadsThenText {
        calls: AtomicUsize,
    }
    #[async_trait]
    impl Provider for TwoReadsThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![
                        ToolCallDelta {
                            index: 0,
                            id: Some("r1".into()),
                            name: Some("slow_read".into()),
                            arguments: r#"{"k":"a"}"#.into(),
                        },
                        ToolCallDelta {
                            index: 1,
                            id: Some("r2".into()),
                            name: Some("slow_read".into()),
                            arguments: r#"{"k":"b"}"#.into(),
                        },
                    ],
                    done: true,
                    ..Chunk::default()
                })]
            } else {
                vec![Ok(Chunk {
                    delta: "done reading".into(),
                    done: true,
                    ..Chunk::default()
                })]
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "read two".into());
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(SlowRead));
    let approve = AlwaysApprove;
    let provider = TwoReadsThenText {
        calls: AtomicUsize::new(0),
    };

    let start = std::time::Instant::now();
    let msg = run_turn(
        &provider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    let elapsed = start.elapsed();

    // Two 150ms reads concurrently ~= 150ms; serial would be ~300ms.
    assert!(
        elapsed < std::time::Duration::from_millis(280),
        "read-only calls must run concurrently, took {elapsed:?}"
    );
    // Exactly one tool result per requested id.
    let history = store.get_messages(&s.id);
    let replied: Vec<String> = history
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter_map(|m| m.tool_call_id.clone())
        .collect();
    assert_eq!(replied.len(), 2, "one result per call: {replied:?}");
    assert!(replied.iter().any(|r| r == "r1"));
    assert!(replied.iter().any(|r| r == "r2"));
    assert_eq!(msg.content, "done reading");
}

/// #863 regression: a ReadOnly tool run in the parallel batch must receive the
/// turn's real `session_id` (via `run_with_session`), not the anonymous
/// `NO_SESSION`. Session-scoped ReadOnly tools (notebook_runner `status`,
/// ProcessManagerTool `poll`/`list`) otherwise query an empty bucket and never
/// see state created by their serial `start`/`run_cell` siblings.
#[tokio::test]
async fn parallel_readonly_call_receives_session_id() {
    use std::sync::Mutex;

    struct SessionSpy {
        seen: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl ff_tools::Tool for SessionSpy {
        fn name(&self) -> &str {
            "session_spy"
        }
        fn description(&self) -> &str {
            "records the session id it is called with"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn safety(&self, _args: &serde_json::Value) -> ff_core::Safety {
            ff_core::Safety::ReadOnly
        }
        async fn run(&self, _args: serde_json::Value, _root: &Path) -> ff_tools::ToolOutcome {
            // The base `run` (NO_SESSION) must NOT be the path taken.
            self.seen.lock().unwrap().push("<no-session>".into());
            ff_tools::ToolOutcome::ok("ran without session")
        }
        async fn run_with_session(
            &self,
            _args: serde_json::Value,
            _root: &Path,
            session_id: &str,
        ) -> ff_tools::ToolOutcome {
            self.seen.lock().unwrap().push(session_id.to_string());
            ff_tools::ToolOutcome::ok("ran with session")
        }
    }

    struct OneReadThenText {
        calls: AtomicUsize,
    }
    #[async_trait]
    impl Provider for OneReadThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("c1".into()),
                        name: Some("session_spy".into()),
                        arguments: "{}".into(),
                    }],
                    done: true,
                    ..Chunk::default()
                })]
            } else {
                vec![Ok(Chunk {
                    delta: "done".into(),
                    done: true,
                    ..Chunk::default()
                })]
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "spy".into());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(SessionSpy { seen: seen.clone() }));
    let approve = AlwaysApprove;
    let provider = OneReadThenText {
        calls: AtomicUsize::new(0),
    };

    run_turn(
        &provider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "spy called once: {seen:?}");
    assert_eq!(
        seen[0], s.id,
        "parallel ReadOnly call must get the real session id, not NO_SESSION"
    );
}

/// #A1: a turn mixing a read-only call and a write call keeps the write on the
/// serial, approval-gated path; the read-only call never reaches the approver.
/// Uses Unix-specific `touch` command.
#[cfg(unix)]
#[tokio::test]
async fn mixed_read_and_write_keeps_write_gated() {
    struct ReadAndWriteThenText {
        calls: AtomicUsize,
    }
    #[async_trait]
    impl Provider for ReadAndWriteThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![
                        ToolCallDelta {
                            index: 0,
                            id: Some("r1".into()),
                            name: Some("slow_read".into()),
                            arguments: r#"{"k":"a"}"#.into(),
                        },
                        ToolCallDelta {
                            index: 1,
                            id: Some("w1".into()),
                            name: Some("bash".into()),
                            arguments: r#"{"command":"touch made_by_write"}"#.into(),
                        },
                    ],
                    done: true,
                    ..Chunk::default()
                })]
            } else {
                vec![Ok(Chunk {
                    delta: "did both".into(),
                    done: true,
                    ..Chunk::default()
                })]
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "read and write".into());
    let mut registry = ToolRegistry::with_defaults();
    registry.register(Box::new(SlowRead));
    let consulted = Arc::new(AtomicBool::new(false));
    let approve = RecordingApprover {
        consulted: consulted.clone(),
    };
    let provider = ReadAndWriteThenText {
        calls: AtomicUsize::new(0),
    };

    run_turn(
        &provider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    // The write went through the approval gate; the read-only call did not need it.
    assert!(
        consulted.load(Ordering::SeqCst),
        "the write call must be approval-gated on the serial path"
    );
    // Both calls produced a tool result.
    let history = store.get_messages(&s.id);
    let replied: Vec<String> = history
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter_map(|m| m.tool_call_id.clone())
        .collect();
    assert!(replied.iter().any(|r| r == "r1"));
    assert!(replied.iter().any(|r| r == "w1"));
    // The approved write actually ran.
    assert!(dir.path().join("made_by_write").exists());
}

/// Dropping the `run_turn` future mid tool-loop (window closed, runtime torn
/// down, or a superseding turn) must NOT leave an assistant `tool_use` without
/// a matching tool result — strict providers reject that on the next turn
/// (#316). The cooperative-cancel backfill (`cancel_mid_loop_backfills_tool_results`)
/// only fires if execution reaches it; a dropped future skips it. The RAII
/// guard closes that gap.
#[tokio::test]
async fn dropped_future_backfills_tool_results() {
    use std::future::Future;
    use std::task::{Context, Poll};

    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "do two things".into());
    let registry = ToolRegistry::with_defaults();

    // Two write (approval-gated) calls; the loop parks on the first call's
    // approval, which is exactly the window between `attach_tool_calls` and the
    // first tool result.
    struct TwoWrites;
    #[async_trait]
    impl Provider for TwoWrites {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let chunks = vec![Ok(Chunk {
                tool_calls: vec![
                    ToolCallDelta {
                        index: 0,
                        id: Some("call_a".into()),
                        name: Some("bash".into()),
                        arguments: r#"{"command":"touch a"}"#.into(),
                    },
                    ToolCallDelta {
                        index: 1,
                        id: Some("call_b".into()),
                        name: Some("bash".into()),
                        arguments: r#"{"command":"touch b"}"#.into(),
                    },
                ],
                done: true,
                ..Chunk::default()
            })];
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    // Never resolves: the turn future parks forever awaiting approval, so we
    // can drop it while a `tool_use` is persisted but un-resulted.
    struct NeverApprove;
    #[async_trait]
    impl Approver for NeverApprove {
        async fn approve(
            &self,
            _message_id: &str,
            _call_id: &str,
            _name: &str,
            _safety: Safety,
            _args: &serde_json::Value,
        ) -> bool {
            std::future::pending::<()>().await;
            unreachable!("pending() never resolves")
        }
    }

    let approve = NeverApprove;
    let tool_ctx = ctx(&registry, dir.path(), &approve);
    let fut = run_turn(
        &TwoWrites,
        &store,
        &tool_ctx,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    );

    // Poll the turn future a bounded number of times so it reaches and parks on
    // the first approval, then drop it — simulating the host abandoning the turn.
    let mut fut = Box::pin(fut);
    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    for _ in 0..256 {
        match fut.as_mut().poll(&mut cx) {
            Poll::Pending => {}
            Poll::Ready(_) => panic!("turn should park on NeverApprove, not complete"),
        }
    }
    drop(fut);

    // Every requested tool call has a matching Role::Tool reply despite the drop.
    let history = store.get_messages(&s.id);
    let assistant = history
        .iter()
        .find(|m| m.tool_calls.is_some())
        .expect("assistant tool-call message persisted before the drop");
    let requested: Vec<&str> = assistant
        .tool_calls
        .as_ref()
        .unwrap()
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    let replied: Vec<String> = history
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter_map(|m| m.tool_call_id.clone())
        .collect();
    for id in &requested {
        assert!(
            replied.iter().any(|r| r == id),
            "dropped turn left tool_use {id} without a result"
        );
    }
}

/// Dropping the `run_turn` future *after* the per-iteration assistant row is
/// reserved but *before* any completion must not leave a silent empty bubble
/// (#646). The row is created empty at the top of the loop so streaming tokens
/// have a home; if the future is abandoned while the provider stream is still
/// pending, the `AssistantRowGuard` backfills an interrupted notice on Drop.
#[tokio::test]
async fn dropped_future_backfills_interrupted_notice_on_empty_row() {
    use std::future::Future;
    use std::task::{Context, Poll};

    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "hello".into());
    let registry = ToolRegistry::with_defaults();

    // A provider whose stream never yields: `run_turn` reserves the assistant
    // row, issues the request, and parks awaiting the first chunk -- exactly the
    // window between row reservation and `set_message_content`.
    struct PendingStream;
    #[async_trait]
    impl Provider for PendingStream {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            Ok(futures_util::stream::pending::<Result<Chunk, LlmError>>().boxed())
        }
    }

    let approve = AlwaysApprove;
    let tool_ctx = ctx(&registry, dir.path(), &approve);
    let fut = run_turn(
        &PendingStream,
        &store,
        &tool_ctx,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    );

    // Poll until the turn parks on the pending stream, then drop it.
    let mut fut = Box::pin(fut);
    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    for _ in 0..256 {
        match fut.as_mut().poll(&mut cx) {
            Poll::Pending => {}
            Poll::Ready(_) => panic!("turn should park on the pending stream, not complete"),
        }
    }
    drop(fut);

    // The reserved assistant row carries an interrupted notice, not empty content.
    let history = store.get_messages(&s.id);
    let assistant = history
        .iter()
        .find(|m| m.role == Role::Assistant)
        .expect("assistant row reserved before the drop");
    assert_eq!(
        assistant.content, INTERRUPTED_NOTICE,
        "dropped turn left a silent empty assistant bubble"
    );
    assert!(
        assistant.tool_calls.is_none(),
        "no tool calls were made, so none should be attached"
    );
    // The structured reason is stamped alongside the notice, so the frontend
    // classifies the row without falling back to the legacy string match.
    assert_eq!(
        assistant.stop_reason,
        Some(StopReason::Interrupted),
        "dropped turn should record a structured Interrupted stop reason"
    );
}

#[tokio::test]
async fn denied_write_tool_reports_failure() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "old\n").unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "edit it".into());
    let registry = ToolRegistry::with_defaults();
    // Deny everything that needs approval.
    let deny = AlwaysDeny;

    struct EditProvider {
        calls: AtomicUsize,
    }
    #[async_trait]
    impl Provider for EditProvider {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("call_e".into()),
                        name: Some("edit".into()),
                        arguments: r#"{"path":"f.txt","old_str":"old","new_str":"new"}"#.into(),
                    }],
                    done: true,
                    ..Chunk::default()
                })]
            } else {
                vec![Ok(Chunk {
                    delta: "ok".into(),
                    done: true,
                    ..Chunk::default()
                })]
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    let mut denied_reported = false;
    run_turn(
        &EditProvider {
            calls: AtomicUsize::new(0),
        },
        &store,
        &ctx(&registry, dir.path(), &deny),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if let AgentEvent::ToolCallFinished {
                success, result, ..
            } = ev
            {
                if !success && result.contains("not approved") {
                    denied_reported = true;
                }
            }
        },
    )
    .await
    .unwrap();

    assert!(denied_reported);
    // The file must be untouched because the edit was denied.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "old\n"
    );
}

/// The loop must await an async approval decision before running the tool.
#[tokio::test]
async fn awaits_async_approval() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "run echo".into());
    let registry = ToolRegistry::with_defaults();
    let approve = YieldThenApprove;
    let provider = ToolThenText {
        calls: AtomicUsize::new(0),
    };

    let mut finished_ok = false;
    run_turn(
        &provider,
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if let AgentEvent::ToolCallFinished { success, .. } = ev {
                finished_ok = success;
            }
        },
    )
    .await
    .unwrap();

    assert!(finished_ok, "tool should run after async approval resolves");
}

/// Captures the `ChatRequest` it receives so a test can assert what reached the
/// provider (the system prompt is transient — never stored in history).
struct RecordingProvider {
    seen: Arc<std::sync::Mutex<Vec<ChatMessage>>>,
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

#[tokio::test]
async fn ingest_compacts_large_tool_result_below_pressure_and_stores_original() {
    // #933 B.2: a big tool-result blob in the cold region is compacted on the
    // wire even with the transcript far under the pressure gate, and its verbatim
    // original is persisted so `compaction_retrieve` can fetch it back.
    let store = SessionStore::new();
    let s = store.create_session(None);
    // A large JSON-ish tool result (the kind codegraph_explore/pr diff produce).
    let big = format!("[{}]", vec!["\"xxxxxxxxxxxxxxxx\""; 400].join(","));
    store.add_message(&s.id, Role::User, "go".into());
    store.add_tool_result_message(&s.id, "call-1".into(), big.clone());
    // Pad the recent tail so the big tool result sits OUTSIDE the
    // KEEP_RECENT_VERBATIM=6 window (which is kept byte-identical).
    for i in 0..8 {
        store.add_message(&s.id, Role::User, format!("follow-up {i}"));
    }

    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = RecordingProvider { seen: seen.clone() };

    run_turn(
        &provider,
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    // The tool result on the wire carries the compaction marker...
    let msgs = seen.lock().unwrap();
    let tool_wire = msgs
        .iter()
        .find(|m| m.role == "tool")
        .expect("tool message reached the provider");
    let wire_content = tool_wire.content.as_deref().unwrap();
    assert!(
        wire_content.contains("[compacted; retrieve key="),
        "large cold tool result must be ingest-compacted on the wire: {wire_content}"
    );
    assert!(
        wire_content.len() < big.len(),
        "compacted wire content must be smaller than the original blob"
    );

    // ...and the verbatim original is retrievable from the store.
    let key = wire_content
        .rsplit("retrieve key=")
        .next()
        .unwrap()
        .trim_end_matches(']')
        .trim();
    assert_eq!(
        store.compaction_original(key).as_deref(),
        Some(big.as_str()),
        "verbatim original persisted for compaction_retrieve"
    );

    // The store itself keeps the tool row fully verbatim (wire-only transform).
    let stored = store.get_messages(&s.id);
    let stored_tool = stored.iter().find(|m| m.role == Role::Tool).unwrap();
    assert_eq!(stored_tool.content, big, "store stays verbatim (Option B)");
}

#[tokio::test]
async fn system_prompt_is_injected_into_request_not_history() {
    use ff_skills::SkillRegistry;

    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("rust-debug");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: rust-debug\ndescription: Systematic Rust debugging\nversion: 0.1.0\n---\nBisect with bash.\n",
    )
        .unwrap();
    let (skills, errs) = SkillRegistry::load_dir(dir.path());
    assert!(errs.is_empty());

    let user = UserContext {
        local_date: "2026-06-13".into(),
        timezone: "America/Chicago".into(),
        time_of_day: TimeOfDay::Morning,
        working_dir: String::new(),
    };
    let system = build_system_prompt(None, &skills, &[], &user, None, None, Mode::default());

    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "hi".into());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = RecordingProvider { seen: seen.clone() };

    run_turn(
        &provider,
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        Some(&system),
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    let msgs = seen.lock().unwrap();
    // Two system messages: stable prefix then volatile tail (#933 A.1).
    assert_eq!(msgs[0].role, "system");
    assert_eq!(msgs[1].role, "system");
    let stable = msgs[0].content.as_deref().unwrap();
    let volatile = msgs[1].content.as_deref().unwrap();
    assert!(
        stable.contains("- rust-debug: Systematic Rust debugging"),
        "{stable}"
    );
    assert!(volatile.contains("Current: 2026-06-13, morning (America/Chicago)."));
    assert_eq!(msgs[2].role, "user");

    // The system prompt must not be persisted: history is just [user, assistant].
    let history = store.get_messages(&s.id);
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].role, Role::User);
    assert_eq!(history[1].role, Role::Assistant);
}

#[tokio::test]
async fn subagent_delegates_and_returns_summary() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "delegate an audit".into());
    let registry = ToolRegistry::with_defaults();
    let root = dir.path().to_path_buf();
    let approve = AlwaysApprove;
    let provider = AgentThenText {
        calls: AtomicUsize::new(0),
    };

    let msg = run_turn(
        &provider,
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    // Parent finished with its own answer, having delegated mid-turn.
    assert_eq!(msg.content, "parent: delegated and done");

    // The child's summary came back as the parent's tool result.
    let history = store.get_messages(&s.id);
    let tool_result = history
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("parent should have a tool result for the agent call");
    assert_eq!(tool_result.content, "child: audit complete, 0 issues");

    // The ephemeral child session was deleted — only the parent remains.
    assert_eq!(store.list_sessions().len(), 1);
}

#[tokio::test]
async fn subagent_depth_guard_refuses_nested_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "try to delegate from a child".into());
    let registry = ToolRegistry::with_defaults();
    let root = dir.path().to_path_buf();
    let approve = AlwaysApprove;
    let provider = AgentThenText {
        calls: AtomicUsize::new(0),
    };

    let matrix = PermissionMatrix::default();
    // Simulate an agent already at the depth cap.
    let at_cap = ToolContext {
        registry: &registry,
        root: &root,
        approve: &approve,
        max_iterations: 8,
        depth: 1,
        max_depth: 1,
        allowed: None,
        mode: Mode::default(),
        egress: Egress::default(),
        matrix: &matrix,
        abstractive: AbstractiveConfig::default(),
        compaction_model: None,
        compaction_budget: None,
        compaction_cache: None,
    };

    run_turn(
        &provider,
        &store,
        &at_cap,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    let history = store.get_messages(&s.id);
    let tool_result = history
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("the refused spawn still produces a tool result");
    assert!(
        tool_result.content.contains("max delegation depth"),
        "{}",
        tool_result.content
    );
    // No child session was ever created.
    assert_eq!(store.list_sessions().len(), 1);
}

#[tokio::test]
async fn subagent_allowlist_blocks_disallowed_tool() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "scoped read-only run".into());
    let registry = ToolRegistry::with_defaults();
    let root = dir.path().to_path_buf();
    let approve = AlwaysApprove;
    let provider = ToolThenText {
        calls: AtomicUsize::new(0),
    };

    let matrix = PermissionMatrix::default();
    // A sub-agent scoped to read-only tools tries to call `bash`.
    let scoped = ToolContext {
        registry: &registry,
        root: &root,
        approve: &approve,
        max_iterations: 8,
        depth: 1,
        max_depth: 1,
        allowed: Some(["view".to_string()].into_iter().collect()),
        mode: Mode::default(),
        egress: Egress::default(),
        matrix: &matrix,
        abstractive: AbstractiveConfig::default(),
        compaction_model: None,
        compaction_budget: None,
        compaction_cache: None,
    };

    run_turn(
        &provider,
        &store,
        &scoped,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    let history = store.get_messages(&s.id);
    let tool_result = history
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("the disallowed call still produces a tool result");
    assert!(
        tool_result.content.contains("not permitted"),
        "{}",
        tool_result.content
    );
}

/// Always requests a tool call (never finishes on its own), and records, per
/// request, whether a wrap-up nudge was present (`nudge_seen`, either copy),
/// whether the *hard* "do not call tools" copy was present (`hard_copy_seen`),
/// and whether the tool schema was withheld (`tools_withheld`). Lets a test
/// drive the loop to its cap and assert that the soft nudge spans the window
/// while the hard copy and tool-withholding align only on the final iteration.
struct RecordingToolLooper {
    nudge_seen: Arc<std::sync::Mutex<Vec<bool>>>,
    hard_copy_seen: Arc<std::sync::Mutex<Vec<bool>>>,
    tools_withheld: Arc<std::sync::Mutex<Vec<bool>>>,
}

#[async_trait]
impl Provider for RecordingToolLooper {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let system_text = |needle: &str| {
            req.messages.iter().any(|m| {
                m.role == "system" && m.content.as_deref().is_some_and(|c| c.contains(needle))
            })
        };
        // "tool-call limit" appears in both the soft and hard wrap-up copies
        // (and not in the repeat-stall nudge), so it marks the whole window.
        self.nudge_seen
            .lock()
            .unwrap()
            .push(system_text("tool-call limit"));
        self.hard_copy_seen
            .lock()
            .unwrap()
            .push(system_text("Do not call any more tools"));
        self.tools_withheld
            .lock()
            .unwrap()
            .push(req.tools.is_empty());
        Ok(futures_util::stream::iter(vec![Ok(Chunk {
            tool_calls: vec![ToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                name: Some("bash".into()),
                arguments: r#"{"command":"echo wired"}"#.into(),
            }],
            done: true,
            ..Chunk::default()
        })])
        .boxed())
    }
}

#[tokio::test]
async fn wrap_up_nudge_graduates_then_hard_stops_and_withholds_tools_on_final_iteration() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "keep going".into());
    let registry = ToolRegistry::with_defaults();
    let approve = AlwaysApprove;
    let nudge_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let hard_copy_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let tools_withheld = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = RecordingToolLooper {
        nudge_seen: nudge_seen.clone(),
        hard_copy_seen: hard_copy_seen.clone(),
        tools_withheld: tools_withheld.clone(),
    };
    // Cap 5 with WRAP_UP_AT_REMAINING == 3: remaining counts down 5,4,3,2,1.
    let tools = ToolContext::new(&registry, dir.path(), &approve, 5, &TEST_MATRIX);

    run_turn(
        &provider,
        &store,
        &tools,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    let seen = nudge_seen.lock().unwrap();
    // The provider is hit once per iteration, up to the cap.
    assert_eq!(seen.len(), 5, "loop should run to the iteration cap");
    // A wrap-up nudge (either copy) fires across the window (remaining <= 3).
    assert_eq!(seen.as_slice(), &[false, false, true, true, true]);
    // But the hard "do not call tools" copy is reserved for the final
    // iteration (remaining == 1) -- the earlier window gets the soft nudge.
    let hard = hard_copy_seen.lock().unwrap();
    assert_eq!(hard.as_slice(), &[false, false, false, false, true]);
    // ...and tool-withholding aligns exactly with the hard copy, so the
    // instruction never tells the model to stop while tools are still offered.
    let withheld = tools_withheld.lock().unwrap();
    assert_eq!(withheld.as_slice(), &[false, false, false, false, true]);
}

#[tokio::test]
async fn no_wrap_up_nudge_when_cap_is_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "keep going".into());
    let registry = ToolRegistry::with_defaults();
    let approve = AlwaysApprove;
    let nudge_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = RecordingToolLooper {
        nudge_seen: nudge_seen.clone(),
        hard_copy_seen: Arc::new(std::sync::Mutex::new(Vec::new())),
        tools_withheld: Arc::new(std::sync::Mutex::new(Vec::new())),
    };
    let tools = ToolContext::new(&registry, dir.path(), &approve, 1, &TEST_MATRIX);

    run_turn(
        &provider,
        &store,
        &tools,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    let seen = nudge_seen.lock().unwrap();
    // With a single-iteration cap there is no "next step" to wrap up toward.
    assert_eq!(seen.as_slice(), &[false]);
}

/// Loops on tool calls while tools are advertised, but emits a final text
/// answer the moment the request carries no tools. Lets a test prove that
/// withholding tools on the last iteration forces a real answer.
struct FinalizesWhenToolsWithdrawn;
#[async_trait]
impl Provider for FinalizesWhenToolsWithdrawn {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let chunk = if req.tools.is_empty() {
            Chunk {
                delta: "wrapped up".into(),
                done: true,
                ..Chunk::default()
            }
        } else {
            Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_1".into()),
                    name: Some("bash".into()),
                    arguments: r#"{"command":"echo loop"}"#.into(),
                }],
                done: true,
                ..Chunk::default()
            }
        };
        Ok(futures_util::stream::iter(vec![Ok(chunk)]).boxed())
    }
}

#[tokio::test]
async fn cap_finalization_produces_answer_not_stopped_notice() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "review this".into());
    let registry = ToolRegistry::with_defaults();
    let approve = AlwaysApprove;
    // A model that never finishes on its own would previously loop to the cap
    // and yield "[stopped: reached tool-call limit]". Withholding tools on the
    // final iteration (RC3, #454) must instead force a real text answer.
    let tools = ToolContext::new(&registry, dir.path(), &approve, 3, &TEST_MATRIX);

    let final_msg = run_turn(
        &FinalizesWhenToolsWithdrawn,
        &store,
        &tools,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    assert!(
        final_msg.content.contains("wrapped up"),
        "the turn must end with a real answer, got: {}",
        final_msg.content
    );
    assert!(
        !final_msg
            .content
            .contains("[stopped: reached tool-call limit]"),
        "withholding tools on the final iteration must avoid the dead-end notice"
    );
}

// ----- #244 R4: tool-argument parse feedback -----

/// First call emits a tool call with malformed JSON arguments; the second returns
/// plain text (the model "self-correcting" after seeing the parse error).
struct BadArgsThenText {
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Provider for BadArgsThenText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_bad".into()),
                    name: Some("bash".into()),
                    arguments: "{not valid json".into(),
                }],
                done: true,
                ..Chunk::default()
            })]
        } else {
            vec![Ok(Chunk {
                delta: "fixed and done".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

#[tokio::test]
async fn invalid_tool_args_return_parse_error_and_loop_continues() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "go".into());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;
    let calls = Arc::new(AtomicUsize::new(0));

    let mut finished_success = true;
    let msg = run_turn(
        &BadArgsThenText {
            calls: calls.clone(),
        },
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if let AgentEvent::ToolCallFinished { success, .. } = ev {
                finished_success = success;
            }
        },
    )
    .await
    .unwrap();

    // The model got a second turn and produced a real answer.
    assert_eq!(msg.content, "fixed and done");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // The bad call surfaced as a failed tool result, not a silent Null.
    assert!(!finished_success);

    // History integrity: the assistant tool_calls message has a matching tool
    // reply, and that reply tells the model its JSON was invalid.
    let history = store.get_messages(&s.id);
    let tool_reply = history
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("a tool result must exist for the bad call");
    assert!(
        tool_reply.content.contains("not valid JSON"),
        "tool reply should explain the parse failure, got: {}",
        tool_reply.content
    );
}

/// First turn: a tool call whose JSON args are cut off, on a chunk flagged
/// `truncated` (output-token cap, #528). Second turn: a real answer.
struct TruncatedToolArgs {
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Provider for TruncatedToolArgs {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_cut".into()),
                    name: Some("write".into()),
                    arguments: r#"{"path": "docs/rfc.md"#.into(),
                }],
                done: true,
                truncated: true,
                ..Chunk::default()
            })]
        } else {
            vec![Ok(Chunk {
                delta: "done in chunks".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

#[tokio::test]
async fn truncated_tool_args_report_truncation_not_invalid_json() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "write a long file".into());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;
    let calls = Arc::new(AtomicUsize::new(0));

    let msg = run_turn(
        &TruncatedToolArgs {
            calls: calls.clone(),
        },
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    assert_eq!(msg.content, "done in chunks");
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let history = store.get_messages(&s.id);
    let tool_reply = history
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("a tool result must exist for the truncated call");
    assert!(
        tool_reply.content.contains("truncated"),
        "a cap-truncated call should report truncation, got: {}",
        tool_reply.content
    );
    assert!(
        !tool_reply.content.contains("not valid JSON"),
        "truncation must not be mislabeled as invalid JSON (#528), got: {}",
        tool_reply.content
    );
}

/// First turn: a truncated chunk (cut tool-call JSON, `done:false`) followed by
/// a *clean* terminal chunk (`done:true`, `truncated:false`) -- mirroring a
/// provider that streams a `length`/`MaxTokens` frame and then a separate
/// terminal frame. The trailing clean chunk must NOT reset the truncation flag
/// (OR-accumulate, not last-write), or the cut call re-mislabels as invalid
/// JSON -- the exact #528 regression the `|=` guards against. Second turn: a
/// real answer.
struct TruncatedThenCleanTerminal {
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Provider for TruncatedThenCleanTerminal {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![
                Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("call_cut".into()),
                        name: Some("write".into()),
                        arguments: r#"{"path": "docs/rfc.md"#.into(),
                    }],
                    done: false,
                    truncated: true,
                    ..Chunk::default()
                }),
                Ok(Chunk {
                    done: true,
                    truncated: false,
                    ..Chunk::default()
                }),
            ]
        } else {
            vec![Ok(Chunk {
                delta: "done in chunks".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

#[tokio::test]
async fn truncation_survives_a_trailing_clean_terminal_chunk() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "write a long file".into());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;
    let calls = Arc::new(AtomicUsize::new(0));

    let msg = run_turn(
        &TruncatedThenCleanTerminal {
            calls: calls.clone(),
        },
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    assert_eq!(msg.content, "done in chunks");

    let tool_reply = store
        .get_messages(&s.id)
        .into_iter()
        .find(|m| m.role == Role::Tool)
        .expect("a tool result must exist for the truncated call");
    assert!(
        tool_reply.content.contains("truncated"),
        "a trailing clean chunk must not reset the truncation flag (#528), got: {}",
        tool_reply.content
    );
    assert!(
        !tool_reply.content.contains("not valid JSON"),
        "truncation must not be mislabeled as invalid JSON (#528), got: {}",
        tool_reply.content
    );
}

// ----- #244 R1: transient-error retry with backoff -----

/// Returns a transient setup error for the first `fails` calls, then a text turn.
struct FlakySetup {
    fails: usize,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Provider for FlakySetup {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.fails {
            return Err(LlmError::Transport("connection refused".into()));
        }
        let chunks = vec![Ok(Chunk {
            delta: "recovered".into(),
            done: true,
            ..Chunk::default()
        })];
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

/// Always fails the request setup with a fatal (client) error.
struct FatalSetup {
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Provider for FatalSetup {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(LlmError::Api {
            status: 401,
            message: "unauthorized".into(),
        })
    }
}

/// First call yields a transient error mid-stream; `emit_first` controls whether a
/// token is emitted before the error. Later calls return a text turn.
struct MidStreamErr {
    calls: Arc<AtomicUsize>,
    emit_first: bool,
}
#[async_trait]
impl Provider for MidStreamErr {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            let mut chunks: Vec<Result<Chunk, LlmError>> = Vec::new();
            if self.emit_first {
                chunks.push(Ok(Chunk {
                    delta: "partial".into(),
                    ..Chunk::default()
                }));
            }
            chunks.push(Err(LlmError::Transport("reset".into())));
            return Ok(futures_util::stream::iter(chunks).boxed());
        }
        let chunks = vec![Ok(Chunk {
            delta: "recovered".into(),
            done: true,
            ..Chunk::default()
        })];
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

async fn run_text_turn(provider: &dyn Provider) -> (Result<Message, AgentError>, bool) {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "hi".into());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;
    let mut errored = false;
    let res = run_turn(
        provider,
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if let AgentEvent::Error { .. } = ev {
                errored = true;
            }
        },
    )
    .await;
    (res, errored)
}

/// Like `run_text_turn` but captures every emitted event, for assertions on the
/// `Reconnecting` / `ConnectionFailed` surface (#928).
async fn collect_turn_events(
    provider: &dyn Provider,
) -> (Result<Message, AgentError>, Vec<AgentEvent>) {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "hi".into());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;
    let mut events: Vec<AgentEvent> = Vec::new();
    let res = run_turn(
        provider,
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| events.push(ev),
    )
    .await;
    (res, events)
}

#[tokio::test(start_paused = true)]
async fn transient_setup_error_retries_then_succeeds() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = FlakySetup {
        fails: 2,
        calls: calls.clone(),
    };
    let (res, errored) = run_text_turn(&provider).await;
    assert_eq!(res.unwrap().content, "recovered");
    assert!(!errored, "recovered turn should not surface an error");
    assert_eq!(calls.load(Ordering::SeqCst), 3, "two retries then success");
}

#[tokio::test(start_paused = true)]
async fn fatal_setup_error_surfaces_without_retry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = FatalSetup {
        calls: calls.clone(),
    };
    let (res, errored) = run_text_turn(&provider).await;
    assert!(res.is_err(), "fatal error must surface");
    assert!(errored, "an Error event should fire");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "fatal errors are not retried"
    );
}

/// Fails the first `fails` requests with a 429 RateLimited (optionally with a
/// Retry-After), then returns a text turn — to exercise the window-aware path.
struct RateLimitedSetup {
    fails: usize,
    retry_after: Option<std::time::Duration>,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Provider for RateLimitedSetup {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.fails {
            return Err(LlmError::RateLimited {
                retry_after: self.retry_after,
                message: "rate limit: TPM exceeded".into(),
            });
        }
        let chunks = vec![Ok(Chunk {
            delta: "recovered".into(),
            done: true,
            ..Chunk::default()
        })];
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

#[test]
fn rate_limit_delay_honors_retry_after_clamped() {
    use std::time::Duration;
    // Retry-After honored verbatim when sane.
    assert_eq!(rate_limit_delay(0, Some(Duration::from_secs(12))), 12_000);
    // Clamped to the max ceiling.
    assert_eq!(
        rate_limit_delay(0, Some(Duration::from_secs(9999))),
        RATE_LIMIT_BACKOFF_MAX_MS
    );
    // Absent -> exponential on the 0-based attempt: 1s, 2s, 4s ...
    assert_eq!(rate_limit_delay(0, None), 1_000);
    assert_eq!(rate_limit_delay(1, None), 2_000);
    assert_eq!(rate_limit_delay(2, None), 4_000);
    // Far-out attempt saturates at the ceiling, never overflows.
    assert_eq!(rate_limit_delay(64, None), RATE_LIMIT_BACKOFF_MAX_MS);
}

#[test]
fn retry_backoff_routes_by_regime() {
    let rl = LlmError::RateLimited {
        retry_after: Some(std::time::Duration::from_secs(5)),
        message: "tpm".into(),
    };
    let blip = LlmError::Transport("reset".into());
    let fatal = LlmError::Api {
        status: 400,
        message: "bad".into(),
    };
    // Rate-limit uses its own budget + Retry-After (transport attempt irrelevant).
    assert_eq!(retry_backoff_ms(&rl, 99, 0), Some(5_000));
    assert_eq!(retry_backoff_ms(&rl, 0, MAX_RATE_LIMIT_ATTEMPTS), None);
    // Transport blip uses the snappy schedule + wider transport budget (#928).
    assert_eq!(retry_backoff_ms(&blip, 1, 0), Some(RETRY_BACKOFF_BASE_MS));
    // The per-attempt backoff is clamped so a wider budget never balloons: the
    // raw schedule would give 250 << 6 = 16s at attempt 7, but it caps at 5s.
    assert_eq!(retry_backoff_ms(&blip, 7, 0), Some(RETRY_BACKOFF_CAP_MS));
    assert!(
        retry_backoff_ms(&blip, MAX_TRANSPORT_ATTEMPTS - 1, 0).unwrap() <= RETRY_BACKOFF_CAP_MS
    );
    // Budget exhausted at the transport cap, not the (smaller) anomaly cap.
    assert_eq!(
        retry_backoff_ms(&blip, MAX_PROVIDER_ATTEMPTS, 0),
        Some(1_000)
    );
    assert_eq!(retry_backoff_ms(&blip, MAX_TRANSPORT_ATTEMPTS, 0), None);
    // Fatal is never retried.
    assert_eq!(retry_backoff_ms(&fatal, 1, 0), None);
}

#[tokio::test(start_paused = true)]
async fn rate_limited_then_success_completes_turn() {
    // A 429 window clears: two RateLimited rejections then success. The
    // seconds-scale backoff is waited out (virtual time) and the turn recovers.
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = RateLimitedSetup {
        fails: 2,
        retry_after: None,
        calls: calls.clone(),
    };
    let (res, errored) = run_text_turn(&provider).await;
    assert_eq!(res.unwrap().content, "recovered");
    assert!(
        !errored,
        "a recovered rate-limit turn should not surface an error"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3, "two waits then success");
}

#[tokio::test(start_paused = true)]
async fn rate_limited_honors_retry_after_and_recovers() {
    // With a Retry-After present the turn still recovers (delay is honored,
    // virtual time advances through it).
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = RateLimitedSetup {
        fails: 1,
        retry_after: Some(std::time::Duration::from_secs(20)),
        calls: calls.clone(),
    };
    let (res, _) = run_text_turn(&provider).await;
    assert_eq!(res.unwrap().content, "recovered");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "one wait then success");
}

#[tokio::test(start_paused = true)]
async fn persistent_rate_limit_fails_after_bounded_attempts() {
    // A window that never clears must fail cleanly after MAX_RATE_LIMIT_ATTEMPTS,
    // not spin forever. `fails` is large so every attempt is a 429.
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = RateLimitedSetup {
        fails: 999,
        retry_after: None,
        calls: calls.clone(),
    };
    let (res, errored) = run_text_turn(&provider).await;
    assert!(res.is_err(), "a persistent rate limit must surface");
    assert!(errored);
    // First attempt + MAX_RATE_LIMIT_ATTEMPTS retries before giving up.
    assert_eq!(
        calls.load(Ordering::SeqCst),
        MAX_RATE_LIMIT_ATTEMPTS + 1,
        "bounded by the rate-limit budget, separate from the transport budget"
    );
}

#[tokio::test(start_paused = true)]
async fn mid_stream_error_before_emit_retries() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = MidStreamErr {
        calls: calls.clone(),
        emit_first: false,
    };
    let (res, errored) = run_text_turn(&provider).await;
    assert_eq!(res.unwrap().content, "recovered");
    assert!(!errored);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "retried after pre-emit blip"
    );
}

#[tokio::test(start_paused = true)]
async fn mid_stream_error_after_emit_surfaces() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = MidStreamErr {
        calls: calls.clone(),
        emit_first: true,
    };
    let (res, events) = collect_turn_events(&provider).await;
    assert!(
        res.is_err(),
        "error after streamed output must surface, not replay"
    );
    // A mid-stream transport drop ends the turn with a connection error, not the
    // generic Error, and never a resume/retry (#928).
    let connection_failed = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ConnectionFailed { .. }))
        .count();
    assert_eq!(connection_failed, 1, "one terminal connection_failed");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::Reconnecting { .. })),
        "a mid-stream drop does not reconnect -- it surfaces"
    );
    // The single "partial" token is emitted exactly once: no re-issue, no dup.
    let partial_tokens = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Token { delta, .. } if delta == "partial"))
        .count();
    assert_eq!(partial_tokens, 1, "streamed content is not duplicated");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "no retry once tokens reached the UI"
    );
}

#[tokio::test(start_paused = true)]
async fn pre_stream_drop_emits_reconnecting_then_recovers() {
    // Two pre-stream transport drops then success: each retry surfaces a
    // Reconnecting event counting toward the transport budget, and the turn
    // recovers cleanly with no connection_failed (#928).
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = FlakySetup {
        fails: 2,
        calls: calls.clone(),
    };
    let (res, events) = collect_turn_events(&provider).await;
    assert_eq!(res.unwrap().content, "recovered");

    let reconnects: Vec<(u32, u32)> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Reconnecting {
                attempt,
                max_attempts,
                ..
            } => Some((*attempt, *max_attempts)),
            _ => None,
        })
        .collect();
    assert_eq!(
        reconnects,
        vec![
            (2, MAX_TRANSPORT_ATTEMPTS as u32),
            (3, MAX_TRANSPORT_ATTEMPTS as u32)
        ],
        "one reconnecting per retry, counting the upcoming attempt"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ConnectionFailed { .. })),
        "a recovered turn never surfaces connection_failed"
    );
}

#[tokio::test(start_paused = true)]
async fn transport_budget_exhaustion_emits_connection_failed() {
    // A transport that never recovers: retries up to the transport budget, then
    // surfaces connection_failed (not the generic Error), never spinning (#928).
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = FlakySetup {
        fails: 999,
        calls: calls.clone(),
    };
    let (res, events) = collect_turn_events(&provider).await;
    assert!(res.is_err(), "an unrecoverable transport drop must surface");

    let reconnects = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Reconnecting { .. }))
        .count();
    // One Reconnecting per retry: MAX_TRANSPORT_ATTEMPTS calls -> N-1 retries.
    assert_eq!(reconnects, MAX_TRANSPORT_ATTEMPTS - 1);
    let connection_failed = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ConnectionFailed { .. }))
        .count();
    assert_eq!(connection_failed, 1, "one terminal connection_failed");
    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::Error { .. })),
        "a transport failure is a connection error, not a generic Error"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        MAX_TRANSPORT_ATTEMPTS,
        "bounded by the transport budget"
    );
}

// ----- #244 R2: repeated-call / no-progress guard -----

/// Always emits the identical `bash` tool call, recording per-request whether the
/// repeat-nudge system message was present -- a model stuck in a no-progress loop.
struct RepeatProvider {
    calls: Arc<AtomicUsize>,
    saw_nudge: Arc<std::sync::Mutex<Vec<bool>>>,
}
#[async_trait]
impl Provider for RepeatProvider {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let saw = req.messages.iter().any(|m| {
            m.role == "system"
                && m.content
                    .as_deref()
                    .is_some_and(|c| c.contains("without making progress"))
        });
        self.saw_nudge.lock().unwrap().push(saw);
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = vec![Ok(Chunk {
            tool_calls: vec![ToolCallDelta {
                index: 0,
                id: Some(format!("call_{n}")),
                name: Some("bash".into()),
                arguments: r#"{"command":"echo loop"}"#.into(),
            }],
            done: true,
            ..Chunk::default()
        })];
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

#[tokio::test]
async fn repeated_identical_calls_nudge_then_break() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "go".into());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;
    let calls = Arc::new(AtomicUsize::new(0));
    let saw_nudge = Arc::new(std::sync::Mutex::new(Vec::new()));

    // A generous cap so the *guard* (not the cap) is what stops the turn.
    let tools = ToolContext::new(&registry, &root, &approve, 20, &TEST_MATRIX);
    let msg = run_turn(
        &RepeatProvider {
            calls: calls.clone(),
            saw_nudge: saw_nudge.clone(),
        },
        &store,
        &tools,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    // Broke at REPEAT_BREAK_AT (5), well before the cap of 20.
    assert_eq!(calls.load(Ordering::SeqCst), REPEAT_BREAK_AT);
    // The corrective nudge was injected at least once before the break.
    assert!(
        saw_nudge.lock().unwrap().iter().any(|&b| b),
        "the repeat nudge should have been sent"
    );
    // The turn ends with the structured stall marker, not the generic cap notice
    // (#658 -- the reason is carried structurally; the marker text is static).
    assert_eq!(msg.content, StopReason::Stall.marker());
    assert!(
        msg.content.contains("repeated a tool call"),
        "got: {}",
        msg.content
    );
}

// ----- #244 R7 + R1/R2 follow-up nits: loop polish -----

/// Cancels the turn, then returns a transient setup error -- so the retry backoff
/// runs with the token already cancelled. Counts how many times the provider is hit.
struct CancelDuringBackoff {
    calls: Arc<AtomicUsize>,
    cancel: CancelToken,
}
#[async_trait]
impl Provider for CancelDuringBackoff {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.cancel.cancel();
        Err(LlmError::Transport("reset".into()))
    }
}

#[tokio::test(start_paused = true)]
async fn no_extra_chat_stream_after_cancel_during_backoff() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "hi".into());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;
    let calls = Arc::new(AtomicUsize::new(0));
    let cancel = CancelToken::new();
    let provider = CancelDuringBackoff {
        calls: calls.clone(),
        cancel: cancel.clone(),
    };

    let _ = run_turn(
        &provider,
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        cancel,
        |_| {},
    )
    .await;

    // Without the cancel-after-backoff check the loop would issue two more wasted
    // calls before surfacing; the fix stops it dead after the first (#244 R1 nit).
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "cancel during backoff must not issue another provider call"
    );
}

/// Returns an empty (no text, no tool call) but successful stream for the first
/// `empties` calls, then a real text turn -- a provider hiccup (#244 R7).
struct EmptyThenText {
    empties: usize,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Provider for EmptyThenText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.empties {
            return Ok(futures_util::stream::iter(vec![Ok(Chunk {
                done: true,
                ..Chunk::default()
            })])
            .boxed());
        }
        Ok(futures_util::stream::iter(vec![Ok(Chunk {
            delta: "recovered".into(),
            done: true,
            ..Chunk::default()
        })])
        .boxed())
    }
}

#[tokio::test(start_paused = true)]
async fn empty_response_retries_then_recovers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = EmptyThenText {
        empties: 1,
        calls: calls.clone(),
    };
    let (res, errored) = run_text_turn(&provider).await;
    assert_eq!(res.unwrap().content, "recovered");
    assert!(
        !errored,
        "an anomaly that recovers should not surface an error"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "one empty response retried, then success"
    );
}

/// Always returns an empty successful stream -- a persistent anomaly.
struct AlwaysEmpty {
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Provider for AlwaysEmpty {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(futures_util::stream::iter(vec![Ok(Chunk {
            done: true,
            ..Chunk::default()
        })])
        .boxed())
    }
}

#[tokio::test(start_paused = true)]
async fn empty_response_exhausts_to_notice() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "go".into());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;
    let calls = Arc::new(AtomicUsize::new(0));

    let msg = run_turn(
        &AlwaysEmpty {
            calls: calls.clone(),
        },
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    // The empty-response retry is bounded by the provider-attempt cap within one
    // iteration -- not an infinite spin.
    assert_eq!(calls.load(Ordering::SeqCst), MAX_PROVIDER_ATTEMPTS);
    // ...and the turn ends with a clear notice, never a silent empty bubble.
    assert!(
        msg.content.contains("empty response"),
        "got: {}",
        msg.content
    );
}

#[tokio::test]
async fn repeat_nudge_persists_through_the_recovery_window() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "go".into());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;
    let calls = Arc::new(AtomicUsize::new(0));
    let saw_nudge = Arc::new(std::sync::Mutex::new(Vec::new()));

    let tools = ToolContext::new(&registry, &root, &approve, 20, &TEST_MATRIX);
    run_turn(
        &RepeatProvider {
            calls: calls.clone(),
            saw_nudge: saw_nudge.clone(),
        },
        &store,
        &tools,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    // Five requests fire before the break at REPEAT_BREAK_AT (5). The nudge is
    // re-armed across the whole window, so both the count-4 request (index 3) and
    // the count-5 request (index 4) carry it -- not just the first (#244 R2 nit).
    let seen = saw_nudge.lock().unwrap();
    assert_eq!(
        seen.as_slice(),
        &[false, false, false, true, true],
        "{seen:?}"
    );
}

#[tokio::test]
async fn done_event_reports_estimated_token_count() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "hi".into());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;

    let seen = std::sync::Mutex::new(None);
    run_turn(
        &TextProvider,
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if let AgentEvent::Done { token_count, .. } = ev {
                *seen.lock().unwrap() = Some(token_count);
            }
        },
    )
    .await
    .unwrap();

    // The Done event must carry a populated, non-zero estimate (#244 R6) rather
    // than the previous hardcoded None.
    let tc = seen.lock().unwrap().expect("Done event was emitted");
    let tc = tc.expect("token_count must be populated, not None");
    assert!(tc > 0, "estimated token count should be positive, got {tc}");
}

#[tokio::test]
async fn done_event_reports_f1b_prefill_and_compaction_telemetry() {
    // #441: the Done event carries one prefill estimate per provider round-trip,
    // and zero compaction fires for a tiny transcript well under the budget.
    let store = SessionStore::new();
    let s = store.create_session(None);
    // A non-trivial prompt so the chars/4 proxy rounds to a positive estimate.
    store.add_message(
        &s.id,
        Role::User,
        "please summarize the architecture of this project in detail".into(),
    );
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;

    type F1b = (Option<Vec<u32>>, Option<u32>, Option<u32>);
    let seen: std::sync::Mutex<Option<F1b>> = std::sync::Mutex::new(None);
    run_turn(
        &TextProvider,
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if let AgentEvent::Done {
                prefill_estimates,
                tier1_fires,
                tier2_fires,
                ..
            } = ev
            {
                *seen.lock().unwrap() = Some((prefill_estimates, tier1_fires, tier2_fires));
            }
        },
    )
    .await
    .unwrap();

    let (prefill, t1, t2) = seen
        .lock()
        .unwrap()
        .clone()
        .expect("Done event was emitted");
    let prefill = prefill.expect("prefill_estimates must be populated");
    // TextProvider answers in a single round-trip -> exactly one estimate, > 0.
    assert_eq!(prefill.len(), 1, "one prefill estimate per round-trip");
    assert!(
        prefill[0] > 0,
        "estimate should be positive, got {}",
        prefill[0]
    );
    // A two-message transcript is far under budget: no compaction engages, and
    // Tier-2 is default-off regardless.
    assert_eq!(t1, Some(0), "Tier-1 must not fire under budget");
    assert_eq!(t2, Some(0), "Tier-2 must not fire (and is default-off)");
}

#[tokio::test]
async fn done_event_reports_prompt_latency_for_round_zero() {
    // #960: the Done event carries `prompt_latency_ms` -- the wall-clock from the
    // provider stream being returned to its first output-carrying chunk. A
    // provider that sleeps 40ms before its first chunk must produce a latency of
    // at least ~40ms (and never exceed the whole turn's wall-clock).
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "hi".into());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;

    let seen: std::sync::Mutex<Option<Option<u32>>> = std::sync::Mutex::new(None);
    let turn_start = std::time::Instant::now();
    run_turn(
        &DelayedFirstToken { delay_ms: 40 },
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if let AgentEvent::Done {
                prompt_latency_ms, ..
            } = ev
            {
                *seen.lock().unwrap() = Some(prompt_latency_ms);
            }
        },
    )
    .await
    .unwrap();
    let turn_ms = turn_start.elapsed().as_millis() as u32;

    let latency = seen
        .lock()
        .unwrap()
        .expect("Done event was emitted")
        .expect("prompt_latency_ms must be populated when a token streamed");
    assert!(
        latency >= 35,
        "prompt latency should reflect the ~40ms pre-first-token delay, got {latency}"
    );
    assert!(
        latency <= turn_ms,
        "prompt latency ({latency}ms) must not exceed the whole turn ({turn_ms}ms)"
    );
}

// ----- #244 R8: oversized tool-result history truncation -----

#[test]
fn truncate_tool_result_passes_through_small_input() {
    let small = "ok";
    assert_eq!(truncate_tool_result(small), small);
    let exact = "x".repeat(TOOL_RESULT_MAX_BYTES);
    assert_eq!(truncate_tool_result(&exact), exact);
}

#[test]
fn truncate_tool_result_caps_and_keeps_head_and_tail() {
    let big = format!("HEAD{}TAIL", "x".repeat(TOOL_RESULT_MAX_BYTES * 2));
    let out = truncate_tool_result(&big);
    assert!(
        out.len() <= TOOL_RESULT_MAX_BYTES,
        "truncated to {} bytes, cap {}",
        out.len(),
        TOOL_RESULT_MAX_BYTES
    );
    assert!(out.starts_with("HEAD"), "head slice must survive");
    assert!(out.ends_with("TAIL"), "tail slice must survive");
    assert!(out.contains("truncated"), "marker must be present");
}

#[test]
fn truncate_tool_result_respects_utf8_boundaries() {
    // A grinning-face emoji is 4 bytes; a naive byte slice mid-codepoint would
    // panic. The output must stay valid UTF-8 and within the cap.
    let big = "😀".repeat(TOOL_RESULT_MAX_BYTES);
    let out = truncate_tool_result(&big);
    assert!(out.len() <= TOOL_RESULT_MAX_BYTES);
    assert!(out.chars().count() > 0);
}

// ----- #378: persisted reasoning sizing -----

#[test]
fn truncate_reasoning_passes_through_small_input() {
    let small = "thought briefly";
    assert_eq!(truncate_reasoning(small), small);
    let exact = "x".repeat(REASONING_MAX_BYTES);
    assert_eq!(truncate_reasoning(&exact), exact);
}

#[test]
fn truncate_reasoning_caps_and_keeps_tail() {
    // A chain-of-thought is most useful at its end, so unlike a tool result the
    // truncation keeps the TAIL and drops the head.
    let big = format!("HEAD{}TAIL", "x".repeat(REASONING_MAX_BYTES * 2));
    let out = truncate_reasoning(&big);
    assert!(
        out.len() <= REASONING_MAX_BYTES,
        "truncated to {} bytes, cap {}",
        out.len(),
        REASONING_MAX_BYTES
    );
    assert!(out.ends_with("TAIL"), "tail slice must survive");
    assert!(!out.contains("HEAD"), "head must be dropped (tail-biased)");
    assert!(out.contains("truncated"), "marker must be present");
}

#[test]
fn truncate_reasoning_respects_utf8_boundaries() {
    // Tail-biased slicing must land on a char boundary, not mid-codepoint.
    let big = "😀".repeat(REASONING_MAX_BYTES);
    let out = truncate_reasoning(&big);
    assert!(out.len() <= REASONING_MAX_BYTES);
    assert!(out.chars().count() > 0);
}

/// A tool whose result is far larger than the history cap.
struct BigResultTool {
    bytes: usize,
}
#[async_trait]
impl ff_tools::Tool for BigResultTool {
    fn name(&self) -> &str {
        "big"
    }
    fn description(&self) -> &str {
        "returns a large blob"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    fn safety(&self, _args: &serde_json::Value) -> Safety {
        Safety::ReadOnly
    }
    async fn run(&self, _args: serde_json::Value, _root: &Path) -> ff_tools::ToolOutcome {
        ff_tools::ToolOutcome::ok("B".repeat(self.bytes))
    }
}

/// First call invokes `big`; second call returns plain text.
struct BigToolThenText {
    calls: AtomicUsize,
}
#[async_trait]
impl Provider for BigToolThenText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_1".into()),
                    name: Some("big".into()),
                    arguments: "{}".into(),
                }],
                done: true,
                ..Chunk::default()
            })]
        } else {
            vec![Ok(Chunk {
                delta: "done".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

#[tokio::test]
async fn oversized_tool_result_is_truncated_in_history_but_full_in_event() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "go".into());
    let mut registry = ToolRegistry::new();
    let full_len = TOOL_RESULT_MAX_BYTES * 3;
    registry.register(Box::new(BigResultTool { bytes: full_len }));
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;

    let mut event_result_len = 0usize;
    run_turn(
        &BigToolThenText {
            calls: AtomicUsize::new(0),
        },
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if let AgentEvent::ToolCallFinished { result, .. } = ev {
                event_result_len = result.len();
            }
        },
    )
    .await
    .unwrap();

    // The UI event keeps the full, untruncated result.
    assert_eq!(event_result_len, full_len, "event must carry full content");

    // History (replayed to the model) is capped.
    let history = store.get_messages(&s.id);
    let tool_msg = history
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("a tool result message");
    assert!(
        tool_msg.content.len() <= TOOL_RESULT_MAX_BYTES,
        "history result {} exceeds cap {}",
        tool_msg.content.len(),
        TOOL_RESULT_MAX_BYTES
    );
    assert!(
        tool_msg.content.contains("truncated"),
        "history result should carry the truncation marker"
    );
}

/// A tool whose result is a compressible JSON blob of a configurable size.
struct JsonResultTool {
    summary_len: usize,
}
#[async_trait]
impl ff_tools::Tool for JsonResultTool {
    fn name(&self) -> &str {
        "jsonbig"
    }
    fn description(&self) -> &str {
        "returns a large json blob"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    fn safety(&self, _args: &serde_json::Value) -> Safety {
        Safety::ReadOnly
    }
    async fn run(&self, _args: serde_json::Value, _root: &Path) -> ff_tools::ToolOutcome {
        let blob = serde_json::to_string(&serde_json::json!({
            "summary": "x".repeat(self.summary_len),
            "items": (0..50).map(|i| format!("row {i}")).collect::<Vec<_>>(),
        }))
        .unwrap();
        ff_tools::ToolOutcome::ok(blob)
    }
}

/// First call invokes `jsonbig`; second returns plain text.
struct JsonToolThenText {
    calls: AtomicUsize,
}
#[async_trait]
impl Provider for JsonToolThenText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_1".into()),
                    name: Some("jsonbig".into()),
                    arguments: "{}".into(),
                }],
                done: true,
                ..Chunk::default()
            })]
        } else {
            vec![Ok(Chunk {
                delta: "done".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

/// Call 0 invokes `jsonbig` (oversized -> compacted at ingest, gains a retrieve
/// marker); call 1 reads the key out of that marker and invokes
/// `compaction_retrieve`; call 2 returns plain text. Drives the RC6 path.
struct JsonThenRetrieveThenText {
    calls: AtomicUsize,
}
#[async_trait]
impl Provider for JsonThenRetrieveThenText {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = match n {
            0 => vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_1".into()),
                    name: Some("jsonbig".into()),
                    arguments: "{}".into(),
                }],
                done: true,
                ..Chunk::default()
            })],
            1 => {
                // Pull the retrieve key out of the compacted tool result the loop
                // just appended to the request, exactly as a real model would.
                let key = req
                    .messages
                    .iter()
                    .filter_map(|m| m.content.as_deref())
                    .find_map(|c| c.split("[compacted; retrieve key=").nth(1))
                    .and_then(|rest| rest.split(']').next())
                    .map(str::to_owned)
                    .expect("the jsonbig result must carry a retrieve key");
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("call_2".into()),
                        name: Some(COMPACTION_RETRIEVE_TOOL.into()),
                        arguments: format!(r#"{{"key":"{key}"}}"#),
                    }],
                    done: true,
                    ..Chunk::default()
                })]
            }
            _ => vec![Ok(Chunk {
                delta: "done".into(),
                done: true,
                ..Chunk::default()
            })],
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

/// RC6 (#476): `compaction_retrieve` returns a verbatim original that is, by
/// definition, larger than the cap (exceeding the cap is the only reason it was
/// compacted). The ingest gate must NOT re-compact the retrieve result -- doing
/// so re-emits the same elision and the same deterministic key, a no-op loop
/// that makes retrieve useless for any large original. The model's retrieve must
/// land verbatim, marker-free.
#[tokio::test]
async fn retrieve_output_is_not_recompacted_at_ingest() {
    let store = std::sync::Arc::new(SessionStore::new());
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "expand the diff".into());
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(JsonResultTool {
        summary_len: TOOL_RESULT_MAX_BYTES + 4000,
    }));
    registry.register(Box::new(ff_tools::CompactionRetrieveTool::new(
        std::sync::Arc::clone(&store),
    )));
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;

    run_turn(
        &JsonThenRetrieveThenText {
            calls: AtomicUsize::new(0),
        },
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    let history = store.get_messages(&s.id);
    let tool_msgs: Vec<_> = history.iter().filter(|m| m.role == Role::Tool).collect();
    assert_eq!(
        tool_msgs.len(),
        2,
        "one jsonbig result + one retrieve result"
    );

    // Sanity: the original jsonbig result was compacted at ingest (it is oversized).
    let jsonbig = &tool_msgs[0];
    assert!(
        jsonbig.content.contains("[compacted; retrieve key="),
        "oversized jsonbig result should be compacted: {}",
        jsonbig.content
    );

    // The fix: the retrieve result reaches the transcript verbatim -- no marker,
    // and larger than the cap (so it was neither re-compacted nor truncated).
    let retrieved = &tool_msgs[1];
    assert!(
        !retrieved.content.contains("[compacted; retrieve key="),
        "retrieve output must NOT be re-compacted at ingest: {}",
        &retrieved.content[..retrieved.content.len().min(200)]
    );
    assert!(
        retrieved.content.len() > TOOL_RESULT_MAX_BYTES,
        "retrieve output must be the verbatim (oversized) original, got {} bytes",
        retrieved.content.len()
    );
    // And it is exactly the original stored under the marker key.
    let key = jsonbig
        .content
        .rsplit("key=")
        .next()
        .unwrap()
        .trim_end_matches(']')
        .trim();
    assert_eq!(
        retrieved.content,
        store.compaction_original(key).expect("original stored"),
        "retrieve output must equal the verbatim stored original"
    );
}

#[tokio::test]
async fn large_tool_result_is_compacted_and_retrievable() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "go".into());
    let mut registry = ToolRegistry::new();
    // Over the hard per-result byte cap, so it takes the reversible ingest
    // compaction path (RC1 #453: only oversized results are compacted at ingest).
    registry.register(Box::new(JsonResultTool {
        summary_len: TOOL_RESULT_MAX_BYTES + 4000,
    }));
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;

    let mut full_len = 0usize;
    run_turn(
        &JsonToolThenText {
            calls: AtomicUsize::new(0),
        },
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if let AgentEvent::ToolCallFinished { result, .. } = ev {
                full_len = result.len();
            }
        },
    )
    .await
    .unwrap();

    let history = store.get_messages(&s.id);
    let tool_msg = history
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("a tool result message");

    // The stored content was compacted (smaller than the original) and carries
    // the reversible retrieve marker.
    assert!(
        tool_msg.content.contains("[compacted; retrieve key="),
        "compacted tool result must carry a retrieve marker: {}",
        tool_msg.content
    );
    assert!(
        tool_msg.content.len() < full_len,
        "compacted content ({}) must be smaller than the original ({full_len})",
        tool_msg.content.len()
    );

    // The marker key resolves to the verbatim original in the store.
    let key = tool_msg
        .content
        .rsplit("key=")
        .next()
        .unwrap()
        .trim_end_matches(']')
        .trim();
    let original = store
        .compaction_original(key)
        .expect("the original must be retrievable by the marker key");
    assert_eq!(
        original.len(),
        full_len,
        "retrieved original must be verbatim"
    );
}

/// RC1 reproduction (PR #452 review timeline): a large tool result produced
/// on the CURRENT turn must reach the model verbatim. Today it is compressed
/// at ingest (lib.rs `compress_one` on the just-produced outcome) before the
/// model ever reads it, so the model's first read of a large diff comes back
/// already `[compacted; retrieve key=...]`. That forces a `compaction_retrieve`
/// round-trip (or a re-read with a different tool), which is the redundant-step
/// loop Abid observed. The cold-tail path (`compact_cold_collect` +
/// `KEEP_RECENT_VERBATIM`) already compresses results once they age out of the
/// hot window, so ingest-time compression of the hot result is both redundant
/// and harmful.
///
/// This test asserts the DESIRED behavior and currently FAILS, reproducing RC1.
#[tokio::test]
async fn current_turn_tool_result_reaches_model_verbatim() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "review this".into());
    let mut registry = ToolRegistry::new();
    // Within the hard per-result byte cap: must be stored verbatim at ingest.
    registry.register(Box::new(JsonResultTool { summary_len: 4000 }));
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;

    let mut full_len = 0usize;
    run_turn(
        &JsonToolThenText {
            calls: AtomicUsize::new(0),
        },
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if let AgentEvent::ToolCallFinished { result, .. } = ev {
                full_len = result.len();
            }
        },
    )
    .await
    .unwrap();

    let history = store.get_messages(&s.id);
    let tool_msg = history
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("a tool result message");

    // The most-recent tool result is in the hot window: the model has not yet
    // had a chance to read it, so it must be stored verbatim with NO retrieve
    // marker. A marker here means the model's first read is already compacted.
    assert!(
        !tool_msg.content.contains("[compacted; retrieve key="),
        "a current-turn tool result must NOT be compacted at ingest \
         (the model has not read it yet); got: {}",
        &tool_msg.content[..tool_msg.content.len().min(200)]
    );
    assert_eq!(
        tool_msg.content.len(),
        full_len,
        "the current-turn tool result must reach the transcript verbatim \
         (stored {} bytes vs original {full_len})",
        tool_msg.content.len()
    );
}

/// A tool result below the compaction threshold is stored verbatim with no
/// marker and no stored original.
struct SmallResultTool;
#[async_trait]
impl ff_tools::Tool for SmallResultTool {
    fn name(&self) -> &str {
        "small"
    }
    fn description(&self) -> &str {
        "returns a tiny blob"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    fn safety(&self, _args: &serde_json::Value) -> Safety {
        Safety::ReadOnly
    }
    async fn run(&self, _args: serde_json::Value, _root: &Path) -> ff_tools::ToolOutcome {
        ff_tools::ToolOutcome::ok("ok: 3 results")
    }
}

struct SmallToolThenText {
    calls: AtomicUsize,
}
#[async_trait]
impl Provider for SmallToolThenText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_1".into()),
                    name: Some("small".into()),
                    arguments: "{}".into(),
                }],
                done: true,
                ..Chunk::default()
            })]
        } else {
            vec![Ok(Chunk {
                delta: "done".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

#[tokio::test]
async fn small_tool_result_is_passed_through_uncompacted() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "go".into());
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(SmallResultTool));
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;

    run_turn(
        &SmallToolThenText {
            calls: AtomicUsize::new(0),
        },
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_ev| {},
    )
    .await
    .unwrap();

    let history = store.get_messages(&s.id);
    let tool_msg = history
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("a tool result message");
    assert_eq!(tool_msg.content, "ok: 3 results");
    assert!(!tool_msg.content.contains("[compacted"));
}

// ----- #244 R5: in-turn context-pressure flush -----

/// Always returns the same short text turn, counting how many times it is hit.
/// Used to observe whether a flush fired (the flush issues an extra provider
/// call before the main turn).
struct CountingText {
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Provider for CountingText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(futures_util::stream::iter(vec![Ok(Chunk {
            delta: "ok".into(),
            done: true,
            ..Chunk::default()
        })])
        .boxed())
    }
}

#[tokio::test]
async fn context_pressure_under_budget_skips_flush() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "hi".into());
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;
    let calls = Arc::new(AtomicUsize::new(0));

    run_turn(
        &CountingText {
            calls: calls.clone(),
        },
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    // A tiny transcript is well under the budget fraction -> no flush, so the
    // provider is hit exactly once (the main turn).
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "under-budget turn must not trigger a flush"
    );
}

#[tokio::test]
async fn context_pressure_over_budget_triggers_flush() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    // Push the token estimate over 0.75 * context_budget (0.8 * 32K = 25.6K;
    // threshold = 0.75 * 25.6K = 19.2K). tokenx-rs: 200K "x" ~ 33K tokens.
    let huge = "x".repeat(200_000);
    store.add_message(&s.id, Role::User, huge);
    let registry = ToolRegistry::new();
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;
    let calls = Arc::new(AtomicUsize::new(0));

    // Sanity: the seeded transcript really is over budget.
    let pressure = ProxyTokenEstimator::default().assess(&store.get_messages(&s.id), "mock");
    assert!(
        pressure.is_over(DEFAULT_FLUSH_AT_FRACTION),
        "test precondition: transcript must exceed the flush threshold"
    );

    // A NoReply flush writes nothing, so no provenance event must fire (#283).
    let mut flush_events = 0usize;
    run_turn(
        &CountingText {
            calls: calls.clone(),
        },
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if matches!(ev, AgentEvent::MemoryFlushed { .. }) {
                flush_events += 1;
            }
        },
    )
    .await
    .unwrap();

    // Over budget on the first iteration -> a silent flush fires (one extra
    // provider call that returns no tool calls -> NoReply) before the main turn.
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "over-budget turn must run exactly one flush before the main turn"
    );
    assert_eq!(
        flush_events, 0,
        "a flush that writes nothing (NoReply) must not emit MemoryFlushed"
    );
    // The flush is silent: it must not add any message to the visible transcript
    // (memory writes go to disk, not the session). Still just the one user msg
    // plus the single assistant reply from the main turn.
    let history = store.get_messages(&s.id);
    assert_eq!(history.len(), 2, "flush must not mutate the transcript");
}

/// A `memory_write` tool that records how many times it ran, so an over-budget
/// flush can actually persist a durable fact during the test (#283).
struct CountingMemoryWrite {
    writes: Arc<AtomicUsize>,
}
#[async_trait]
impl ff_tools::Tool for CountingMemoryWrite {
    fn name(&self) -> &str {
        "memory_write"
    }
    fn description(&self) -> &str {
        "persists a durable fact"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "text": { "type": "string" } }
        })
    }
    fn safety(&self, _args: &serde_json::Value) -> Safety {
        Safety::Write
    }
    async fn run(&self, _args: serde_json::Value, _root: &Path) -> ff_tools::ToolOutcome {
        self.writes.fetch_add(1, Ordering::SeqCst);
        ff_tools::ToolOutcome::ok("saved")
    }
}

/// Calls `memory_write` once (the flush's first request), then answers with
/// plain text — which both terminates the flush loop and finishes the main turn.
struct FlushWriteThenText {
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Provider for FlushWriteThenText {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("w1".into()),
                    name: Some("memory_write".into()),
                    arguments: r#"{"text":"user prefers dark mode"}"#.into(),
                }],
                done: true,
                ..Chunk::default()
            })]
        } else {
            vec![Ok(Chunk {
                delta: "ok".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

#[tokio::test]
async fn over_budget_flush_that_writes_emits_memory_flushed_event() {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "x".repeat(200_000));
    let writes = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(CountingMemoryWrite {
        writes: writes.clone(),
    }));
    let root = std::env::current_dir().unwrap();
    let approve = AlwaysApprove;

    let mut flushed: Vec<u32> = Vec::new();
    run_turn(
        &FlushWriteThenText {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |ev| {
            if let AgentEvent::MemoryFlushed { writes, .. } = ev {
                flushed.push(writes);
            }
        },
    )
    .await
    .unwrap();

    assert_eq!(
        flushed,
        vec![1],
        "a flush that wrote one fact emits one MemoryFlushed carrying writes=1"
    );
    assert_eq!(
        writes.load(Ordering::SeqCst),
        1,
        "the flush ran memory_write exactly once"
    );
    // Provenance, not mutation: the visible transcript stays user + assistant.
    assert_eq!(store.get_messages(&s.id).len(), 2);
}

fn extract_marker_key(content: &str) -> Option<String> {
    if !content.contains(COMPACTION_MARKER_PREFIX) {
        return None;
    }
    Some(
        content
            .rsplit("key=")
            .next()
            .unwrap()
            .trim_end_matches(']')
            .trim()
            .to_string(),
    )
}

#[tokio::test]
async fn over_pressure_compacts_wire_but_store_stays_verbatim() {
    // Build a transcript heavy enough to clear the 0.75 budget fraction with
    // a long cold prefix of large, compressible blobs followed by small recent
    // turns. The wire request must be compacted; the store must stay verbatim.
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);

    // 10 cold messages, each a large JSON blob that compresses decisively.
    let mut cold_contents = Vec::new();
    for i in 0..10 {
        let blob = serde_json::to_string(&serde_json::json!({
            "idx": i,
            "summary": "y".repeat(15000),
            "items": (0..60).collect::<Vec<i32>>(),
        }))
        .unwrap();
        let role = if i % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        store.add_message(&s.id, role, blob.clone());
        cold_contents.push(blob);
    }
    // 6 small recent turns kept byte-identical on the wire.
    let recents = ["r0", "r1", "r2", "r3", "r4", "r5"];
    for (i, r) in recents.iter().enumerate() {
        let role = if i % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        store.add_message(&s.id, role, (*r).to_string());
    }

    // Sanity: we are actually over the extractive threshold.
    let history = store.get_messages(&s.id);
    let pressure = ProxyTokenEstimator::default().assess(&history, "mock");
    assert!(
        pressure.is_over(EXTRACTIVE_COMPACT_AT_FRACTION),
        "test transcript must exceed the extractive threshold: fraction={}",
        pressure.fraction()
    );

    let registry = ToolRegistry::new();
    let root = dir.path().to_path_buf();
    let approve = AlwaysApprove;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = RecordingProvider { seen: seen.clone() };

    run_turn(
        &provider,
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    let wire = seen.lock().unwrap().clone();
    // Wire has no system prompt here -> first message is the first cold blob.
    // The cold prefix must be compacted (marker present) and shorter.
    let cold_wire = &wire[0];
    assert!(
        cold_wire
            .content
            .as_deref()
            .unwrap()
            .contains(COMPACTION_MARKER_PREFIX),
        "cold prefix must be compacted on the wire"
    );
    assert!(
        cold_wire.content.as_deref().unwrap().len() < cold_contents[0].len(),
        "compacted wire content must be shorter than the original blob"
    );

    // The 6 most recent messages stay byte-identical on the wire.
    let n = wire.len();
    for (i, r) in recents.iter().enumerate() {
        assert_eq!(
            wire[n - recents.len() + i].content.as_deref().unwrap(),
            *r,
            "recent message {i} must be verbatim on the wire"
        );
    }

    // The store keeps the full verbatim transcript untouched.
    let stored = store.get_messages(&s.id);
    for (i, original) in cold_contents.iter().enumerate() {
        assert_eq!(
            &stored[i].content, original,
            "store must keep cold message {i} verbatim"
        );
    }

    // Each compacted blob's original is retrievable by its marker key.
    let key = extract_marker_key(cold_wire.content.as_deref().unwrap()).unwrap();
    assert_eq!(
        store.compaction_original(&key).as_deref(),
        Some(cold_contents[0].as_str()),
        "the verbatim original must be retrievable by its marker key"
    );
}

#[tokio::test]
async fn below_pressure_wire_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "a short question".into());
    store.add_message(&s.id, Role::Assistant, "a short answer".into());
    store.add_message(&s.id, Role::User, "another short one".into());

    let history = store.get_messages(&s.id);
    let pressure = ProxyTokenEstimator::default().assess(&history, "mock");
    assert!(
        !pressure.is_over(EXTRACTIVE_COMPACT_AT_FRACTION),
        "small transcript must be below the extractive threshold"
    );

    let registry = ToolRegistry::new();
    let root = dir.path().to_path_buf();
    let approve = AlwaysApprove;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = RecordingProvider { seen: seen.clone() };

    run_turn(
        &provider,
        &store,
        &ctx(&registry, &root, &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    let wire = seen.lock().unwrap().clone();
    for m in &wire {
        assert!(
            !m.content
                .as_deref()
                .unwrap_or_default()
                .contains(COMPACTION_MARKER_PREFIX),
            "below pressure, no message may be compacted on the wire"
        );
    }
    // And nothing was persisted to the originals store.
    assert!(
        history
            .iter()
            .all(|m| store.compaction_original(&m.id).is_none()),
        "below pressure, no originals may be persisted"
    );
}

#[tokio::test]
async fn tier2_summarizes_cold_prefix_but_store_stays_verbatim() {
    // Single-line cold messages that the Tier-1 extractive pass leaves alone
    // (one line each, so its line-elision never triggers): pressure stays high
    // *after* Tier 1, so the Tier-2 abstractive fallback engages and collapses
    // the cold prefix into a single summary message.
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);

    let mut cold_contents = Vec::new();
    for i in 0..30 {
        let line = format!("cold-{i} {}", "lorem ipsum dolor sit amet ".repeat(300));
        let role = if i % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        store.add_message(&s.id, role, line.clone());
        cold_contents.push(line);
    }
    let recents = ["r0", "r1", "r2", "r3", "r4", "r5"];
    for (i, r) in recents.iter().enumerate() {
        let role = if i % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        store.add_message(&s.id, role, (*r).to_string());
    }

    // Sanity: even after Tier 1 would run, these single-line blobs do not
    // shrink, so the wire stays over the Tier-2 fraction.
    let history = store.get_messages(&s.id);
    let wire_t1 =
        ExtractiveCompactor::default().compact_cold_collect(&history, KEEP_RECENT_VERBATIM);
    let pressure = ProxyTokenEstimator::default().assess(&wire_t1.messages, "mock");
    assert!(
        pressure.is_over(0.90),
        "post-Tier-1 transcript must exceed the Tier-2 fraction: fraction={}",
        pressure.fraction()
    );

    let registry = ToolRegistry::new();
    let root = dir.path().to_path_buf();
    let approve = AlwaysApprove;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = RecordingProvider { seen: seen.clone() };

    let mut tctx = ctx(&registry, &root, &approve);
    tctx.abstractive = AbstractiveConfig {
        enabled: true,
        fire_at_fraction: 0.90,
        ..AbstractiveConfig::default()
    };

    run_turn(
        &provider,
        &store,
        &tctx,
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    // The main-turn wire begins with the synthetic summary (no system prompt
    // passed), and the 6 most recent messages stay byte-identical.
    let wire = seen.lock().unwrap().clone();
    let summary = &wire[0];
    assert_eq!(summary.role.as_str(), "user");
    let summary_text = summary.content.as_deref().unwrap();
    assert!(
        summary_text.contains("Summary of") && summary_text.contains(COMPACTION_MARKER_PREFIX),
        "the wire must lead with the abstractive summary + retrieve marker"
    );
    let n = wire.len();
    for (i, r) in recents.iter().enumerate() {
        assert_eq!(
            wire[n - recents.len() + i].content.as_deref().unwrap(),
            *r,
            "recent message {i} must be verbatim on the wire"
        );
    }
    // The collapsed cold prefix is far smaller than the 30 originals combined.
    assert!(wire.len() < history.len(), "cold prefix must be collapsed");

    // The store keeps the full verbatim transcript (plus this turn's reply).
    let stored = store.get_messages(&s.id);
    assert!(stored.len() >= history.len());
    for (i, original) in cold_contents.iter().enumerate() {
        assert_eq!(
            &stored[i].content, original,
            "store keeps cold {i} verbatim"
        );
    }

    // Reversible: the marker key resolves to the verbatim cold block.
    let key = extract_marker_key(summary_text).unwrap();
    let retrieved = store
        .compaction_original(&key)
        .expect("cold block is retrievable");
    assert!(retrieved.contains("cold-0"));
    assert!(retrieved.contains("cold-23"));
}

// ---- #458 RC5: per-turn semantic read dedupe ----

#[tokio::test]
async fn rereads_of_unchanged_file_collapse_to_sentinel() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "hello world\nsecond line\n").unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "read it twice".into());
    let registry = ToolRegistry::with_defaults();
    let approve = AlwaysApprove;

    // Two views of the same file under *different* args (a line range on the
    // second), so the byte-identical repeat-breaker would NOT fire -- only RC5's
    // content dedupe catches it.
    struct ViewTwice {
        calls: AtomicUsize,
    }
    #[async_trait]
    impl Provider for ViewTwice {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let chunk = match n {
                0 => Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("v1".into()),
                        name: Some("view".into()),
                        arguments: r#"{"path":"f.txt"}"#.into(),
                    }],
                    done: true,
                    ..Chunk::default()
                },
                1 => Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("v2".into()),
                        name: Some("view".into()),
                        arguments: r#"{"path":"f.txt","start_line":1}"#.into(),
                    }],
                    done: true,
                    ..Chunk::default()
                },
                _ => Chunk {
                    delta: "done".into(),
                    done: true,
                    ..Chunk::default()
                },
            };
            Ok(futures_util::stream::iter(vec![Ok(chunk)]).boxed())
        }
    }

    run_turn(
        &ViewTwice {
            calls: AtomicUsize::new(0),
        },
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    let history = store.get_messages(&s.id);
    let results: Vec<&str> = history
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(results.len(), 2, "two view calls -> two tool results");
    assert!(
        results[0].contains("hello world"),
        "first read returns full content: {}",
        results[0]
    );
    assert!(
        results[1].contains("unchanged since step"),
        "re-read of unchanged file is deduped to the sentinel: {}",
        results[1]
    );
}

/// Verifies changed files are re-read in full, not deduped. Uses Unix-specific
/// `printf` command to modify the file.
#[cfg(unix)]
#[tokio::test]
async fn changed_file_is_not_deduped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "v1\n").unwrap();
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "read, change, read".into());
    let registry = ToolRegistry::with_defaults();
    let approve = AlwaysApprove;

    // view -> bash overwrites the file -> view again. The second read's content
    // differs (different hash), so it must NOT be deduped.
    struct ViewEditView {
        calls: AtomicUsize,
    }
    #[async_trait]
    impl Provider for ViewEditView {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let tc = |id: &str, name: &str, args: &str| ToolCallDelta {
                index: 0,
                id: Some(id.into()),
                name: Some(name.into()),
                arguments: args.into(),
            };
            let chunk = match n {
                0 => Chunk {
                    tool_calls: vec![tc("v1", "view", r#"{"path":"f.txt"}"#)],
                    done: true,
                    ..Chunk::default()
                },
                1 => Chunk {
                    tool_calls: vec![tc("b2", "bash", r#"{"command":"printf 'v2\n' > f.txt"}"#)],
                    done: true,
                    ..Chunk::default()
                },
                2 => Chunk {
                    tool_calls: vec![tc("v3", "view", r#"{"path":"f.txt"}"#)],
                    done: true,
                    ..Chunk::default()
                },
                _ => Chunk {
                    delta: "done".into(),
                    done: true,
                    ..Chunk::default()
                },
            };
            Ok(futures_util::stream::iter(vec![Ok(chunk)]).boxed())
        }
    }

    run_turn(
        &ViewEditView {
            calls: AtomicUsize::new(0),
        },
        &store,
        &ctx(&registry, dir.path(), &approve),
        &s.id,
        "mock",
        None,
        false,
        ReasoningVisibility::All,
        CancelToken::new(),
        |_| {},
    )
    .await
    .unwrap();

    let history = store.get_messages(&s.id);
    // The second view (id v3) returns the new content in full, not a sentinel.
    let reread = history
        .iter()
        .find(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some("v3"))
        .expect("re-read tool result present");
    assert!(
        reread.content.contains("v2"),
        "changed file is re-read in full: {}",
        reread.content
    );
    assert!(
        !reread.content.contains("unchanged since step"),
        "a changed file must NOT be deduped: {}",
        reread.content
    );
}

#[test]
fn context_breakdown_splits_system_tools_and_messages() {
    fn msg(role: Role, content: &str, reasoning: Option<&str>) -> Message {
        Message {
            id: "m".into(),
            session_id: "s".into(),
            role,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            attachments: None,
            reasoning: reasoning.map(str::to_string),
            stop_reason: None,
            author_name: None,
            created_at: 0,
        }
    }

    let system = SystemPrompt {
        stable: "x".repeat(40),
        volatile: "v".repeat(20),
    };
    let tool_schemas = vec![
        serde_json::json!({"type": "function", "function": {"name": "a"}}),
        serde_json::json!({"type": "function", "function": {"name": "b"}}),
    ];
    let messages = vec![
        msg(Role::User, &"y".repeat(20), None),
        msg(Role::Assistant, &"z".repeat(16), Some(&"r".repeat(4))),
    ];

    let b = context_breakdown(Some(&system), &tool_schemas, &messages);

    // Buckets use the same tokenx-rs estimator as ProxyTokenEstimator::assess.
    assert!(
        b.system_tokens > 0,
        "non-empty system prompt -> non-zero tokens"
    );
    assert_eq!(b.tool_specs, 2);
    assert_eq!(b.message_count, 2);
    assert!(
        b.tool_tokens > 0,
        "non-empty tool schemas -> non-zero tokens"
    );
    assert!(
        b.message_tokens > 0,
        "non-empty messages -> non-zero tokens"
    );

    // Key invariant: message_tokens must equal what the estimator computes for
    // the same messages (so the popover bar sums to token_count).
    let estimator = ProxyTokenEstimator {
        budget_tokens: 100_000,
    };
    let pressure = estimator.assess(&messages, "any");
    assert_eq!(
        b.message_tokens, pressure.estimated_tokens as u32,
        "breakdown.message_tokens must equal estimator.assess() for same messages"
    );
}

#[test]
fn context_breakdown_handles_absent_system_prompt() {
    let b = context_breakdown(None, &[], &[]);
    assert_eq!(b.system_tokens, 0);
    assert_eq!(b.tool_tokens, 0);
    assert_eq!(b.tool_specs, 0);
    assert_eq!(b.message_tokens, 0);
    assert_eq!(b.message_count, 0);
}
