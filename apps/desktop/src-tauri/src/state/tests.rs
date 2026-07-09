use super::*;
use ff_core::{ReasoningEffort, ReasoningVisibility};
use ff_llm::{ChatMessage, ChatRequest};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// Approvals are only registered while a turn is live, so a cancel token must
// exist for the session first — mirrors `send_message` registering the token
// before the turn (and thus any approval) starts.
fn arm(state: &AppState, session_id: &str) {
    state.register_cancel(session_id, CancelToken::new());
}

// ---- LLM-summarized session title (#671 item 2b) ----

struct FixedTitleProvider(String);
#[async_trait::async_trait]
impl Provider for FixedTitleProvider {
    async fn chat_stream(
        &self,
        _req: ff_llm::ChatRequest,
    ) -> Result<ff_llm::ChunkStream, ff_llm::LlmError> {
        use futures_util::StreamExt;
        let chunk = ff_llm::Chunk {
            delta: self.0.clone(),
            done: true,
            ..Default::default()
        };
        Ok(futures_util::stream::iter(vec![Ok(chunk)]).boxed())
    }
}

#[test]
fn sanitize_title_strips_quotes_punctuation_and_caps() {
    assert_eq!(
        sanitize_generated_title("  \"Fix the parser bug\".  "),
        Some("Fix the parser bug".to_string())
    );
    // First non-empty line only.
    assert_eq!(
        sanitize_generated_title("\n\nRefactor the store\nignored trailer"),
        Some("Refactor the store".to_string())
    );
    // Collapses inner whitespace and strips backticks.
    assert_eq!(
        sanitize_generated_title("`Update   session   model`"),
        Some("Update session model".to_string())
    );
    // Empty / punctuation-only -> None (keep the heuristic title).
    assert_eq!(sanitize_generated_title("   "), None);
    assert_eq!(sanitize_generated_title("\"\""), None);
    // Length cap.
    let long = "word ".repeat(40);
    let title = sanitize_generated_title(&long).unwrap();
    assert!(title.chars().count() <= 60);
}

#[tokio::test]
async fn generate_title_summarizes_after_first_turn() {
    use ff_core::Role;
    let state = AppState::new();
    let s = state.store.create_session(None);
    state
        .store
        .add_message(&s.id, Role::User, "help me fix the parser".into());
    state
        .store
        .add_message(&s.id, Role::Assistant, "Sure, let's look.".into());

    let provider = FixedTitleProvider("Fix the parser bug".into());
    let title = state
        .generate_session_title(&provider, &s.id, "test-model", CancelToken::new())
        .await;
    assert_eq!(title.as_deref(), Some("Fix the parser bug"));
}

#[tokio::test]
async fn generate_title_skips_before_a_reply_and_after_second_turn() {
    use ff_core::Role;
    let provider = FixedTitleProvider("Title".into());

    // Only a user message, no assistant reply yet -> skip.
    let state = AppState::new();
    let s = state.store.create_session(None);
    state.store.add_message(&s.id, Role::User, "first".into());
    assert!(state
        .generate_session_title(&provider, &s.id, "m", CancelToken::new())
        .await
        .is_none());

    // Two user messages (past the first turn) -> skip so we don't re-title.
    state
        .store
        .add_message(&s.id, Role::Assistant, "reply".into());
    state.store.add_message(&s.id, Role::User, "second".into());
    assert!(state
        .generate_session_title(&provider, &s.id, "m", CancelToken::new())
        .await
        .is_none());
}

#[tokio::test]
async fn generate_title_bails_when_cancelled() {
    use ff_core::Role;
    let state = AppState::new();
    let s = state.store.create_session(None);
    state.store.add_message(&s.id, Role::User, "hi".into());
    state
        .store
        .add_message(&s.id, Role::Assistant, "hello".into());

    let provider = FixedTitleProvider("Greeting".into());
    let cancel = CancelToken::new();
    cancel.cancel();
    assert!(state
        .generate_session_title(&provider, &s.id, "m", cancel)
        .await
        .is_none());
}

// #277: the desktop store persists across a "restart". `build_session_store`
// delegates to `SessionStore::open`, so this exercises the same path-backed
// contract over an explicit temp db (without touching the real config dir).
#[test]
fn session_db_survives_restart() {
    use ff_core::Role;
    use ff_session::SessionStore;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");

    let session_id = {
        let store = SessionStore::open(&path).unwrap();
        let s = store.create_session(Some("persist me".into()));
        store.add_message(&s.id, Role::User, "still here?".into());
        s.id
    };

    // A fresh store over the same path == an app restart.
    let store = SessionStore::open(&path).unwrap();
    let sessions = store.list_sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, session_id);
    let msgs = store.get_messages(&session_id);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content, "still here?");
}

#[tokio::test]
async fn resolve_approval_delivers_decision() {
    let state = AppState::new();
    arm(&state, "sess");
    let rx = state.register_approval("sess", "call-1");
    state.resolve_approval("sess", "call-1", true);
    assert!(rx.await.unwrap());
    // Slot is freed after resolve.
    assert!(state.approvals.lock().unwrap().pending.is_empty());
}

#[tokio::test]
async fn cancel_pending_denies_via_drop() {
    let state = AppState::new();
    arm(&state, "sess-x");
    let rx = state.register_approval("sess-x", "call-2");
    state.cancel_pending_approvals("sess-x");
    // Sender was dropped -> RecvError -> caller treats as deny.
    assert!(rx.await.is_err());
}

#[tokio::test]
async fn cancel_pending_only_affects_matching_session() {
    let state = AppState::new();
    arm(&state, "sess-a");
    arm(&state, "sess-b");
    let rx_a = state.register_approval("sess-a", "a");
    let rx_b = state.register_approval("sess-b", "b");
    state.cancel_pending_approvals("sess-a");
    // sess-b survives.
    state.resolve_approval("sess-b", "b", true);
    assert!(rx_a.await.is_err());
    assert!(rx_b.await.unwrap());
}

#[tokio::test]
async fn resolve_unknown_call_is_noop() {
    let state = AppState::new();
    // Must not panic.
    state.resolve_approval("nope", "nope", true);
}

#[tokio::test]
async fn register_without_live_cancel_denies_immediately() {
    let state = AppState::new();
    // No `register_cancel` -> the turn is gone (or never started). The TOCTOU
    // guard refuses the prompt: the receiver resolves to Err -> deny, and no
    // orphaned sender is left behind to hang the approver.
    let rx = state.register_approval("ghost", "call-x");
    assert!(rx.await.is_err());
    assert!(state.approvals.lock().unwrap().pending.is_empty());
}

#[tokio::test]
async fn standalone_op_with_liveness_token_can_be_approved() {
    // Mirrors the install_skill flow: a non-turn op registers a liveness token
    // under its request_id so the TOCTOU guard admits it, keyed (id, id).
    let state = AppState::new();
    state.register_cancel("op", CancelToken::new());
    let rx = state.register_approval("op", "op");
    state.resolve_approval("op", "op", true);
    assert!(rx.await.unwrap());
    // The flow releases the liveness token afterward.
    assert!(state.take_cancel("op").is_some());
}

#[test]
fn has_active_turn_tracks_registered_cancel() {
    // Gates the orphaned-row reconciliation (#646): true only while a turn
    // holds a live cancel token, so the sweep never touches a live turn's
    // reserved tail row.
    let state = AppState::new();
    assert!(!state.has_active_turn("sess"), "no turn registered");
    state.register_cancel("sess", CancelToken::new());
    assert!(state.has_active_turn("sess"), "turn is live");
    state.take_cancel("sess");
    assert!(!state.has_active_turn("sess"), "turn finished");
}

#[tokio::test]
async fn get_messages_reconciles_orphan_only_when_no_active_turn() {
    // End-to-end guard behavior: an orphaned empty assistant row is relabeled
    // on load, but not while a turn is live for that session (#646).
    use ff_core::Role;
    let state = AppState::new();
    let s = state.store.create_session(None);
    state.store.add_message(&s.id, Role::User, "hi".into());
    state
        .store
        .add_message(&s.id, Role::Assistant, String::new());

    // Live turn: the reserved tail row must be left untouched.
    state.register_cancel(&s.id, CancelToken::new());
    if !state.has_active_turn(&s.id) {
        state
            .store
            .reconcile_orphaned_assistant_rows(&s.id, ff_agent::INTERRUPTED_NOTICE);
    }
    assert_eq!(
        state.store.get_messages(&s.id)[1].content,
        "",
        "live turn's reserved row must not be reconciled"
    );

    // Turn ends: now the orphan is relabeled on the next load.
    state.take_cancel(&s.id);
    if !state.has_active_turn(&s.id) {
        state
            .store
            .reconcile_orphaned_assistant_rows(&s.id, ff_agent::INTERRUPTED_NOTICE);
    }
    assert_eq!(
        state.store.get_messages(&s.id)[1].content,
        ff_agent::INTERRUPTED_NOTICE,
        "orphan is relabeled once no turn is live"
    );
}

#[test]
fn take_cancel_if_removes_only_its_own_token() {
    // The clean single-turn finish: the token a turn registered is still the
    // live one, so its epilogue removes it.
    let state = AppState::new();
    let token = CancelToken::new();
    state.register_cancel("sess", token.clone());
    assert!(
        state.take_cancel_if("sess", &token).is_some(),
        "a turn drops its own still-registered token"
    );
    assert!(state.take_cancel("sess").is_none(), "map is now empty");
}

#[test]
fn take_cancel_if_leaves_a_successor_turns_token() {
    // The edit-during-live-turn race (#464/#468 blocker): turn A is cancelled
    // and a re-run turn B registers its token before A's epilogue runs. A must
    // NOT strip B's token, or B loses its Stop button and auto-denies tools.
    let state = AppState::new();
    let token_a = CancelToken::new();
    state.register_cancel("sess", token_a.clone());

    // Turn B replaces the session's token (mirrors edit_message -> spawn).
    let token_b = CancelToken::new();
    state.register_cancel("sess", token_b.clone());

    // Turn A's epilogue: identity check fails, B's token survives.
    assert!(
        state.take_cancel_if("sess", &token_a).is_none(),
        "A must not remove B's token"
    );
    // B's token is still live and removable by B's own epilogue.
    assert!(
        state.take_cancel_if("sess", &token_b).is_some(),
        "B drops its own token cleanly"
    );
}

#[tokio::test]
async fn colliding_call_ids_across_sessions_are_isolated() {
    let state = AppState::new();
    arm(&state, "sess-1");
    arm(&state, "sess-2");
    // Same LLM-supplied call_id in two concurrent sessions must not collide.
    let rx1 = state.register_approval("sess-1", "dup");
    let rx2 = state.register_approval("sess-2", "dup");
    // Resolving one leaves the other intact.
    state.resolve_approval("sess-1", "dup", true);
    state.resolve_approval("sess-2", "dup", false);
    assert!(rx1.await.unwrap());
    assert!(!rx2.await.unwrap());
}

#[tokio::test]
async fn ask_delivers_answer() {
    let state = AppState::new();
    arm(&state, "sess");
    let rx = state.register_ask("sess", "ask-1");
    state.resolve_ask("sess", "ask-1", "main.rs".to_string());
    assert_eq!(rx.await.unwrap(), "main.rs");
}

#[tokio::test]
async fn cancel_dismisses_pending_ask_via_drop() {
    let state = AppState::new();
    arm(&state, "sess-x");
    let rx = state.register_ask("sess-x", "ask-2");
    state.cancel_pending_approvals("sess-x");
    // Sender dropped -> RecvError -> caller treats as a dismissed question.
    assert!(rx.await.is_err());
}

#[tokio::test]
async fn register_ask_without_live_turn_dismisses_immediately() {
    let state = AppState::new();
    // No `arm` -> no live cancel token -> the TOCTOU guard refuses the slot.
    let rx = state.register_ask("ghost", "ask-3");
    assert!(rx.await.is_err());
}

#[tokio::test]
async fn cancel_dismisses_ask_only_for_matching_session() {
    let state = AppState::new();
    arm(&state, "sess-a");
    arm(&state, "sess-b");
    let rx_a = state.register_ask("sess-a", "a");
    let rx_b = state.register_ask("sess-b", "b");
    state.cancel_pending_approvals("sess-a");
    state.resolve_ask("sess-b", "b", "kept".to_string());
    assert!(rx_a.await.is_err());
    assert_eq!(rx_b.await.unwrap(), "kept");
}

#[tokio::test]
async fn resolve_unknown_ask_is_noop() {
    let state = AppState::new();
    state.resolve_ask("nope", "nope", "x".to_string());
}

#[test]
fn activate_unknown_skill_errors() {
    let state = AppState::new();
    // `new()` restores the persisted phenotype from the real config dir, which
    // can pre-populate the active set; this test exercises only the activate
    // guard, so start from a known-empty baseline (test-isolation, not behavior).
    state.active_skills.lock().unwrap().clear();
    let err = state
        .activate_skill("definitely-not-installed-skill-xyz")
        .unwrap_err();
    assert!(err.contains("unknown skill"), "{err}");
    assert!(state.active_skills().is_empty());
}

#[test]
fn active_skills_is_sorted_and_deduped() {
    let state = AppState::new();
    {
        // Bypass the install-check guard: this test exercises the accessor and
        // deactivate, not the registry lookup (covered elsewhere).
        let mut guard = state.active_skills.lock().unwrap();
        // Clear the phenotype `new()` restored from the real config dir so the
        // assertions below own the full expected set (test-isolation).
        guard.clear();
        guard.insert("zeta".into());
        guard.insert("alpha".into());
        guard.insert("alpha".into());
    }
    assert_eq!(state.active_skills(), vec!["alpha", "zeta"]);
    state.deactivate_skill("alpha");
    assert_eq!(state.active_skills(), vec!["zeta"]);
    // Deactivating an absent skill is a no-op.
    state.deactivate_skill("nope");
    assert_eq!(state.active_skills(), vec!["zeta"]);
}

// First-run phenotype selection (#298). A fake resolver lets us cover the whole
// branch matrix without touching `~/.flowforge`. `codon` and `default` resolve;
// anything else is unknown.
fn fake_resolve(name: &str) -> Option<Phenotype> {
    match name {
        "codon" | "default" | "rust" => Some(Phenotype {
            name: name.to_string(),
            skills: vec![],
            model: None,
            persona: None,
            max_iterations: None,
            provider: None,
            mcp_servers: Vec::new(),
            egress: ff_core::Egress::Open,
        }),
        _ => None,
    }
}

#[test]
fn initial_phenotype_prefers_persisted_choice() {
    let pheno = initial_phenotype(Some("rust".to_string()), fake_resolve);
    assert_eq!(pheno.name, "rust");
}

#[test]
fn initial_phenotype_defaults_to_codon_when_no_persisted_choice() {
    let pheno = initial_phenotype(None, fake_resolve);
    assert_eq!(pheno.name, "codon");
}

#[test]
fn initial_phenotype_unknown_persisted_falls_through_to_codon() {
    let pheno = initial_phenotype(Some("ghost".to_string()), fake_resolve);
    assert_eq!(pheno.name, "codon");
}

#[test]
fn initial_phenotype_falls_back_to_default_when_codon_absent() {
    // Codon not installed (rare seed-failure): resolver only knows `default`.
    let resolve = |name: &str| (name == "default").then(default_phenotype);
    let pheno = initial_phenotype(None, resolve);
    assert_eq!(pheno.name, DEFAULT_PHENOTYPE);
}

#[test]
fn switch_to_unknown_phenotype_errors() {
    let state = AppState::new();
    let err = state
        .switch_phenotype("definitely-not-a-phenotype-xyz")
        .unwrap_err();
    assert!(err.contains("unknown phenotype"), "{err}");
}

#[test]
fn default_phenotype_is_always_listed() {
    let state = AppState::new();
    let names: Vec<String> = state
        .list_phenotypes()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert!(names.contains(&DEFAULT_PHENOTYPE.to_string()), "{names:?}");
}

#[test]
fn update_phenotype_rejects_immutable_default() {
    // Guard fires before any disk I/O -- the built-in `default` is never written.
    let state = AppState::new();
    let err = state.update_phenotype(default_phenotype()).unwrap_err();
    assert!(err.contains("immutable"), "{err}");
}

#[test]
fn update_phenotype_rejects_unknown_provider() {
    // Provider binding is validated against the live registry before saving, so a
    // stale editor cannot pin a phantom connection (mirrors set_session_model).
    let state = AppState::new();
    let pheno = Phenotype {
        name: "rust".into(),
        skills: vec![],
        model: None,
        persona: None,
        max_iterations: None,
        provider: Some("definitely-not-a-connection-xyz".into()),
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::Open,
    };
    let err = state.update_phenotype(pheno).unwrap_err();
    assert!(err.contains("unknown connection"), "{err}");
}

#[test]
fn apply_phenotype_records_overrides_and_drops_unknown_skills() {
    let state = AppState::new();
    // No skills are installed in the test environment, so every named skill is
    // unknown and must be dropped — never activated as a phantom.
    let pheno = Phenotype {
        name: "rust".into(),
        skills: vec!["not-installed".into()],
        model: Some("qwen3-coder".into()),
        persona: Some("You are a Rust expert.".into()),
        max_iterations: None,
        provider: None,
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::Open,
    };
    state.apply_phenotype(pheno);
    assert!(state.active_skills().is_empty());
    assert_eq!(
        state.active_model_override().as_deref(),
        Some("qwen3-coder")
    );
    assert_eq!(
        state.active_phenotype().persona.as_deref(),
        Some("You are a Rust expert.")
    );
    assert_eq!(state.active_phenotype().name, "rust");
}

/// Build a `SkillRegistry` from a tempdir of `SKILL.md` fixtures and install it
/// on `state`. Each spec is `(skill_name, declared_mcp_servers)`. The returned
/// `TempDir` is only kept to satisfy ownership; `load_dir` reads eagerly.
fn install_skills(state: &AppState, specs: &[(&str, &[&str])]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, mcp) in specs {
        let skill_dir = dir.path().join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let mut fm = format!("---\nname: {name}\ndescription: test\nversion: 0.1.0\n");
        if !mcp.is_empty() {
            fm.push_str("mcp:\n");
            for s in *mcp {
                fm.push_str(&format!("  - {s}\n"));
            }
        }
        fm.push_str("---\nbody\n");
        std::fs::write(skill_dir.join("SKILL.md"), fm).unwrap();
    }
    let (reg, errs) = SkillRegistry::load_dir(dir.path());
    assert!(errs.is_empty(), "skill fixtures failed to load: {errs:?}");
    *state.skills.write().unwrap() = reg;
    dir
}

#[test]
fn skill_declared_mcp_servers_are_required() {
    let state = AppState::new();
    let _dir = install_skills(&state, &[("codegraph", &["codegraph"]), ("plain", &[])]);
    let skills: BTreeSet<String> = ["codegraph".into(), "plain".into()].into();
    // No MCP supervisor is initialized in tests, so a required server is always
    // reported missing — exactly the "server not in mcp.json" warn path.
    assert_eq!(
        state.missing_skill_mcp_servers(&skills),
        vec!["codegraph".to_string()]
    );
}

#[test]
fn skill_without_mcp_requires_no_server() {
    let state = AppState::new();
    let _dir = install_skills(&state, &[("plain", &[])]);
    let skills: BTreeSet<String> = ["plain".into()].into();
    assert!(state.missing_skill_mcp_servers(&skills).is_empty());
}

#[test]
fn unknown_active_skill_contributes_no_mcp_requirement() {
    let state = AppState::new();
    let skills: BTreeSet<String> = ["not-installed".into()].into();
    assert!(state.missing_skill_mcp_servers(&skills).is_empty());
}

#[test]
fn missing_mcp_servers_are_deduped_and_sorted() {
    let state = AppState::new();
    let _dir = install_skills(
        &state,
        &[("a", &["zeta", "codegraph"]), ("b", &["codegraph"])],
    );
    let skills: BTreeSet<String> = ["a".into(), "b".into()].into();
    assert_eq!(
        state.missing_skill_mcp_servers(&skills),
        vec!["codegraph".to_string(), "zeta".to_string()]
    );
}

#[test]
fn unavailable_skill_mcp_servers_is_empty_for_unknown_phenotype() {
    let state = AppState::new();
    // The query resolves the phenotype by name first; an unknown name yields no
    // requirement (the command never emits in that case).
    assert!(state
        .unavailable_skill_mcp_servers("definitely-not-a-phenotype")
        .is_empty());
}

#[test]
fn unavailable_skill_mcp_servers_resolves_and_delegates_for_known_phenotype() {
    let state = AppState::new();
    // The default phenotype always resolves; no skills are installed in the test
    // environment, so it requires no MCP server. This exercises the resolve +
    // delegate path (the non-empty list building is covered by the
    // missing_skill_mcp_servers / unavailable_required_servers tests above).
    assert!(state
        .unavailable_skill_mcp_servers(DEFAULT_PHENOTYPE)
        .is_empty());
}

#[test]
fn present_but_not_running_server_is_reported_unavailable() {
    let status = |id: &str, state: McpServerState| McpServerStatus {
        id: id.into(),
        state,
        tool_count: 0,
        last_error: None,
        restarts: 0,
        pid: None,
        scope_key: None,
    };
    let required: BTreeSet<String> = ["running".into(), "failed".into(), "disabled".into()].into();
    let snapshot = [
        status("running", McpServerState::Running),
        status("failed", McpServerState::Failed),
        status("disabled", McpServerState::Disabled),
    ];
    // Only the Running server provides tools; Failed/Disabled are present in the
    // snapshot yet still reported, since their tools are unavailable.
    assert_eq!(
        AppState::unavailable_required_servers(&required, &snapshot),
        vec!["disabled".to_string(), "failed".to_string()]
    );
}

#[test]
fn apply_phenotype_returns_resolved_active_skills() {
    let state = AppState::new();
    let _dir = install_skills(&state, &[("codegraph", &["codegraph"])]);
    let resolved = state.apply_phenotype(Phenotype {
        name: "codon".into(),
        skills: vec!["codegraph".into(), "not-installed".into()],
        model: None,
        persona: None,
        max_iterations: None,
        provider: None,
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::Open,
    });
    // Unknown skills are dropped; the returned set mirrors the active set so the
    // caller can warn about MCP requirements without re-resolving.
    assert_eq!(
        resolved,
        ["codegraph".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(state.active_skills(), vec!["codegraph"]);
}

#[test]
fn apply_default_clears_overrides() {
    let state = AppState::new();
    state.apply_phenotype(Phenotype {
        name: "rust".into(),
        skills: vec![],
        model: Some("m".into()),
        persona: Some("p".into()),
        max_iterations: None,
        provider: None,
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::Open,
    });
    state.apply_phenotype(default_phenotype());
    assert!(state.active_model_override().is_none());
    assert!(state.active_phenotype().persona.is_none());
    assert!(state.active_skills().is_empty());
}

#[test]
fn unbound_session_inherits_global_active_phenotype() {
    let state = AppState::new();
    // Make the global active phenotype non-default so inheritance is observable.
    state.apply_phenotype(Phenotype {
        name: "rust".into(),
        skills: vec![],
        model: Some("qwen3-coder".into()),
        persona: Some("You are a Rust expert.".into()),
        max_iterations: Some(40),
        provider: None,
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::Open,
    });
    let s = state.store.create_session(None);
    let resolved = state.session_phenotype(&s.id);
    assert_eq!(resolved.name, "rust");
    assert_eq!(resolved.model.as_deref(), Some("qwen3-coder"));
    assert_eq!(resolved.persona.as_deref(), Some("You are a Rust expert."));
    // The per-session resolver carries the loop cap (#244-R3 x #246), so a bound
    // pane can run a different iteration budget than the global default.
    assert_eq!(resolved.max_iterations, Some(40));
}

#[test]
fn bound_session_resolves_to_its_own_phenotype() {
    let state = AppState::new();
    // Global active is the built-in default...
    let s = state.store.create_session(None);
    // ...but bind this session explicitly to `default` and confirm it resolves
    // to a real, named phenotype (the built-in is always resolvable).
    state
        .set_session_phenotype(&s.id, Some(DEFAULT_PHENOTYPE.into()))
        .unwrap();
    let resolved = state.session_phenotype(&s.id);
    assert_eq!(resolved.name, DEFAULT_PHENOTYPE);
}

#[test]
fn session_bound_to_unknown_phenotype_falls_back_to_global() {
    let state = AppState::new();
    state.apply_phenotype(Phenotype {
        name: "rust".into(),
        skills: vec![],
        model: None,
        persona: None,
        max_iterations: None,
        provider: None,
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::Open,
    });
    let s = state.store.create_session(None);
    // Inject a dangling binding directly through the store (the validated
    // setter would reject it) to exercise the resolver's graceful fallback.
    state
        .store
        .set_session_phenotype(&s.id, Some("ghost-phenotype".into()));
    let resolved = state.session_phenotype(&s.id);
    assert_eq!(
        resolved.name, "rust",
        "unknown binding inherits global active"
    );
}

// RFC 0005 Phase C (#498): three-tier model resolution. `with_registry` seeds the
// default registry (active `candle-vllm` @ Qwen3-4B + `ollama` @ llama3.2), so a
// phenotype `provider` binding is observable against a real second connection.
#[test]
fn unbound_session_resolves_to_active_connection_and_its_model() {
    let state = AppState::with_registry(ProviderRegistry::default());
    let s = state.store.create_session(None);
    let sel = state.resolve_model_selection(&s.id);
    assert_eq!(sel.connection, "candle-vllm");
    assert_eq!(sel.model, "Qwen3-4B-Instruct-2507");
}

#[test]
fn phenotype_model_override_without_provider_rides_active_connection() {
    let state = AppState::with_registry(ProviderRegistry::default());
    state.apply_phenotype(Phenotype {
        name: "rust".into(),
        skills: vec![],
        model: Some("custom-model".into()),
        persona: None,
        max_iterations: None,
        provider: None,
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::Open,
    });
    let s = state.store.create_session(None);
    let sel = state.resolve_model_selection(&s.id);
    // No provider binding -> stays on the active connection (backward compatible),
    // but now the override model is routed through it explicitly.
    assert_eq!(sel.connection, "candle-vllm");
    assert_eq!(sel.model, "custom-model");
}

#[test]
fn phenotype_provider_binding_routes_to_that_connections_model() {
    let state = AppState::with_registry(ProviderRegistry::default());
    state.apply_phenotype(Phenotype {
        name: "rust".into(),
        skills: vec![],
        model: None,
        persona: None,
        max_iterations: None,
        provider: Some("ollama".into()),
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::Open,
    });
    let s = state.store.create_session(None);
    let sel = state.resolve_model_selection(&s.id);
    // Provider bound, no explicit model -> the BOUND connection's own model, never
    // the active connection's (the RFC 0005 §11.1 cross-endpoint bug).
    assert_eq!(sel.connection, "ollama");
    assert_eq!(sel.model, "llama3.2");
}

#[test]
fn phenotype_provider_and_model_are_both_honored() {
    let state = AppState::with_registry(ProviderRegistry::default());
    state.apply_phenotype(Phenotype {
        name: "rust".into(),
        skills: vec![],
        model: Some("llama3.2:70b".into()),
        persona: None,
        max_iterations: None,
        provider: Some("ollama".into()),
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::Open,
    });
    let s = state.store.create_session(None);
    let sel = state.resolve_model_selection(&s.id);
    assert_eq!(sel.connection, "ollama");
    assert_eq!(sel.model, "llama3.2:70b");
}

// RFC 0005 §11.2 Phase D (#499): an explicit per-session selection is the top tier.
#[test]
fn session_model_override_wins_over_phenotype() {
    let state = AppState::with_registry(ProviderRegistry::default());
    // A phenotype binding that would otherwise route to ollama/llama3.2...
    state.apply_phenotype(Phenotype {
        name: "rust".into(),
        skills: vec![],
        model: None,
        persona: None,
        max_iterations: None,
        provider: Some("ollama".into()),
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::Open,
    });
    let s = state.store.create_session(None);
    // ...is overridden by an explicit session pin to the active connection.
    state
        .set_session_model(
            &s.id,
            Some(ModelSelection {
                connection: "candle-vllm".into(),
                model: "pinned-model".into(),
            }),
        )
        .unwrap();
    let sel = state.resolve_model_selection(&s.id);
    assert_eq!(sel.connection, "candle-vllm");
    assert_eq!(sel.model, "pinned-model");
}

// RFC 0005 §11.3 (#525 PR C): attachment caps are derived from the *resolved*
// `(kind, model)`, so a per-session model override is gated by the model it runs.
#[test]
fn resolved_caps_follow_session_override_model() {
    let state = AppState::with_registry(ProviderRegistry::default());
    let s = state.store.create_session(None);
    // Unbound: active candle-vllm @ Qwen3-4B is text-only.
    let base = state.resolve_model_selection(&s.id);
    assert!(!base.supports_vision);
    // Pin the session to a vision-capable Ollama model; caps follow the override.
    state
        .set_session_model(
            &s.id,
            Some(ModelSelection {
                connection: "ollama".into(),
                model: "llama3.2-vision".into(),
            }),
        )
        .unwrap();
    let sel = state.resolve_model_selection(&s.id);
    assert_eq!(sel.connection, "ollama");
    assert!(sel.supports_vision, "vision tag => derived supports_vision");
    // As of the #338 follow-up, every provider kind supports documents
    // (OpenAI/Ollama via the text-extraction fallback), so an ollama
    // session can still stage a document for extraction.
    assert!(
        sel.supports_documents,
        "ollama supports documents via extraction"
    );
}

#[test]
fn resolved_caps_fail_closed_when_connection_missing() {
    let state = AppState::with_registry(ProviderRegistry::default());
    // A phenotype bound to a connection that does not exist (e.g. since removed),
    // with a model name that *would* be vision-capable on a real connection.
    state.apply_phenotype(Phenotype {
        name: "rust".into(),
        skills: vec![],
        model: Some("llama3.2-vision".into()),
        persona: None,
        max_iterations: None,
        provider: Some("ghost-conn".into()),
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::Open,
    });
    let s = state.store.create_session(None);
    let sel = state.resolve_model_selection(&s.id);
    assert_eq!(sel.connection, "ghost-conn");
    // No kind to scope the capability lookup -> fail closed, matching the gate.
    assert!(!sel.supports_vision);
    assert!(!sel.supports_documents);
}

#[test]
fn set_session_model_rejects_unknown_connection() {
    let state = AppState::with_registry(ProviderRegistry::default());
    let s = state.store.create_session(None);
    let err = state
        .set_session_model(
            &s.id,
            Some(ModelSelection {
                connection: "ghost-conn".into(),
                model: "whatever".into(),
            }),
        )
        .unwrap_err();
    assert!(err.contains("unknown connection"), "{err}");
    // The rejected selection must NOT have been written.
    assert!(state.session_model(&s.id).is_none());
}

#[test]
fn clearing_session_model_falls_back_to_phenotype() {
    let state = AppState::with_registry(ProviderRegistry::default());
    state.apply_phenotype(Phenotype {
        name: "rust".into(),
        skills: vec![],
        model: None,
        persona: None,
        max_iterations: None,
        provider: Some("ollama".into()),
        mcp_servers: Vec::new(),
        egress: ff_core::Egress::Open,
    });
    let s = state.store.create_session(None);
    state
        .set_session_model(
            &s.id,
            Some(ModelSelection {
                connection: "candle-vllm".into(),
                model: "pinned-model".into(),
            }),
        )
        .unwrap();
    // Clearing the pin returns resolution to the phenotype tier.
    state.set_session_model(&s.id, None).unwrap();
    let sel = state.resolve_model_selection(&s.id);
    assert_eq!(sel.connection, "ollama");
    assert_eq!(sel.model, "llama3.2");
}

// --- RFC 0018 C3 (#590): tiered MCP resolution (session > phenotype > global) ---

fn mcp_cfg(id: &str, command: &str, scope: McpScope) -> McpServerConfig {
    McpServerConfig {
        id: id.into(),
        command: command.into(),
        args: vec!["serve".into(), "--mcp".into()],
        env: Default::default(),
        disabled: false,
        scope,
    }
}

fn pheno_with_mcp(name: &str, servers: Vec<McpServerConfig>) -> Phenotype {
    Phenotype {
        name: name.into(),
        skills: vec![],
        model: None,
        persona: None,
        max_iterations: None,
        provider: None,
        mcp_servers: servers,
    }
}

#[test]
fn unbound_session_resolves_no_mcp_servers_by_default() {
    let state = AppState::new();
    let s = state.store.create_session(None);
    let resolved = state.resolve_mcp_servers(&s.id);
    let has_global = resolved.iter().any(|c| c.id == "github");
    assert!(
        !has_global,
        "without global mcp.json, no github server should be present"
    );
}

#[test]
fn global_mcp_json_feeds_resolution() {
    let state = AppState::new();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mcp.json");
    fs::write(&path, r#"{"mcpServers":{"github":{"command":"gh-mcp"}}}"#).unwrap();
    state.init_mcp_at(path);

    let s = state.store.create_session(None);
    let resolved = state.resolve_mcp_servers(&s.id);
    let github = resolved
        .iter()
        .find(|c| c.id == "github")
        .expect("github server should be present from global mcp.json");
    assert_eq!(github.scope, McpScope::Global);
}

#[test]
fn phenotype_mcp_servers_resolve_for_unbound_session() {
    let state = AppState::new();
    state.apply_phenotype(pheno_with_mcp(
        "codon",
        vec![mcp_cfg("codegraph", "codegraph", McpScope::Workspace)],
    ));
    let s = state.store.create_session(None);
    let resolved = state.resolve_mcp_servers(&s.id);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].id, "codegraph");
    assert_eq!(resolved[0].scope, McpScope::Workspace);
}

#[test]
fn phenotype_overrides_global_by_id_whole_record() {
    let state = AppState::new();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mcp.json");
    // Global declares codegraph as a global-scoped server.
    fs::write(
        &path,
        r#"{"mcpServers":{"codegraph":{"command":"old-codegraph"}}}"#,
    )
    .unwrap();
    state.init_mcp_at(path);
    // The phenotype's codegraph wins as a WHOLE record (command + scope), not a
    // field-level merge (RFC 0018 section 11.5).
    state.apply_phenotype(pheno_with_mcp(
        "codon",
        vec![mcp_cfg("codegraph", "codegraph", McpScope::Workspace)],
    ));

    let s = state.store.create_session(None);
    let resolved = state.resolve_mcp_servers(&s.id);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].command, "codegraph");
    assert_eq!(resolved[0].scope, McpScope::Workspace);
}

#[test]
fn session_tier_overrides_phenotype_by_id() {
    let state = AppState::new();
    state.apply_phenotype(pheno_with_mcp(
        "codon",
        vec![mcp_cfg("codegraph", "pheno-codegraph", McpScope::Workspace)],
    ));
    let s = state.store.create_session(None);
    state.store.set_session_mcp_servers(
        &s.id,
        Some(vec![mcp_cfg(
            "codegraph",
            "session-codegraph",
            McpScope::Workspace,
        )]),
    );

    let resolved = state.resolve_mcp_servers(&s.id);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].command, "session-codegraph");
}

#[test]
fn disabled_at_a_tier_suppresses_an_inherited_server() {
    let state = AppState::new();
    state.apply_phenotype(pheno_with_mcp(
        "codon",
        vec![mcp_cfg("codegraph", "codegraph", McpScope::Workspace)],
    ));
    let s = state.store.create_session(None);
    // The session suppresses the phenotype's codegraph for this turn.
    let mut off = mcp_cfg("codegraph", "codegraph", McpScope::Workspace);
    off.disabled = true;
    state.store.set_session_mcp_servers(&s.id, Some(vec![off]));

    assert!(state.resolve_mcp_servers(&s.id).is_empty());
}

#[test]
fn set_session_phenotype_validates_name() {
    let state = AppState::new();
    let s = state.store.create_session(None);
    let err = state
        .set_session_phenotype(&s.id, Some("not-a-real-pheno".into()))
        .unwrap_err();
    assert!(err.contains("unknown phenotype"), "{err}");
    // The rejected name must NOT have been written.
    assert!(state.store.session_phenotype(&s.id).is_none());
    // Clearing always succeeds.
    state.set_session_phenotype(&s.id, None).unwrap();
}

// An unbound session runs as the global default mode (#265). We compare against
// the live `default_mode()` rather than a literal so the test stays hermetic
// regardless of any persisted `mode.json` in the dev environment.
#[test]
fn unbound_session_inherits_default_mode() {
    let state = AppState::new();
    let s = state.store.create_session(None);
    assert_eq!(state.session_mode(&s.id), state.default_mode());
}

#[test]
fn bound_session_resolves_to_its_own_mode() {
    let state = AppState::new();
    let s = state.store.create_session(None);
    state.set_session_mode(&s.id, Some(Mode::Plan));
    assert_eq!(state.session_mode(&s.id), Mode::Plan);
    // Clearing the binding inherits the global default again.
    state.set_session_mode(&s.id, None);
    assert_eq!(state.session_mode(&s.id), state.default_mode());
}

#[test]
fn unknown_session_resolves_to_default_mode() {
    let state = AppState::new();
    assert_eq!(state.session_mode("ghost-session"), state.default_mode());
}

// Regression guard for the #246 review blocker: per-session phenotype binding
// must not silently disconnect the manual skill-activation palette. An UNBOUND
// session keeps the global active set (so palette toggles reach the turn); only
// an EXPLICITLY bound session swaps in its phenotype's declared skills.
#[test]
fn turn_active_skills_unbound_uses_palette_bound_uses_phenotype() {
    let state = AppState::new();
    // Simulate a command-palette activation: a skill toggled into the global
    // active set (bypassing the install-check guard, as the dedup test does).
    state
        .active_skills
        .lock()
        .unwrap()
        .insert("palette-skill".into());

    let s = state.store.create_session(None);

    // Unbound: the turn must still see the palette activation.
    assert!(
        state
            .turn_active_skills(&s.id)
            .contains(&"palette-skill".to_string()),
        "an unbound session must inherit the global palette-activated skills"
    );

    // Explicitly bound: the phenotype's declared skills govern the turn, so the
    // manually-activated palette skill drops out (no installed skills in tests).
    state
        .set_session_phenotype(&s.id, Some(DEFAULT_PHENOTYPE.into()))
        .unwrap();
    assert!(
        !state
            .turn_active_skills(&s.id)
            .contains(&"palette-skill".to_string()),
        "an explicitly bound session uses its phenotype's declared skills, not the palette"
    );
}

// Regression guard for issue #117: the supervisor actor is `tokio::spawn`'d, so
// `init_mcp` must establish a reactor context itself. This is intentionally a
// plain `#[test]` (no `#[tokio::test]`) to mirror Tauri's `setup`, which runs
// off-runtime on macOS — pre-fix this panicked with "there is no reactor running".
#[test]
fn migration_makes_saved_candle_provider_active_and_seeds_ollama_inactive() {
    let config = ProviderConfig {
        kind: ProviderKind::CandleVllm,
        base_url: Some("http://localhost:9001/v1".into()),
        model: "my-candle-model".into(),
        has_key: false,
        ..Default::default()
    };
    let reg = build_migrated_registry(config);
    assert_eq!(reg.active, "candle-vllm");
    let active = reg.active_connection().unwrap();
    assert_eq!(active.kind, ProviderKind::CandleVllm);
    assert_eq!(active.model, "my-candle-model");
    assert_eq!(active.base_url.as_deref(), Some("http://localhost:9001/v1"));
    // The other local vendor is seeded, keyless, and NOT active.
    let ollama = reg
        .connections
        .iter()
        .find(|c| c.kind == ProviderKind::Ollama)
        .unwrap();
    assert_ne!(reg.active, ollama.id);
    assert!(!ollama.has_key);
    assert_eq!(reg.connections.len(), 2);
}

#[test]
fn migration_makes_saved_ollama_provider_active_and_seeds_candle_inactive() {
    let config = ProviderConfig {
        kind: ProviderKind::Ollama,
        base_url: None,
        model: "qwen2.5".into(),
        has_key: false,
        ..Default::default()
    };
    let reg = build_migrated_registry(config);
    assert_eq!(reg.active, "ollama");
    assert_eq!(reg.active_connection().unwrap().model, "qwen2.5");
    assert!(reg
        .connections
        .iter()
        .any(|c| c.kind == ProviderKind::CandleVllm));
    assert_eq!(reg.connections.len(), 2);
}

#[test]
fn load_uses_existing_registry_file_over_legacy_config() {
    let tmp = tempfile::tempdir().unwrap();
    let reg_path = tmp.path().join("provider-registry.json");
    let cfg_path = tmp.path().join("provider.json");
    // A pre-#633 registry file (schema_version 0) with a single ollama
    // connection stored thinking-on -- the old universal default.
    let on_disk = ProviderRegistry {
        active: "ollama".into(),
        connections: vec![ProviderConnection {
            id: "ollama".into(),
            kind: ProviderKind::Ollama,
            display_name: "Ollama".into(),
            vendor: None,
            base_url: None,
            model: "saved".into(),
            has_key: false,
            secret_missing: false,
            thinking: true,
            reasoning_effort: ReasoningEffort::default(),
            reasoning_visibility: ReasoningVisibility::default(),
            warmup_enabled: true,
            num_ctx: None,
            region: None,
            auth_mode: None,
            aws_profile: None,
            access_key_id: None,
            compaction_model: None,
            compaction_budget: None,
        }],
        schema_version: 0,
    };
    fs::write(&reg_path, serde_json::to_string(&on_disk).unwrap()).unwrap();
    // A legacy config that must be ignored when the registry file exists.
    fs::write(
        &cfg_path,
        serde_json::to_string(&ProviderConfig::default()).unwrap(),
    )
    .unwrap();
    let loaded = load_or_migrate_registry_at(Some(reg_path), Some(cfg_path));
    // The registry file wins over the legacy config (model preserved)...
    assert_eq!(loaded.active, "ollama");
    assert_eq!(loaded.active_connection().unwrap().model, "saved");
    // ...and the #633 migration flips the local connection off + stamps the
    // current schema version.
    assert!(!loaded.active_connection().unwrap().thinking);
    assert_eq!(
        loaded.schema_version,
        ProviderRegistry::default().schema_version
    );
}

#[test]
fn load_migrates_when_only_legacy_config_present() {
    let tmp = tempfile::tempdir().unwrap();
    let reg_path = tmp.path().join("provider-registry.json");
    let cfg_path = tmp.path().join("provider.json");
    let config = ProviderConfig {
        kind: ProviderKind::Ollama,
        base_url: None,
        model: "legacy".into(),
        has_key: false,
        ..Default::default()
    };
    fs::write(&cfg_path, serde_json::to_string(&config).unwrap()).unwrap();
    let loaded = load_or_migrate_registry_at(Some(reg_path.clone()), Some(cfg_path));
    assert_eq!(loaded.active, "ollama");
    assert_eq!(loaded.active_connection().unwrap().model, "legacy");
    // #633: a migrated legacy local connection also lands thinking-off.
    assert!(!loaded.active_connection().unwrap().thinking);
    assert_eq!(
        loaded.schema_version,
        ProviderRegistry::default().schema_version
    );
    // Pure load: migration does not write the registry file (lazy persist).
    assert!(!reg_path.exists());
}

/// Regression (#487): a real on-disk registry holding a profile-auth Bedrock
/// connection -- captured verbatim from a user's `provider-registry.json` -- must
/// survive load with every Bedrock field intact. This exercises the exact bytes,
/// including the pre-#395 absence of `reasoningEffort` (must default to Medium) and
/// the camelCase Bedrock fields (`authMode`, `awsProfile`). Proves the persistence
/// layer never silently drops the connection on load.
#[test]
fn load_preserves_profile_auth_bedrock_connection_verbatim() {
    let tmp = tempfile::tempdir().unwrap();
    let reg_path = tmp.path().join("provider-registry.json");
    // Exact shape of the field-reported file (active Bedrock, profile auth, no key).
    let on_disk = r#"{
      "connections": [
        {
          "id": "ollama",
          "kind": "ollama",
          "displayName": "Ollama",
          "model": "llama3.2",
          "hasKey": false,
          "thinking": true,
          "supportsVision": false
        },
        {
          "id": "aws-bedrock",
          "kind": "bedrock",
          "displayName": "AWS Bedrock",
          "model": "us.anthropic.claude-opus-4-8",
          "hasKey": false,
          "thinking": false,
          "supportsVision": false,
          "region": "us-east-2",
          "authMode": "profile",
          "awsProfile": "bedrock-profile"
        }
      ],
      "active": "aws-bedrock"
    }"#;
    fs::write(&reg_path, on_disk).unwrap();

    let loaded = load_or_migrate_registry_at(Some(reg_path), None);

    // Neither connection is dropped, and Bedrock stays active.
    assert_eq!(
        loaded.connections.len(),
        2,
        "both connections must survive load"
    );
    assert_eq!(loaded.active, "aws-bedrock");

    let bedrock = loaded
        .connections
        .iter()
        .find(|c| c.id == "aws-bedrock")
        .expect("the Bedrock connection must not be dropped on load");
    assert_eq!(bedrock.kind, ProviderKind::Bedrock);
    assert_eq!(bedrock.region.as_deref(), Some("us-east-2"));
    assert_eq!(bedrock.auth_mode, Some(BedrockAuth::Profile));
    assert_eq!(bedrock.aws_profile.as_deref(), Some("bedrock-profile"));
    assert_eq!(bedrock.model, "us.anthropic.claude-opus-4-8");
    // A file written before #395 carries no reasoningEffort -> defaults to Medium.
    assert_eq!(bedrock.reasoning_effort, ReasoningEffort::default());
}

async fn siliconflow_connection_body(
    mut conn: ProviderConnection,
    thinking: bool,
) -> serde_json::Value {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("data: [DONE]\n\n"))
        .mount(&server)
        .await;

    conn.base_url = Some(server.uri());
    let model = conn.model.clone();
    let provider = super::build_provider(&conn, &conn.model);
    let req = ChatRequest {
        model,
        messages: vec![ChatMessage::text("user", "hi")],
        tools: Vec::new(),
        thinking,
        max_tokens: None,
        cache_messages: false,
    };
    let _ = provider.chat_stream(req).await.expect("send succeeds");
    let reqs = server.received_requests().await.expect("requests recorded");
    serde_json::from_slice(&reqs[0].body).expect("body is json")
}

fn siliconflow_conn(id: &str, effort: ReasoningEffort) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        kind: ProviderKind::SiliconFlow,
        display_name: "SiliconFlow".into(),
        vendor: None,
        base_url: None,
        model: "zai-org/GLM-5.2".into(),
        has_key: false,
        secret_missing: false,
        thinking: true,
        reasoning_effort: effort,
        reasoning_visibility: ReasoningVisibility::default(),
        warmup_enabled: true,
        num_ctx: None,
        region: None,
        auth_mode: None,
        aws_profile: None,
        access_key_id: None,
        compaction_model: None,
        compaction_budget: None,
    }
}

#[tokio::test]
async fn build_provider_threads_high_connection_effort_to_siliconflow_request() {
    let body = siliconflow_connection_body(
        siliconflow_conn("sf-high-effort", ReasoningEffort::High),
        true,
    )
    .await;

    assert_eq!(body["thinking_budget"], 8192);
    assert!(body.get("enable_thinking").is_none());
}

#[tokio::test]
async fn legacy_connection_without_reasoning_effort_emits_medium_budget() {
    let legacy = r#"{
      "id": "sf-legacy-effort",
      "kind": "siliconFlow",
      "displayName": "SiliconFlow",
      "model": "zai-org/GLM-5.2",
      "hasKey": false,
      "thinking": true,
      "supportsVision": false
    }"#;
    let conn: ProviderConnection = serde_json::from_str(legacy).unwrap();
    assert_eq!(conn.reasoning_effort, ReasoningEffort::Medium);

    let body = siliconflow_connection_body(conn, true).await;
    assert_eq!(body["thinking_budget"], 4096);
}

/// Editing one connection (the legacy active-config shim, or a per-connection
/// upsert) must never drop a sibling -- the failure mode behind "my Bedrock
/// connection vanished after I touched Ollama".
#[test]
fn editing_one_connection_preserves_siblings() {
    let mut reg = ProviderRegistry {
        active: "aws-bedrock".into(),
        connections: vec![
            ProviderConnection {
                id: "ollama".into(),
                kind: ProviderKind::Ollama,
                display_name: "Ollama".into(),
                vendor: None,
                base_url: None,
                model: "llama3.2".into(),
                has_key: false,
                secret_missing: false,
                thinking: true,
                reasoning_effort: ReasoningEffort::default(),
                reasoning_visibility: ReasoningVisibility::default(),
                warmup_enabled: true,
                num_ctx: None,
                region: None,
                auth_mode: None,
                aws_profile: None,
                access_key_id: None,
                compaction_model: None,
                compaction_budget: None,
            },
            ProviderConnection {
                id: "aws-bedrock".into(),
                kind: ProviderKind::Bedrock,
                display_name: "AWS Bedrock".into(),
                vendor: None,
                base_url: None,
                model: "us.anthropic.claude-opus-4-8".into(),
                has_key: false,
                secret_missing: false,
                thinking: false,
                reasoning_effort: ReasoningEffort::default(),
                reasoning_visibility: ReasoningVisibility::default(),
                warmup_enabled: true,
                num_ctx: None,
                region: Some("us-east-2".into()),
                auth_mode: Some(BedrockAuth::Profile),
                aws_profile: Some("bedrock-profile".into()),
                access_key_id: None,
                compaction_model: None,
                compaction_budget: None,
            },
        ],
        schema_version: 0,
    };
    // Per-connection upsert of an existing id edits in place, keeps the sibling.
    reg.upsert(ProviderConnection {
        model: "llama3.3".into(),
        ..reg.connections[0].clone()
    });
    assert_eq!(
        reg.connections.len(),
        2,
        "upsert must not drop the Bedrock sibling"
    );
    assert!(reg.connections.iter().any(|c| c.id == "aws-bedrock"));
    assert_eq!(reg.connections[0].model, "llama3.3");
}

#[test]
fn load_falls_back_to_default_when_no_files() {
    let tmp = tempfile::tempdir().unwrap();
    let loaded = load_or_migrate_registry_at(
        Some(tmp.path().join("provider-registry.json")),
        Some(tmp.path().join("provider.json")),
    );
    assert_eq!(loaded, ProviderRegistry::default());
}

#[test]
fn corrupt_registry_is_quarantined_not_overwritten() {
    let tmp = tempfile::tempdir().unwrap();
    let reg_path = tmp.path().join("provider-registry.json");
    // A truncated / half-written file (the crash-mid-write failure mode).
    fs::write(&reg_path, "{ \"connections\": [ {").unwrap();

    let loaded = load_or_migrate_registry_at(Some(reg_path.clone()), None);

    // A corrupt file seeds a clean default rather than silently nuking config.
    assert_eq!(loaded, ProviderRegistry::default());
    // The unreadable bytes are preserved in a sibling, never destroyed...
    let preserved: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("provider-registry.corrupt-")
        })
        .collect();
    assert_eq!(
        preserved.len(),
        1,
        "corrupt file should be quarantined once"
    );
    // ...and the original path is moved aside (so the next save starts clean).
    assert!(!reg_path.exists());
}

#[test]
fn corrupt_registry_does_not_trigger_legacy_migration() {
    let tmp = tempfile::tempdir().unwrap();
    let reg_path = tmp.path().join("provider-registry.json");
    let cfg_path = tmp.path().join("provider.json");
    fs::write(&reg_path, "not json at all").unwrap();
    // A legacy config exists, but a *corrupt* registry must not re-migrate it
    // over the user's (now quarantined) active registry — seed default instead.
    fs::write(
        &cfg_path,
        serde_json::to_string(&ProviderConfig {
            kind: ProviderKind::Ollama,
            model: "legacy".into(),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();

    let loaded = load_or_migrate_registry_at(Some(reg_path), Some(cfg_path));
    assert_eq!(loaded, ProviderRegistry::default());
}

#[test]
fn config_path_resolvers_are_isolated_under_test() {
    // Every config-FILE resolver must resolve to None under `cfg!(test)` so the
    // suite can never read or clobber the developer's real provider registry /
    // phenotype / mode (the clobber half of #811). The store builders
    // (`build_session_store` etc.) are isolated separately at the builder level.
    assert!(config_path().is_none());
    assert!(registry_path().is_none());
    assert!(active_phenotype_path().is_none());
    assert!(default_mode_path().is_none());
    assert!(search_config_path().is_none());
    assert!(control_config_path().is_none());
    assert!(tool_permissions_path().is_none());
    assert!(permission_matrix_path().is_none());
    // The persistence round-trips are therefore no-ops in tests, not real writes.
    assert!(load_active_phenotype_name().is_none());
    assert_eq!(load_default_mode(), Mode::Auto);
}

#[test]
fn partially_corrupt_registry_salvages_good_connection() {
    let tmp = tempfile::tempdir().unwrap();
    let reg_path = tmp.path().join("provider-registry.json");
    // A registry whose active connection carries a future/unknown `kind` this
    // build cannot deserialize, alongside a valid Bedrock connection. The user's
    // Bedrock config must survive rather than the whole file wiping to Candle.
    fs::write(
        &reg_path,
        r#"{"active":"future","connections":[
            {"id":"future","kind":"gemini","displayName":"Gemini","model":"g","hasKey":true},
            {"id":"bedrock-opus","kind":"bedrock","displayName":"AWS Bedrock","model":"m","hasKey":false}
        ]}"#,
    )
    .unwrap();

    let loaded = load_or_migrate_registry_at(Some(reg_path.clone()), None);

    assert_ne!(
        loaded,
        ProviderRegistry::default(),
        "a partially-bad registry must not wipe to the Candle default"
    );
    assert_eq!(loaded.connections.len(), 1);
    assert_eq!(loaded.connections[0].id, "bedrock-opus");
    assert_eq!(loaded.active, "bedrock-opus");
    // A salvaged registry is a clean load, not a quarantine.
    assert!(reg_path.exists());
}

#[test]
fn load_recovers_from_newest_valid_backup_among_corrupt_baks() {
    let tmp = tempfile::tempdir().unwrap();
    let reg_path = tmp.path().join("provider-registry.json");
    fs::write(&reg_path, "corrupt json").unwrap();

    fs::write(
        tmp.path().join("provider-registry.1000.bak"),
        r#"{"active":"future","connections":[
            {"id":"future","kind":"gemini","displayName":"Gemini","model":"g","hasKey":true}
        ]}"#,
    )
    .unwrap();

    fs::write(
        tmp.path().join("provider-registry.2000.bak"),
        r#"{"active":"middle","connections":[
            {"id":"middle","kind":"ollama","displayName":"Ollama","model":"llama3","hasKey":false}
        ]}"#,
    )
    .unwrap();

    fs::write(
        tmp.path().join("provider-registry.3000.bak"),
        "also corrupt",
    )
    .unwrap();

    let loaded = load_or_migrate_registry_at(Some(reg_path.clone()), None);

    assert_eq!(loaded.active, "middle");
    assert_eq!(loaded.connections.len(), 1);
    assert_eq!(loaded.connections[0].model, "llama3");
}

#[test]
fn load_falls_back_to_default_when_all_baks_corrupt() {
    let tmp = tempfile::tempdir().unwrap();
    let reg_path = tmp.path().join("provider-registry.json");
    fs::write(&reg_path, "corrupt json").unwrap();

    fs::write(
        tmp.path().join("provider-registry.1000.bak"),
        "first corrupt",
    )
    .unwrap();
    fs::write(
        tmp.path().join("provider-registry.2000.bak"),
        "second corrupt",
    )
    .unwrap();

    let loaded = load_or_migrate_registry_at(Some(reg_path.clone()), None);

    assert_eq!(loaded, ProviderRegistry::default());
}

#[test]
fn load_recovers_from_backup_when_registry_completely_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let reg_path = tmp.path().join("provider-registry.json");

    fs::write(
        tmp.path().join("provider-registry.1000.bak"),
        r#"{"active":"backup","connections":[
            {"id":"backup","kind":"openai","displayName":"OpenAI","model":"gpt-4","hasKey":true}
        ]}"#,
    )
    .unwrap();

    let loaded = load_or_migrate_registry_at(Some(reg_path), None);

    assert_eq!(loaded.active, "backup");
    assert_eq!(loaded.connections.len(), 1);
    assert_eq!(loaded.connections[0].kind, ff_core::ProviderKind::OpenAi);
}

#[test]
fn write_atomic_replaces_existing_and_leaves_no_tmp() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("provider-registry.json");
    write_atomic(&path, "first").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "first");
    // A second write fully replaces the contents (no append / partial state)...
    write_atomic(&path, "second").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "second");
    // ...and never leaves the intermediate .tmp file behind.
    assert!(!tmp.path().join("provider-registry.tmp").exists());
}

#[test]
fn config_to_connection_uses_kind_slug_and_label() {
    let conn = config_to_connection(ProviderConfig {
        kind: ProviderKind::Ollama,
        base_url: None,
        model: "solo".into(),
        has_key: false,
        thinking: true,
        ..Default::default()
    });
    assert_eq!(conn.id, "ollama");
    assert_eq!(conn.display_name, "Ollama");
    assert_eq!(conn.model, "solo");
    // Legacy shim projects it back to a ProviderConfig faithfully.
    let cfg = connection_to_config(&conn);
    assert_eq!(cfg.kind, ProviderKind::Ollama);
    assert_eq!(cfg.model, "solo");
}

#[test]
fn set_provider_config_shim_mutates_active_connection_in_place() {
    let state = AppState::with_registry(ProviderRegistry::default());
    state.set_provider_config(ProviderConfig {
        kind: ProviderKind::CandleVllm,
        base_url: Some("http://localhost:9100/v1".into()),
        model: "edited".into(),
        has_key: false,
        thinking: true,
        ..Default::default()
    });
    let reg = state.provider_registry();
    // No new connection; the active one is edited in place.
    assert_eq!(reg.connections.len(), 2);
    let active = reg.active_connection().unwrap();
    assert_eq!(active.id, "candle-vllm");
    assert_eq!(active.model, "edited");
    assert_eq!(active.base_url.as_deref(), Some("http://localhost:9100/v1"));
}

#[test]
fn session_root_defaults_to_workspace_root_when_unset() {
    let state = AppState::new();
    assert_eq!(state.session_root("sess-unset"), state.workspace_root);
}

#[test]
fn session_root_returns_set_cwd() {
    let state = AppState::new();
    let sess = state.store.create_session(None);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    state.set_session_cwd(&sess.id, path.clone());
    assert_eq!(state.session_root(&sess.id), path);
}

#[test]
fn session_cwd_persists_across_a_fresh_state_over_the_same_db() {
    // #279: the cwd lives in the session row, so a restart (a new store over
    // the same db file) restores it rather than dropping it.
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("sessions.db");
    let work = tempfile::tempdir().unwrap();
    let work_path = work.path().to_path_buf();

    let sess_id = {
        let store = ff_session::SessionStore::open(&db_path).unwrap();
        let s = store.create_session(None);
        store.set_session_workspace(&s.id, Some(work_path.display().to_string()));
        s.id
    };
    // A fresh store over the same file == an app restart.
    let store = ff_session::SessionStore::open(&db_path).unwrap();
    assert_eq!(
        store.session_workspace(&sess_id),
        Some(work_path.display().to_string())
    );
}

#[test]
fn default_workspace_root_creates_flowforge_workspaces() {
    let home = tempfile::tempdir().unwrap();
    let root = default_workspace_root_in(Some(home.path().to_path_buf()));
    assert_eq!(root, home.path().join(".flowforge").join("workspaces"));
    assert!(root.is_dir());
}

#[test]
fn default_workspace_root_falls_back_to_cwd_without_home() {
    let root = default_workspace_root_in(None);
    assert_eq!(root, std::env::current_dir().unwrap());
}

#[test]
fn session_cwd_is_isolated_per_session() {
    let state = AppState::new();
    let sess_a = state.store.create_session(None);
    let sess_b = state.store.create_session(None);
    let a = tempfile::tempdir().unwrap();
    let a_path = a.path().to_path_buf();
    state.set_session_cwd(&sess_a.id, a_path.clone());
    assert_eq!(state.session_root(&sess_a.id), a_path);
    // A different session is unaffected and still falls back to the default.
    assert_eq!(state.session_root(&sess_b.id), state.workspace_root);
}

#[test]
fn set_active_connection_rejects_unknown_id() {
    let state = AppState::with_registry(ProviderRegistry::default());
    assert!(state.set_active_connection("ghost").is_err());
    assert_eq!(state.provider_registry().active, "candle-vllm");
    state.set_active_connection("ollama").unwrap();
    assert_eq!(state.provider_registry().active, "ollama");
}

#[test]
fn upsert_and_remove_connection_round_trip() {
    let state = AppState::with_registry(ProviderRegistry::default());
    let stored = state.upsert_connection(ProviderConnection {
        id: String::new(),
        kind: ProviderKind::CandleVllm,
        display_name: "OpenRouter".into(),
        vendor: Some("openrouter".into()),
        base_url: Some("https://openrouter.ai/api/v1".into()),
        model: "x".into(),
        has_key: false,
        secret_missing: false,
        thinking: true,
        reasoning_effort: ReasoningEffort::default(),
        reasoning_visibility: ReasoningVisibility::default(),
        warmup_enabled: true,
        num_ctx: None,
        region: None,
        auth_mode: None,
        aws_profile: None,
        access_key_id: None,
        compaction_model: None,
        compaction_budget: None,
    });
    assert_eq!(stored.id, "openrouter");
    assert_eq!(state.provider_registry().connections.len(), 3);
    state.remove_connection("openrouter").unwrap();
    assert_eq!(state.provider_registry().connections.len(), 2);
}

fn has_key_of(state: &AppState, id: &str) -> bool {
    state
        .provider_registry()
        .connections
        .into_iter()
        .find(|c| c.id == id)
        .unwrap()
        .has_key
}

#[test]
fn set_connection_secret_flips_has_key_without_leaking_value() {
    let state = AppState::with_registry(ProviderRegistry::default());
    let id = "candle-vllm";
    assert!(!has_key_of(&state, id));
    state
        .set_connection_secret(id, SecretKind::ApiKey, "sk-secret-aaa")
        .unwrap();
    assert!(has_key_of(&state, id));
    // The value lands in the keychain (MemStore under cfg(test))...
    assert_eq!(
        crate::secrets::get(id, SecretKind::ApiKey).as_deref(),
        Some("sk-secret-aaa")
    );
    // ...but never in the registry the frontend receives.
    let json = serde_json::to_string(&state.provider_registry()).unwrap();
    assert!(!json.contains("sk-secret-aaa"));
}

#[test]
fn set_connection_secret_unknown_id_errors_and_writes_nothing() {
    let state = AppState::with_registry(ProviderRegistry::default());
    let err = state
        .set_connection_secret("nonexistent-xyz", SecretKind::ApiKey, "nope")
        .unwrap_err();
    assert!(err.contains("nonexistent-xyz"));
    assert!(crate::secrets::get("nonexistent-xyz", SecretKind::ApiKey).is_none());
}

#[test]
fn clear_connection_secret_recomputes_has_key() {
    let state = AppState::with_registry(ProviderRegistry::default());
    let id = "ollama";
    state
        .set_connection_secret(id, SecretKind::SecretAccessKey, "aws-secret")
        .unwrap();
    state
        .set_connection_secret(id, SecretKind::SessionToken, "aws-token")
        .unwrap();
    assert!(has_key_of(&state, id));
    // One of two secrets remains => has_key stays true.
    state
        .clear_connection_secret(id, SecretKind::SessionToken)
        .unwrap();
    assert!(has_key_of(&state, id));
    // Last secret cleared => has_key flips false.
    state
        .clear_connection_secret(id, SecretKind::SecretAccessKey)
        .unwrap();
    assert!(!has_key_of(&state, id));
}

// ---- #311 PR-2: OpenAI connection secret lifecycle ----

/// A hosted-OpenAI connection (keychain `ApiKey`, no AWS fields). Unique id so
/// the process-global MemStore keychain stays isolated across parallel tests.
fn openai_conn(id: &str) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        kind: ProviderKind::OpenAi,
        display_name: "OpenAI".into(),
        vendor: None,
        base_url: None,
        model: "gpt-4o".into(),
        has_key: false,
        secret_missing: false,
        thinking: false,
        reasoning_effort: ReasoningEffort::default(),
        reasoning_visibility: ReasoningVisibility::default(),
        warmup_enabled: true,
        num_ctx: None,
        region: None,
        auth_mode: None,
        aws_profile: None,
        access_key_id: None,
        compaction_model: None,
        compaction_budget: None,
    }
}

#[test]
fn openai_connection_api_key_lifecycle() {
    // Proves PR-1's "works end-to-end once a key is stored" claim for the
    // OpenAI kind: the generic ApiKey plumbing flips has_key, keeps the value
    // out of the registry, reports presence, and clears cleanly.
    let id = "openai-lifecycle";
    let state = AppState::with_registry(ProviderRegistry::default());
    state.upsert_connection(openai_conn(id));

    assert!(!has_key_of(&state, id));
    assert!(state.connection_secret_presence(id).unwrap().is_empty());

    state
        .set_connection_secret(id, SecretKind::ApiKey, "sk-openai-xyz")
        .unwrap();

    assert!(has_key_of(&state, id));
    assert_eq!(
        crate::secrets::get(id, SecretKind::ApiKey).as_deref(),
        Some("sk-openai-xyz")
    );
    assert_eq!(
        state.connection_secret_presence(id).unwrap(),
        vec![SecretKind::ApiKey]
    );
    // The secret value never enters the registry the frontend receives.
    let json = serde_json::to_string(&state.provider_registry()).unwrap();
    assert!(!json.contains("sk-openai-xyz"));

    state
        .clear_connection_secret(id, SecretKind::ApiKey)
        .unwrap();
    assert!(!has_key_of(&state, id));
    assert!(state.connection_secret_presence(id).unwrap().is_empty());
}

// ---- #320: per-kind presence + Auto auth resolution ----

/// Build + insert a Bedrock connection with the given id and auth mode, leaving
/// secret material to the caller (keychain is a process-global MemStore in tests,
/// so each test uses a unique id to stay isolated).
fn bedrock_conn(id: &str, auth_mode: Option<BedrockAuth>) -> ProviderConnection {
    ProviderConnection {
        id: id.into(),
        kind: ProviderKind::Bedrock,
        display_name: "Amazon Bedrock".into(),
        vendor: None,
        base_url: None,
        model: "anthropic.claude-3-5-sonnet".into(),
        has_key: false,
        secret_missing: false,
        thinking: false,
        reasoning_effort: ReasoningEffort::default(),
        reasoning_visibility: ReasoningVisibility::default(),
        warmup_enabled: true,
        num_ctx: None,
        region: Some("us-east-1".into()),
        auth_mode,
        aws_profile: None,
        access_key_id: None,
        compaction_model: None,
        compaction_budget: None,
    }
}

#[test]
fn connection_secret_presence_lists_stored_kinds_and_errors_on_unknown_id() {
    let state = AppState::with_registry(ProviderRegistry::default());
    // Unique, self-registered id: the keychain MemStore is process-global in
    // tests, so sharing a default connection id (e.g. `candle-vllm`) with
    // another secret-storing test makes the initial emptiness assertion
    // order-dependent.
    let id = "secret-presence-listing";
    state.upsert_connection(bedrock_conn(id, None));
    assert!(state.connection_secret_presence(id).unwrap().is_empty());
    state
        .set_connection_secret(id, SecretKind::ApiKey, "sk-1")
        .unwrap();
    state
        .set_connection_secret(id, SecretKind::SessionToken, "tok-1")
        .unwrap();
    assert_eq!(
        state.connection_secret_presence(id).unwrap(),
        vec![SecretKind::ApiKey, SecretKind::SessionToken]
    );
    assert!(state.connection_secret_presence("ghost").is_err());
}

#[test]
fn resolved_auth_auto_prefers_api_key_over_iam_keys() {
    let id = "bedrock-auto-api-wins";
    let state = AppState::with_registry(ProviderRegistry::default());
    let mut conn = bedrock_conn(id, Some(BedrockAuth::Auto));
    conn.access_key_id = Some("AKIA...".into());
    state.upsert_connection(conn);
    // Both an IAM secret and an API key are stored => API key wins.
    state
        .set_connection_secret(id, SecretKind::SecretAccessKey, "iam-secret")
        .unwrap();
    state
        .set_connection_secret(id, SecretKind::ApiKey, "br-bearer")
        .unwrap();
    assert_eq!(state.resolved_bedrock_auth(id), Some(BedrockAuth::ApiKey));
}

#[test]
fn resolved_auth_auto_prefers_profile_over_iam_keys() {
    let id = "bedrock-auto-profile-wins";
    let state = AppState::with_registry(ProviderRegistry::default());
    let mut conn = bedrock_conn(id, Some(BedrockAuth::Auto));
    conn.aws_profile = Some("dev".into());
    conn.access_key_id = Some("AKIA...".into());
    state.upsert_connection(conn);
    state
        .set_connection_secret(id, SecretKind::SecretAccessKey, "iam-secret")
        .unwrap();
    // No API key => profile beats IAM keys.
    assert_eq!(state.resolved_bedrock_auth(id), Some(BedrockAuth::Profile));
}

#[test]
fn resolved_auth_auto_falls_back_to_iam_keys_then_profile() {
    let id = "bedrock-auto-iam-only";
    let state = AppState::with_registry(ProviderRegistry::default());
    let mut conn = bedrock_conn(id, Some(BedrockAuth::Auto));
    conn.access_key_id = Some("AKIA...".into());
    state.upsert_connection(conn);
    state
        .set_connection_secret(id, SecretKind::SecretAccessKey, "iam-secret")
        .unwrap();
    // Only IAM keys configured.
    assert_eq!(state.resolved_bedrock_auth(id), Some(BedrockAuth::IamKeys));
    // Nothing configured at all => Profile fallback so the probe surfaces it.
    let bare = "bedrock-auto-bare";
    state.upsert_connection(bedrock_conn(bare, Some(BedrockAuth::Auto)));
    assert_eq!(
        state.resolved_bedrock_auth(bare),
        Some(BedrockAuth::Profile)
    );
}

#[test]
fn resolved_auth_explicit_pin_wins_over_auto_preference() {
    let id = "bedrock-pinned-iam";
    let state = AppState::with_registry(ProviderRegistry::default());
    let mut conn = bedrock_conn(id, Some(BedrockAuth::IamKeys));
    conn.access_key_id = Some("AKIA...".into());
    state.upsert_connection(conn);
    state
        .set_connection_secret(id, SecretKind::SecretAccessKey, "iam-secret")
        .unwrap();
    // An API key is ALSO stored, but the explicit IamKeys pin is honored.
    state
        .set_connection_secret(id, SecretKind::ApiKey, "br-bearer")
        .unwrap();
    assert_eq!(state.resolved_bedrock_auth(id), Some(BedrockAuth::IamKeys));
}

#[test]
fn resolved_auth_legacy_none_defaults_to_auto() {
    // A pre-#320 Bedrock connection persisted with auth_mode: None and only a
    // profile resolves to Profile under the new Auto default -> no regression.
    let id = "bedrock-legacy-none";
    let state = AppState::with_registry(ProviderRegistry::default());
    let mut conn = bedrock_conn(id, None);
    conn.aws_profile = Some("default".into());
    state.upsert_connection(conn);
    assert_eq!(state.resolved_bedrock_auth(id), Some(BedrockAuth::Profile));
}

#[test]
fn resolved_auth_is_none_for_non_bedrock_or_unknown() {
    let state = AppState::with_registry(ProviderRegistry::default());
    assert_eq!(state.resolved_bedrock_auth("candle-vllm"), None);
    assert_eq!(state.resolved_bedrock_auth("ghost"), None);
}

#[test]
fn init_mcp_spawns_supervisor_without_an_entered_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    let state = AppState::new();
    // Parent dir exists (tempdir); mcp.json is absent => empty config, but the
    // supervisor still spawns. Exercises the exact path that aborted at boot.
    state.init_mcp_at(tmp.path().join("mcp.json"));
    assert!(
        state.mcp_handle().is_some(),
        "supervisor must spawn (and not panic) when init runs off-runtime"
    );
}

#[test]
fn init_mcp_captures_config_path_for_write_back() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mcp.json");
    let state = AppState::new();
    assert!(
        state.mcp_config_path().is_none(),
        "path is None before init"
    );
    state.init_mcp_at(path.clone());
    assert_eq!(
        state.mcp_config_path(),
        Some(path),
        "control commands must write back to the watched file"
    );
}

#[test]
fn start_process_reaper_spawns_without_an_entered_runtime() {
    // Same off-reactor `setup` path as `init_mcp` (issue #117): the method
    // must enter the runtime itself so a caller with no entered reactor
    // doesn't panic. The loop is detached and dies with the test process.
    let state = AppState::new();
    state.start_process_reaper();
    // No panic == the spawn succeeded.
}

#[test]
fn reap_session_processes_spawns_without_an_entered_runtime() {
    // #471: `delete_session` is a synchronous Tauri command, which runs off the
    // reactor on macOS (issue #117). `reap_session_processes` must therefore use
    // a reactor-safe spawn -- a bare `tokio::spawn` here panics with "no reactor
    // running" and the unwind through the command FFI takes the whole app down.
    // Plain `#[test]` (no `#[tokio::test]`) so there is no ambient runtime.
    let state = AppState::new();
    state.reap_session_processes("any-session");
    // No panic == the spawn succeeded.
}

// --- Four-option approval tests (#229) ---

#[test]
fn session_approved_scoped_by_session_and_tool() {
    let mut reg = ApprovalRegistry::default();
    reg.set_session_approve("s1", "t1");
    assert!(reg.is_session_approved("s1", "t1"));
    assert!(!reg.is_session_approved("s2", "t1"));
    assert!(!reg.is_session_approved("s1", "t2"));
}

#[test]
fn turn_cancel_keeps_session_approved_but_delete_clears_it() {
    let state = AppState::new();
    arm(&state, "s1");
    arm(&state, "s2");
    state.set_session_approve("s1", "bash");
    state.set_session_approve("s2", "bash");

    // Turn cancel (Stop button) must NOT revoke "Allow this session" (#229).
    state.cancel_pending_approvals("s1");
    assert!(state.is_session_approved("s1", "bash"));
    assert!(state.is_session_approved("s2", "bash"));

    // Session delete is the only thing that clears the session allowlist,
    // and only for the targeted session.
    state.clear_session_approvals("s1");
    assert!(!state.is_session_approved("s1", "bash"));
    assert!(state.is_session_approved("s2", "bash"));
}

#[test]
fn allowlist_never_covers_dangerous() {
    let state = AppState::new();
    arm(&state, "s1");
    // Session grant (in-memory only; avoids touching the real persisted file).
    state.set_session_approve("s1", "bash");

    // Write / ReadOnly: the grant pre-approves.
    assert!(state.allowlist_covers("s1", "bash", Safety::Write));
    assert!(state.allowlist_covers("s1", "bash", Safety::ReadOnly));

    // Dangerous: always re-prompts despite the grant.
    assert!(!state.allowlist_covers("s1", "bash", Safety::Dangerous));

    // An ungranted tool is never covered regardless of safety.
    assert!(!state.allowlist_covers("s1", "ungranted_tool_xyz", Safety::Write));
}

#[test]
fn always_approved_set_remove_list() {
    let mut reg = ApprovalRegistry::default();
    reg.set_always_approve("read_file");
    reg.set_always_approve("write_file");
    assert!(reg.is_always_approved("read_file"));
    assert!(!reg.is_always_approved("exec"));
    assert_eq!(reg.list_always_approved(), vec!["read_file", "write_file"]);
    reg.remove_always_approve("read_file");
    assert!(!reg.is_always_approved("read_file"));
    assert_eq!(reg.list_always_approved(), vec!["write_file"]);
}

#[test]
fn always_approved_save_load_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("tool_permissions.json");
    let mut set = HashSet::new();
    set.insert("bash".to_string());
    set.insert("read_file".to_string());
    ApprovalRegistry::save_always_approved(&path, &set).unwrap();
    let loaded = ApprovalRegistry::load_always_approved(&path);
    assert_eq!(loaded, set);
}

#[test]
fn load_always_approved_missing_file_returns_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("nonexistent.json");
    let loaded = ApprovalRegistry::load_always_approved(&path);
    assert!(loaded.is_empty());
}

#[test]
fn seed_builtin_content_writes_codon_and_codegraph_when_absent() {
    let phenos = tempfile::tempdir().unwrap();
    let skills = tempfile::tempdir().unwrap();
    seed_builtin_content_at(phenos.path(), skills.path(), None);

    // The Codon phenotype landed and parses through the real loader.
    let codon = phenos.path().join("codon.toml");
    assert!(codon.exists(), "codon.toml must be seeded");
    let (map, errors) = load_phenotypes(phenos.path());
    assert!(
        errors.is_empty(),
        "seeded codon.toml must parse: {errors:?}"
    );
    let pheno = map.get("codon").expect("codon phenotype present");
    assert!(
        pheno.skills.iter().any(|s| s == "codegraph"),
        "codon must declare the codegraph skill"
    );

    // The codegraph skill body landed in the layout the registry scans and parses.
    let skill_md = skills.path().join("codegraph").join("SKILL.md");
    assert!(skill_md.exists(), "codegraph SKILL.md must be seeded");
    let (registry, errors) = SkillRegistry::load_dir(skills.path());
    assert!(errors.is_empty(), "seeded SKILL.md must parse: {errors:?}");
    assert!(
        registry.get("codegraph").is_some(),
        "codegraph skill must be loadable so resolve_skills keeps it"
    );
}

#[test]
fn seed_builtin_content_does_not_clobber_user_edits() {
    let phenos = tempfile::tempdir().unwrap();
    let skills = tempfile::tempdir().unwrap();
    let codon = phenos.path().join("codon.toml");
    fs::write(&codon, "# user-edited\nskills = []\n").unwrap();

    seed_builtin_content_at(phenos.path(), skills.path(), None);

    assert_eq!(
        fs::read_to_string(&codon).unwrap(),
        "# user-edited\nskills = []\n",
        "an existing phenotype must never be overwritten"
    );
    // The absent codegraph skill is still seeded alongside the kept edit.
    assert!(skills.path().join("codegraph").join("SKILL.md").exists());
}

#[test]
fn seed_builtin_content_is_idempotent() {
    let phenos = tempfile::tempdir().unwrap();
    let skills = tempfile::tempdir().unwrap();
    seed_builtin_content_at(phenos.path(), skills.path(), None);
    let first = fs::read_to_string(phenos.path().join("codon.toml")).unwrap();

    seed_builtin_content_at(phenos.path(), skills.path(), None);
    let second = fs::read_to_string(phenos.path().join("codon.toml")).unwrap();

    assert_eq!(first, second, "a second seed run must be a no-op");
}

#[test]
fn seed_gate_runs_and_stamps_when_no_stamp() {
    let root = tempfile::tempdir().unwrap();
    let phenos = root.path().join("phenos");
    let skills = root.path().join("skills");
    let stamp = root.path().join(".seed_version");

    seed_builtin_content_gated(Some(&stamp), &phenos, &skills, None);

    assert!(
        phenos.join("codon.toml").exists(),
        "codon.toml must be seeded"
    );
    assert!(
        skills.join("codegraph").join("SKILL.md").exists(),
        "codegraph SKILL.md must be seeded"
    );
    assert!(stamp.exists(), "a successful pass must write the stamp");
    assert_eq!(
        fs::read_to_string(&stamp).unwrap(),
        format!("{:016x}\n", SEED_FINGERPRINT),
        "the stamp must hold the current fingerprint"
    );
}

#[test]
fn seed_gate_skips_the_whole_pass_when_stamp_matches() {
    let root = tempfile::tempdir().unwrap();
    let phenos = root.path().join("phenos");
    let skills = root.path().join("skills");
    let stamp = root.path().join(".seed_version");
    // Pre-write a matching stamp, as a prior successful launch would have.
    fs::write(&stamp, format!("{:016x}\n", SEED_FINGERPRINT)).unwrap();

    seed_builtin_content_gated(Some(&stamp), &phenos, &skills, None);

    // The pass was skipped: neither seed target nor its parent dirs were
    // created (no exists()/stat calls touched the phenos/skills trees).
    assert!(
        !phenos.exists(),
        "a matching stamp must skip the whole pass, never creating phenos/"
    );
    assert!(
        !skills.exists(),
        "skills/ must not be created on a skipped pass"
    );
    // A skipped pass must not rewrite the stamp.
    assert_eq!(
        fs::read_to_string(&stamp).unwrap(),
        format!("{:016x}\n", SEED_FINGERPRINT)
    );
}

#[test]
fn seed_gate_re_runs_when_stamp_is_stale() {
    let root = tempfile::tempdir().unwrap();
    let phenos = root.path().join("phenos");
    let skills = root.path().join("skills");
    let stamp = root.path().join(".seed_version");
    fs::write(&stamp, "deadbeef\n").unwrap();

    seed_builtin_content_gated(Some(&stamp), &phenos, &skills, None);

    // A stale (wrong-version) stamp must re-run the pass and refresh it.
    assert!(
        phenos.join("codon.toml").exists(),
        "stale stamp must re-run the seed"
    );
    assert!(skills.join("codegraph").join("SKILL.md").exists());
    assert_eq!(
        fs::read_to_string(&stamp).unwrap(),
        format!("{:016x}\n", SEED_FINGERPRINT),
        "a re-run pass must refresh the stamp to the current fingerprint"
    );
}

#[test]
fn seed_gate_treats_corrupt_stamp_as_mismatch() {
    let root = tempfile::tempdir().unwrap();
    let phenos = root.path().join("phenos");
    let skills = root.path().join("skills");
    let stamp = root.path().join(".seed_version");
    fs::write(&stamp, "not-a-fingerprint\n").unwrap();

    seed_builtin_content_gated(Some(&stamp), &phenos, &skills, None);

    assert!(
        phenos.join("codon.toml").exists(),
        "a corrupt stamp must be treated as not-stamped and re-run"
    );
    assert_eq!(
        fs::read_to_string(&stamp).unwrap(),
        format!("{:016x}\n", SEED_FINGERPRINT)
    );
}

#[test]
fn seed_gate_runs_when_stamp_path_is_none() {
    // No home dir → no stamp path → degrade to "always run" (pre-gate
    // behavior), never panicking.
    let root = tempfile::tempdir().unwrap();
    let phenos = root.path().join("phenos");
    let skills = root.path().join("skills");

    seed_builtin_content_gated(None, &phenos, &skills, None);

    assert!(phenos.join("codon.toml").exists());
}

#[test]
fn retire_removes_an_unmodified_disabled_codegraph_seed() {
    let mcp = tempfile::tempdir().unwrap();
    let path = mcp.path().join("mcp.json");
    // The exact shape the pre-C3 seed wrote (disabled, workspace, serve --mcp).
    let seeded = r#"{"mcpServers":{"codegraph":{"command":"codegraph","args":["serve","--mcp"],"disabled":true,"scope":"workspace"}}}"#;
    fs::write(&path, seeded).unwrap();

    retire_seeded_codegraph_if_unmodified(&path);

    let servers = ff_mcp::load(&path).expect("mcp.json must still parse");
    assert!(
        !servers.iter().any(|s| s.id == "codegraph"),
        "an unmodified disabled seed must be retired (codegraph now lives in the codon phenotype)"
    );
}

#[test]
fn retire_leaves_a_user_edited_codegraph_entry() {
    let mcp = tempfile::tempdir().unwrap();
    let path = mcp.path().join("mcp.json");
    // The #573 workaround: an absolute command, enabled. The user owns this.
    let user =
        r#"{"mcpServers":{"codegraph":{"command":"my-codegraph","args":["--port","9000"]}}}"#;
    fs::write(&path, user).unwrap();

    retire_seeded_codegraph_if_unmodified(&path);

    let servers = ff_mcp::load(&path).unwrap();
    let codegraph = servers
        .iter()
        .find(|s| s.id == "codegraph")
        .expect("user entry must survive");
    assert_eq!(codegraph.command, "my-codegraph");
    assert_eq!(
        codegraph.args,
        vec!["--port".to_string(), "9000".to_string()]
    );
}

#[test]
fn retire_leaves_an_enabled_seed_the_user_turned_on() {
    let mcp = tempfile::tempdir().unwrap();
    let path = mcp.path().join("mcp.json");
    // Seed args/command/scope, but the user enabled it -> no longer an unmodified
    // seed, so it is the user's and must be left intact.
    let enabled = r#"{"mcpServers":{"codegraph":{"command":"codegraph","args":["serve","--mcp"],"disabled":false,"scope":"workspace"}}}"#;
    fs::write(&path, enabled).unwrap();

    retire_seeded_codegraph_if_unmodified(&path);

    let servers = ff_mcp::load(&path).unwrap();
    let codegraph = servers
        .iter()
        .find(|s| s.id == "codegraph")
        .expect("an enabled (user-owned) entry must survive");
    assert!(!codegraph.disabled);
}

#[test]
fn retire_is_a_noop_when_no_codegraph_entry() {
    let mcp = tempfile::tempdir().unwrap();
    let path = mcp.path().join("mcp.json");
    let other = r#"{"mcpServers":{"github":{"command":"gh-mcp"}}}"#;
    fs::write(&path, other).unwrap();

    retire_seeded_codegraph_if_unmodified(&path);

    let servers = ff_mcp::load(&path).unwrap();
    assert!(
        servers.iter().any(|s| s.id == "github"),
        "unrelated entries untouched"
    );
    assert_eq!(servers.len(), 1);
}

// Goal store wiring (#716): `build_goal_store` yields a working directory
// store, and the goal lifecycle (set -> checkpoint -> load -> delete)
// round-trips through it. Under cfg(test) the store roots at a per-process
// temp dir, so this never touches the real config dir.
#[test]
fn build_goal_store_round_trips_a_goal() {
    use ff_core::{Goal, GoalStatus};
    let store = build_goal_store();
    let sid = format!("goal-test-sess-{}", std::process::id());
    // Clean any leftover from a prior run of this process.
    let _ = store.delete(&sid);

    assert!(store.load(&sid).unwrap().is_none(), "no goal initially");

    let mut goal = Goal::new(&sid, "ship the thing", 1_000);
    goal.status = GoalStatus::Active;
    store.save(&goal).unwrap();

    let loaded = store.load(&sid).unwrap().expect("goal persisted");
    assert_eq!(loaded.objective, "ship the thing");
    assert_eq!(loaded.status, GoalStatus::Active);

    // A checkpoint accrues spend + bumps the iteration; persisted state
    // reflects it (resume reads the last completed boundary).
    goal.checkpoint(120, 50, 2_000);
    store.save(&goal).unwrap();
    let after = store.load(&sid).unwrap().unwrap();
    assert_eq!(after.iteration, 1);
    assert_eq!(after.spent.tokens, 120);

    store.delete(&sid).unwrap();
    assert!(store.load(&sid).unwrap().is_none(), "cleared");
}

// Single-flight guard (#716) is what makes boot rehydration (#802) safe against
// a racing IPC `goal_resume`: the second claimant is refused, so only one loop
// per session can ever run. The slot is reclaimable once the loop ends.
#[test]
fn goal_loop_single_flight_guard_refuses_a_second_start() {
    let state = AppState::new();
    let sid = "guard-sess";
    assert!(
        state.try_start_goal_loop(sid),
        "first caller claims the slot"
    );
    assert!(
        !state.try_start_goal_loop(sid),
        "a racing second start is refused"
    );
    assert!(state.goal_loop_running(sid));
    state.end_goal_loop(sid);
    assert!(!state.goal_loop_running(sid));
    assert!(
        state.try_start_goal_loop(sid),
        "slot is reclaimable after the loop ends"
    );
    state.end_goal_loop(sid);
}

// Boot resume-on-restart (#802) iterates `goals.list_active()` and respawns a
// loop per session. This asserts the selection that drives it: only `Active`
// goals come back; Paused/Completed/etc are left for a manual resume. Uses an
// isolated tempdir (not the shared per-process `build_goal_store` dir) so the
// assertion is an exact set, free of cross-test leakage.
#[test]
fn boot_rehydration_selects_only_active_goals() {
    use ff_core::{Goal, GoalStatus};
    let dir = tempfile::tempdir().unwrap();
    let store = GoalStore::new(dir.path().join("goals"));
    let save = |sid: &str, status: GoalStatus| {
        let mut g = Goal::new(sid, "obj", 1);
        g.status = status;
        store.save(&g).unwrap();
    };
    save("resume-me", GoalStatus::Active);
    save("was-paused", GoalStatus::Paused);
    save("was-done", GoalStatus::Completed);
    save("was-failed", GoalStatus::Failed);

    let active: Vec<String> = store
        .list_active()
        .into_iter()
        .map(|g| g.session_id)
        .collect();
    assert_eq!(
        active,
        vec!["resume-me".to_string()],
        "only the Active goal is respawned on boot"
    );
}

#[test]
fn default_control_config_matches_the_frontend_defaults() {
    // The backend bakes in the same factory ControlConfig the frontend's
    // CONTROL_DEFAULTS declares (lib/control.ts). If this drifts, the real
    // installed app's Control panel loads a shape the UI can't render.
    let d = default_control_config();
    assert_eq!(d["defaultMode"], "auto");
    assert!(d["injectMemory"].as_bool().unwrap());
    assert_eq!(d["permissionPolicy"]["read"], "allow");
    assert_eq!(d["permissionPolicy"]["localWrites"], "allow");
    assert_eq!(d["permissionPolicy"]["externalChanges"], "ask");
    assert_eq!(d["permissionPolicy"]["dangerous"], "deny");
    assert_eq!(d["ui"]["accentColor"], "#6366f1");
    assert!(d["ui"]["contextualGreeting"].as_bool().unwrap());
    assert_eq!(d["teammates"].as_array().unwrap().len(), 2);
    // Every field the frontend ControlConfig interface requires is present.
    for key in [
        "defaultMode",
        "permissionPolicy",
        "injectMemory",
        "userInstructions",
        "promptFiles",
        "teammates",
        "ui",
    ] {
        assert!(d.get(key).is_some(), "missing field: {key}");
    }
}

// --- #509: timestamped registry backup before each save ---

#[test]
fn backup_preserves_prior_contents_before_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let reg_path = tmp.path().join("provider-registry.json");
    fs::write(&reg_path, "v1").unwrap();

    // Simulate a save: backup then atomic-write.
    backup_registry_at(&reg_path, 1000, 5);
    write_atomic(&reg_path, "v2").unwrap();

    // The live file has the new contents.
    assert_eq!(fs::read_to_string(&reg_path).unwrap(), "v2");
    // Exactly one .bak containing the prior contents.
    let bak = tmp.path().join("provider-registry.1000.bak");
    assert!(bak.exists(), "backup file must exist");
    assert_eq!(fs::read_to_string(&bak).unwrap(), "v1");
}

#[test]
fn backup_prunes_beyond_retention() {
    let tmp = tempfile::tempdir().unwrap();
    let reg_path = tmp.path().join("provider-registry.json");

    // Create retention + 2 backups with distinct injected timestamps.
    let retention = 3usize;
    for ts in 100..107u64 {
        fs::write(&reg_path, format!("v{ts}")).unwrap();
        backup_registry_at(&reg_path, ts, retention);
    }

    // Only the newest `retention` survive.
    let baks: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with("provider-registry.") && n.ends_with(".bak")
        })
        .collect();
    assert_eq!(
        baks.len(),
        retention,
        "only the newest {retention} backups should remain"
    );

    // The oldest (100, 101, 102, 103) are gone; 104, 105, 106 survive.
    assert!(!tmp.path().join("provider-registry.100.bak").exists());
    assert!(!tmp.path().join("provider-registry.101.bak").exists());
    assert!(!tmp.path().join("provider-registry.102.bak").exists());
    assert!(!tmp.path().join("provider-registry.103.bak").exists());
    assert!(tmp.path().join("provider-registry.104.bak").exists());
    assert!(tmp.path().join("provider-registry.105.bak").exists());
    assert!(tmp.path().join("provider-registry.106.bak").exists());
}

#[test]
fn backup_is_noop_when_no_registry_yet() {
    let tmp = tempfile::tempdir().unwrap();
    let reg_path = tmp.path().join("provider-registry.json");
    // File does not exist — backup must not panic or create anything.
    backup_registry_at(&reg_path, 999, 5);
    let entries: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(entries.is_empty(), "no files should be created");
}
