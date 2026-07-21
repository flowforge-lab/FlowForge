use super::state::AppState;
use super::{
    emit_agent_event, git_branch, goal_gate_for, is_app_ready, list_directory_in,
    list_local_branches, matrix_gate, panic_message, pre_prompt_decision, publish_app_ready,
    read_file_in, resolve_tool_arg, resolve_workspace_dir, run_sidecar_turn, should_warmup,
    switch_branch, BootFinalize, PrePromptDecision, TurnMetrics, UpdateStatus, APP_READY,
};
use ff_agent::{AgentEvent, GateDecision};
use ff_core::events::TurnDoneEvent;
use ff_core::{Mode, ProviderKind};
use ff_tools::Safety;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

// `APP_READY` is a process-global static, and the boot-state tests below are
// the only tests in the crate that touch it. Guard them with a mutex so the
// two never race each other (parallel `cargo test` threads share the flag).
static BOOT_TEST_LOCK: Mutex<()> = Mutex::new(());

// #768 review B2: the scoped-rule arg table must read each tool's REAL
// argument key, checked against the ff-tools schemas. A wrong key silently
// resolves to `None`, so the rule never fires (fail-open for deny backstops).
#[test]
fn resolve_tool_arg_reads_real_schema_keys() {
    use serde_json::json;
    assert_eq!(
        resolve_tool_arg("bash", &json!({"command": "cargo build"})),
        Some("cargo build".into())
    );
    // python's key is `code`, NOT `command`.
    assert_eq!(
        resolve_tool_arg("python", &json!({"code": "print(1)"})),
        Some("print(1)".into())
    );
    assert_eq!(
        resolve_tool_arg("python", &json!({"command": "print(1)"})),
        None
    );
    for tool in ["view", "edit", "write"] {
        assert_eq!(
            resolve_tool_arg(tool, &json!({"path": "src/main.rs"})),
            Some("src/main.rs".into())
        );
    }
    // Read-only search tools short-circuit before approve() — not listed.
    assert_eq!(resolve_tool_arg("grep", &json!({"pattern": "x"})), None);
    assert_eq!(
        resolve_tool_arg("glob", &json!({"pattern": "**/*.rs"})),
        None
    );
    // Formerly-listed phantom keys resolve to nothing now.
    assert_eq!(resolve_tool_arg("rg", &json!({"path": "."})), None);
    assert_eq!(resolve_tool_arg("fd", &json!({"path": "."})), None);
}

// Spy that asserts the `manage` → `store` → `emit` ordering at call time: a
// reordered `publish_app_ready` (e.g. `store` hoisted above `manage`, or
// `emit` moved above `store`) trips the assert inside the offending method
// and fails the test, instead of silently breaking the FE gate.
#[derive(Default)]
struct BootFinalizeSpy {
    managed: std::sync::atomic::AtomicBool,
    emitted: std::sync::atomic::AtomicBool,
}

impl BootFinalize for BootFinalizeSpy {
    fn manage_state(&self, _state: Arc<AppState>) {
        // `manage` runs BEFORE the flag flips — if it's already true here,
        // someone hoisted `APP_READY.store` above `manage` and the FE would
        // see `is_app_ready()==true` with no managed state.
        assert!(
            !APP_READY.load(Ordering::SeqCst),
            "invariant violation: manage_state ran after APP_READY was already true \
             (store was reordered before manage)"
        );
        self.managed.store(true, Ordering::SeqCst);
    }
    fn emit_ready(&self) {
        // `emit` runs AFTER the flag flips AND after `manage` — if the flag
        // is false here, `emit` was moved before `store` (the FE's
        // post-event `isAppReady()` poll would then read false and hang); if
        // `manage` didn't run yet, `emit` was moved before `manage`.
        assert!(
            APP_READY.load(Ordering::SeqCst),
            "invariant violation: emit_ready ran before APP_READY was set \
             (emit was reordered before store)"
        );
        assert!(
            self.managed.load(Ordering::SeqCst),
            "invariant violation: emit_ready ran before manage_state \
             (emit was reordered before manage)"
        );
        self.emitted.store(true, Ordering::SeqCst);
    }
}

// #599 boot state machine: `is_app_ready()` reads a static flag flipped once
// at the end of the hydrate task. Pin the false-then-true transition so a
// future change that, say, reads the wrong flag or inverts the polarity is
// caught here rather than in a manual cold-start check.
#[test]
fn is_app_ready_flips_false_to_true() {
    let _guard = BOOT_TEST_LOCK.lock().unwrap();
    APP_READY.store(false, Ordering::SeqCst);
    assert!(!is_app_ready(), "flag must read false before finalization");
    APP_READY.store(true, Ordering::SeqCst);
    assert!(
        is_app_ready(),
        "flag must read true after APP_READY.store(true)"
    );
    APP_READY.store(false, Ordering::SeqCst);
}

// #599 boot state machine: the manage→store→emit ordering is the invariant
// `publish_app_ready` exists to encode. A spy that self-asserts at each call
// catches every reorder that breaks the subscribe-then-check gate
// (store-before-manage, emit-before-store, emit-before-manage), and the
// post-call asserts catch a call being dropped entirely. A future reorder of
// those three lines now fails this test instead of slipping past review.
#[test]
fn publish_app_ready_orders_manage_before_flag_before_emit() {
    let _guard = BOOT_TEST_LOCK.lock().unwrap();
    APP_READY.store(false, Ordering::SeqCst);

    let spy = BootFinalizeSpy::default();
    publish_app_ready(&spy, Arc::new(AppState::new()));

    assert!(
        is_app_ready(),
        "APP_READY must be true after publish_app_ready"
    );
    assert!(
        spy.managed.load(Ordering::SeqCst),
        "manage_state was never called"
    );
    assert!(
        spy.emitted.load(Ordering::SeqCst),
        "emit_ready was never called"
    );

    APP_READY.store(false, Ordering::SeqCst);
}

// `panic_message` renders both stringy panic-payload forms and the rare
// non-string fallback with the caller's context prefix, so a detached-task
// panic surfaces an actionable `app:init-error` rather than an opaque note.
#[test]
fn panic_message_formats_each_payload_form() {
    let literal: Box<dyn std::any::Any + Send> = Box::new("boom");
    assert_eq!(
        panic_message("mcp init", &literal),
        "mcp init panicked: boom"
    );

    let owned: Box<dyn std::any::Any + Send> = Box::new(String::from("kaboom"));
    assert_eq!(
        panic_message("post-init", &owned),
        "post-init panicked: kaboom"
    );

    let other: Box<dyn std::any::Any + Send> = Box::new(42u32);
    assert_eq!(
        panic_message("mcp init", &other),
        "mcp init panicked (non-string panic payload)"
    );
}

#[test]
fn should_warmup_only_for_local_kinds_with_warmup_enabled() {
    assert!(should_warmup(ProviderKind::CandleVllm, true));
    assert!(should_warmup(ProviderKind::Ollama, true));
    // Disabled by the user -> no warmup even on a local kind.
    assert!(!should_warmup(ProviderKind::Ollama, false));
    // Hosted kinds never warm (would fire a billed request).
    assert!(!should_warmup(ProviderKind::OpenAi, true));
    assert!(!should_warmup(ProviderKind::Bedrock, true));
    assert!(!should_warmup(ProviderKind::SiliconFlow, true));
}

// `UpdateStatus` has no ts-rs binding -- it is cast on the FE side from the JSON
// this serializes to (`lib/about.ts`). Pin the wire shape so the handwritten FE
// type and this enum cannot drift apart silently (#159).
#[test]
fn update_status_matches_fe_contract() {
    assert_eq!(
        serde_json::to_value(UpdateStatus::UpToDate {
            version: "0.1.0".into()
        })
        .unwrap(),
        serde_json::json!({ "kind": "upToDate", "version": "0.1.0" })
    );
    assert_eq!(
        serde_json::to_value(UpdateStatus::Available {
            version: "0.2.0".into(),
            notes: Some("notes".into()),
        })
        .unwrap(),
        serde_json::json!({ "kind": "available", "version": "0.2.0", "notes": "notes" })
    );
    assert_eq!(
        serde_json::to_value(UpdateStatus::Available {
            version: "0.2.0".into(),
            notes: None,
        })
        .unwrap(),
        serde_json::json!({ "kind": "available", "version": "0.2.0", "notes": null })
    );
}

// Permission matrix correctness is tested in ff-core::permission::tests.
// This spot-check confirms the matrix drives the approval path as expected.
#[test]
fn permission_matrix_default_matches_expected() {
    use ff_core::{PermissionCell, PermissionMatrix};
    let m = PermissionMatrix::default();
    // Auto: Write is auto-approved, Sensitive prompts, Dangerous denied.
    assert_eq!(m.cell(Mode::Auto, Safety::Write), PermissionCell::Allow);
    assert_eq!(m.cell(Mode::Auto, Safety::Sensitive), PermissionCell::Ask);
    assert_eq!(m.cell(Mode::Auto, Safety::Dangerous), PermissionCell::Deny);
    // Act: Write+Sensitive auto-approved, Dangerous prompts.
    assert_eq!(m.cell(Mode::Act, Safety::Write), PermissionCell::Allow);
    assert_eq!(m.cell(Mode::Act, Safety::Sensitive), PermissionCell::Allow);
    assert_eq!(m.cell(Mode::Act, Safety::Dangerous), PermissionCell::Ask);
    // Plan: only ReadOnly allowed.
    assert_eq!(m.cell(Mode::Plan, Safety::ReadOnly), PermissionCell::Allow);
    assert_eq!(m.cell(Mode::Plan, Safety::Write), PermissionCell::Deny);
}

// #719: the coarse goal-loop gate keys on the Sensitive tier of the active
// mode's matrix posture — Auto pauses (Ask), Act proceeds (Allow), Plan
// denies (Deny). Locks the mapping against the default matrix so a matrix
// change that would let an unattended loop run in Plan (or auto-run Sensitive
// work in Auto) fails here.
#[test]
fn goal_gate_maps_matrix_posture_per_mode() {
    use ff_core::PermissionMatrix;
    let m = PermissionMatrix::default();
    // Auto: Sensitive = Ask -> pause & surface, don't auto-run unattended.
    assert_eq!(goal_gate_for(Mode::Auto, &m), GateDecision::Pause);
    // Act: Sensitive = Allow -> run autonomously.
    assert_eq!(goal_gate_for(Mode::Act, &m), GateDecision::Proceed);
    // Plan: Sensitive = Ask (#793) -> pause & surface; the goal asks the
    // human before an externally-visible read (web_fetch) rather than
    // hard-denying. Write/Dangerous stay denied inside the turn.
    assert_eq!(goal_gate_for(Mode::Plan, &m), GateDecision::Pause);
}

// #719: an edited matrix cell flips the goal gate on the next boundary (read
// live), mirroring the per-tool gate acceptance (#702) at the loop level.
#[test]
fn goal_gate_follows_an_edited_sensitive_cell() {
    use ff_core::{PermissionCell, PermissionMatrix};
    let mut m = PermissionMatrix::default();
    // Default Auto+Sensitive is Ask -> Pause.
    assert_eq!(goal_gate_for(Mode::Auto, &m), GateDecision::Pause);
    // Operator allows Sensitive in Auto -> the loop may now proceed unattended.
    m.set_cell(Mode::Auto, Safety::Sensitive, PermissionCell::Allow);
    assert_eq!(goal_gate_for(Mode::Auto, &m), GateDecision::Proceed);
    // Operator hard-denies it -> the loop halts.
    m.set_cell(Mode::Auto, Safety::Sensitive, PermissionCell::Deny);
    assert_eq!(goal_gate_for(Mode::Auto, &m), GateDecision::Deny);
}

// #778: the neutral continuation nudge must NOT inline the objective — the
// system-prompt goal block (#718) is the single source for it, so repeating
// it here duplicated it every iteration. Guard that the nudge stays generic
// while still pointing at goal_complete.
#[test]
fn goal_continue_nudge_does_not_inline_the_objective() {
    let n = super::GOAL_CONTINUE_NUDGE;
    assert!(
        n.contains("goal_complete"),
        "nudge should still point at the completion tool"
    );
    assert!(
        n.to_lowercase().contains("continue toward the goal"),
        "nudge should be a neutral continue"
    );
    // It must reference the goal only by indirection ("in your instructions"),
    // never carry an objective string or a format placeholder.
    assert!(
        n.contains("described in your instructions"),
        "nudge must defer to the system-prompt goal block for the objective"
    );
    assert!(
        !n.contains("{}") && !n.contains("{0}"),
        "nudge must be a static string, not an objective-interpolating format"
    );
}

// Acceptance (#702): editing a matrix cell changes the invocation-time gate
// decision that `UiApprover::approve` applies to the next tool call. Exercises
// the same pure `matrix_gate` the approver calls, so no AppHandle / live model
// turn is required.
#[test]
fn edited_cell_flips_the_invocation_gate() {
    use ff_core::{PermissionCell, PermissionMatrix};
    let mut m = PermissionMatrix::default();
    // Mirror `UiApprover::approve`: resolve the effective cell (per-tool
    // override else the mode×safety cell, #742) then gate on it.
    let gate = |m: &PermissionMatrix, tool: &str, mode: Mode, safety: Safety| {
        matrix_gate(m.effective_cell(tool, mode, safety))
    };

    // Default: a Dangerous call in Act prompts the user (Ask → None).
    assert_eq!(gate(&m, "bash", Mode::Act, Safety::Dangerous), None);
    // A Sensitive call in Auto also prompts by default.
    assert_eq!(gate(&m, "web_fetch", Mode::Auto, Safety::Sensitive), None);

    // Deny the dangerous cell → the next invocation is rejected outright.
    m.set_cell(Mode::Act, Safety::Dangerous, PermissionCell::Deny);
    assert_eq!(gate(&m, "bash", Mode::Act, Safety::Dangerous), Some(false));

    // Allow the sensitive cell → the next invocation auto-approves (no prompt).
    m.set_cell(Mode::Auto, Safety::Sensitive, PermissionCell::Allow);
    assert_eq!(
        gate(&m, "web_fetch", Mode::Auto, Safety::Sensitive),
        Some(true)
    );

    // A per-tool override (#742) wins over the matrix cell for that tool only.
    m.set_override("web_fetch", PermissionCell::Deny);
    assert_eq!(
        gate(&m, "web_fetch", Mode::Auto, Safety::Sensitive),
        Some(false)
    );
    assert_eq!(
        gate(&m, "web_search", Mode::Auto, Safety::Sensitive),
        Some(true)
    );

    // Untouched cells keep their default decision.
    assert_eq!(gate(&m, "read_file", Mode::Act, Safety::Write), Some(true));
    assert_eq!(
        gate(&m, "write_file", Mode::Plan, Safety::Write),
        Some(false)
    );
}

// #827/#828 Part C: pre_prompt_decision encodes the canonical gate order.
// A regression that reorders allowlist-first is caught here directly.
#[test]
fn pre_prompt_deny_overrides_allowlist() {
    use ff_core::PermissionCell;
    assert_eq!(
        pre_prompt_decision(PermissionCell::Deny, true, None, Safety::Write),
        PrePromptDecision::Deny
    );
    assert_eq!(
        pre_prompt_decision(PermissionCell::Deny, true, None, Safety::Sensitive),
        PrePromptDecision::Deny
    );
}

#[test]
fn pre_prompt_allowlist_accelerates_ask() {
    use ff_core::PermissionCell;
    assert_eq!(
        pre_prompt_decision(PermissionCell::Ask, true, None, Safety::Write),
        PrePromptDecision::Allow
    );
    assert_eq!(
        pre_prompt_decision(PermissionCell::Ask, false, None, Safety::Write),
        PrePromptDecision::Prompt
    );
}

#[test]
fn pre_prompt_scoped_deny_vetoes_when_not_allowlisted() {
    use ff_core::{PermissionCell, RuleEffect};
    // Scoped Deny vetoes when the tool is NOT on the allowlist.
    assert_eq!(
        pre_prompt_decision(
            PermissionCell::Ask,
            false,
            Some(RuleEffect::Deny),
            Safety::Write
        ),
        PrePromptDecision::Deny
    );
    // But the allowlist fires first — if allowlisted, scoped rules are skipped.
    assert_eq!(
        pre_prompt_decision(
            PermissionCell::Ask,
            true,
            Some(RuleEffect::Deny),
            Safety::Write
        ),
        PrePromptDecision::Allow
    );
}

#[test]
fn pre_prompt_scoped_allow_does_not_clear_dangerous() {
    use ff_core::{PermissionCell, RuleEffect};
    assert_eq!(
        pre_prompt_decision(
            PermissionCell::Ask,
            false,
            Some(RuleEffect::Allow),
            Safety::Dangerous
        ),
        PrePromptDecision::Prompt
    );
    assert_eq!(
        pre_prompt_decision(
            PermissionCell::Ask,
            false,
            Some(RuleEffect::Allow),
            Safety::Write
        ),
        PrePromptDecision::Allow
    );
}

#[test]
fn pre_prompt_plan_write_denied_despite_allowlist() {
    let state = AppState::new();
    state.set_session_approve("s1", "github");
    let matrix = state.permission_matrix();
    let cell = matrix.effective_cell("github", Mode::Plan, Safety::Write);
    let allowlisted = state.allowlist_covers("s1", "github", Safety::Write);
    assert!(allowlisted);
    assert_eq!(
        pre_prompt_decision(cell, allowlisted, None, Safety::Write),
        PrePromptDecision::Deny,
        "Plan x Write = Deny must override the allowlist"
    );
}

#[test]
fn resolve_workspace_dir_accepts_existing_directory() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = resolve_workspace_dir(dir.path().to_str().unwrap()).unwrap();
    assert!(resolved.is_dir());
    // Canonicalized: absolute and symlink-resolved.
    assert_eq!(resolved, std::fs::canonicalize(dir.path()).unwrap());
}

#[test]
fn resolve_workspace_dir_rejects_missing_path() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist");
    let err = resolve_workspace_dir(missing.to_str().unwrap()).unwrap_err();
    assert!(err.contains("cannot resolve directory"));
}

#[test]
fn resolve_workspace_dir_rejects_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a-file.txt");
    std::fs::write(&file, "x").unwrap();
    let err = resolve_workspace_dir(file.to_str().unwrap()).unwrap_err();
    assert!(err.contains("not a directory"));
}

#[test]
fn git_branch_reads_symbolic_ref() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/feature/x\n").unwrap();
    assert_eq!(git_branch(dir.path()), Some("feature/x".to_string()));
}

#[test]
fn git_branch_is_none_for_detached_head() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    // Detached HEAD stores a bare commit SHA, not a `ref:` line.
    std::fs::write(
        dir.path().join(".git/HEAD"),
        "0123456789abcdef0123456789abcdef01234567\n",
    )
    .unwrap();
    assert_eq!(git_branch(dir.path()), None);
}

#[test]
fn git_branch_is_none_when_not_a_repo() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(git_branch(dir.path()), None);
}

/// Init a temp repo with one commit on `main` plus the extra `branches`, all
/// pointing at that commit. Returns the tempdir (keep it alive for the test).
/// Skips (returns `None`) if `git` is unavailable so the suite still passes on
/// a host without git installed.
#[cfg(test)]
fn temp_repo(branches: &[&str]) -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !run(&["init", "-q", "-b", "main"]) {
        return None; // git missing or too old for `-b`; skip.
    }
    run(&["config", "user.email", "t@example.com"]);
    run(&["config", "user.name", "t"]);
    std::fs::write(dir.path().join("f.txt"), "x").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "init"]);
    for b in branches {
        run(&["branch", b]);
    }
    Some(dir)
}

#[test]
fn list_local_branches_returns_all_local_branches_sorted() {
    let Some(dir) = temp_repo(&["develop", "feature/x"]) else {
        return;
    };
    let mut got = list_local_branches(dir.path()).unwrap();
    got.sort();
    assert_eq!(got, vec!["develop", "feature/x", "main"]);
}

#[test]
fn list_local_branches_is_empty_when_not_a_repo() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        list_local_branches(dir.path()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn switch_branch_checks_out_and_moves_head() {
    let Some(dir) = temp_repo(&["develop"]) else {
        return;
    };
    assert_eq!(git_branch(dir.path()), Some("main".to_string()));
    switch_branch(dir.path(), "develop").unwrap();
    assert_eq!(git_branch(dir.path()), Some("develop".to_string()));
}

#[test]
fn switch_branch_rejects_unknown_branch() {
    let Some(dir) = temp_repo(&[]) else {
        return;
    };
    let err = switch_branch(dir.path(), "nope").unwrap_err();
    assert!(err.contains("unknown branch"));
    // HEAD did not move.
    assert_eq!(git_branch(dir.path()), Some("main".to_string()));
}

#[test]
fn switch_branch_rejects_flag_like_branch_before_spawning_git() {
    let Some(dir) = temp_repo(&[]) else {
        return;
    };
    // A branch literally named like a flag is never a real local branch, so the
    // membership check rejects it before it can reach `git checkout`.
    let err = switch_branch(dir.path(), "--orphan").unwrap_err();
    assert!(err.contains("unknown branch"));
}

// F1 (#427): the turn-metrics accumulator counts one round-trip per distinct
// assistant message, breaks the turn into per-iteration wall-clock, and counts
// silent memory flushes -- the baseline the performance epic measures against.
#[test]
fn turn_metrics_counts_round_trips_flushes_and_iterations() {
    let mut m = TurnMetrics::default();
    let turn_start = std::time::Instant::now();
    // Two iterations (two distinct message ids); repeats are idempotent.
    m.note_turn("m1");
    m.note_turn("m1");
    m.tokens += 5;
    m.note_flush();
    m.note_turn("m2");
    m.note_turn("m2");

    let (chars, turns) = m.snapshot();
    assert_eq!(chars, 5);
    assert_eq!(turns, 2, "two distinct assistant messages = two turns");

    let (round_trips, iter_ms, flushes, first_token_ms) =
        m.timing(turn_start, std::time::Instant::now());
    assert_eq!(round_trips, 2, "one round-trip per distinct message id");
    assert_eq!(iter_ms.len(), 2, "one wall-clock sample per iteration");
    assert_eq!(flushes, 1, "exactly one mid-turn flush counted");
    assert!(
        first_token_ms.is_some(),
        "TTFT populated when at least one assistant message arrived"
    );
}

// A turn that never reached the model (no assistant message) reports a clean
// zero baseline rather than panicking on the empty iteration vector.
#[test]
fn turn_metrics_empty_turn_is_zeroed() {
    let m = TurnMetrics::default();
    let turn_start = std::time::Instant::now();
    let (round_trips, iter_ms, flushes, first_token_ms) =
        m.timing(turn_start, std::time::Instant::now());
    assert_eq!(round_trips, 0);
    assert!(iter_ms.is_empty());
    assert_eq!(flushes, 0);
    assert!(
        first_token_ms.is_none(),
        "TTFT is None when the turn produced no assistant message"
    );
    // F1b (#441): a turn whose Done carried no telemetry reports a clean zero.
    assert!(m.prefill_estimates.is_empty());
    assert_eq!(m.tier1_fires, 0);
    assert_eq!(m.tier2_fires, 0);
    // #960: no Done telemetry -> no prompt latency.
    assert_eq!(m.prompt_latency_ms, None);
}

// #960: `note_done` folds the agent-side round-0 prompt latency onto the metrics
// so the host can emit it as `promptLatencyMs`. A plain assign (fires once/turn).
#[test]
fn turn_metrics_note_done_captures_prompt_latency() {
    let mut m = TurnMetrics::default();
    m.note_done(&[123], Some(42), Some(9000), 1, 0, 0);
    assert_eq!(
        m.prompt_latency_ms,
        Some(42),
        "prompt latency stored verbatim"
    );
    assert_eq!(m.tier2_ms, Some(9000), "#971 tier2_ms stored verbatim");
    assert_eq!(m.prefill_estimates, vec![123]);
    assert_eq!(m.tier1_fires, 1);
    // A None from an emitter that didn't compute it stays None.
    m.note_done(&[], None, None, 0, 0, 0);
    assert_eq!(m.prompt_latency_ms, None);
    assert_eq!(m.tier2_ms, None);
}

// TTFT (#427): the recorded first-token latency is the delta from `turn_start` to
// the first `note_turn` -- not from the first `note_turn` to the second. This
// pins the semantics so a future refactor can't silently drift to "time between
// iterations" (which is `iter_ms[0]`, a different signal).
#[test]
fn turn_metrics_first_token_ms_measures_from_turn_start_not_between_iters() {
    let mut m = TurnMetrics::default();
    let turn_start = std::time::Instant::now();
    // Simulate 20ms of "waiting for the model to start" before the first token
    // arrives, then a second iteration close behind. The gap between iterations
    // must not be counted as TTFT.
    std::thread::sleep(std::time::Duration::from_millis(20));
    m.note_turn("m1");
    std::thread::sleep(std::time::Duration::from_millis(5));
    m.note_turn("m2");

    let (_, iter_ms, _, first_token_ms) = m.timing(turn_start, std::time::Instant::now());
    let ttft = first_token_ms.expect("TTFT populated when a message arrived");
    assert!(
        ttft >= 20,
        "TTFT must span turn_start -> first token (>= 20ms), got {ttft}"
    );
    // Sanity: iter_ms[0] measures the span between iterations, and is a
    // separate signal from TTFT. The first-iter span is smaller here because
    // the two `note_turn` calls are only ~5ms apart.
    assert!(
        iter_ms[0] < ttft,
        "iter_ms[0] ({}) is between iters, distinct from TTFT ({ttft})",
        iter_ms[0]
    );
}

#[test]
fn turn_metrics_note_done_folds_f1b_telemetry() {
    // #441: the per-round-trip prefill estimate and the two compaction-fire
    // counts from the turn's Done event are captured verbatim for `turn:stats`.
    let mut m = TurnMetrics::default();
    m.note_done(&[120, 340, 75], None, None, 2, 1, 3);
    assert_eq!(m.prefill_estimates, vec![120, 340, 75]);
    assert_eq!(m.tier1_fires, 2);
    assert_eq!(m.tier2_fires, 1);
    assert_eq!(m.retrieve_calls, 3, "#1045 recall cost captured verbatim");
}

// ---- CLI.7 sidecar parity integration test (RFC 0004 §5) ----
//
// `run_sidecar_turn` is the Tauri command that spawns the bundled `flowforge`
// CLI as a sidecar, reads its `--json` stdout line-by-line, and re-emits every
// parsed `AgentEvent` through `emit_agent_event` — the same helper the
// in-process `run_turn` path uses. This test exercises the full
// spawn → stdout-parse → emit pipeline end-to-end:
//
//   1. Requires the sidecar binary at `target/<profile>/flowforge` (staged by
//      `scripts/stage-sidecar.sh` or a bare `cargo build -p ff-cli`). The
//      `tauri_plugin_shell` sidecar resolver looks for the binary relative to
//      `current_exe()`; under `cargo test` that resolves to the workspace
//      `target/<profile>/` directory.
//   2. Stands up a wiremock OpenAI-compatible endpoint so the CLI can actually
//      complete a turn (the `run` subcommand would otherwise fail to reach a
//      provider and exit non-zero before emitting `turn:done`).
//   3. Overrides `HOME` so the CLI reads a temp `provider.json` pointing at
//      the mock. A process-wide mutex serializes the override against parallel
//      tests that might also touch `HOME`.
//   4. Drives `run_sidecar_turn` through a `MockRuntime` Tauri app (the
//      command is generic over `R: tauri::Runtime` for exactly this reason)
//      and asserts the `turn:done` event fires.

/// `HOME` is a process-global env var; serialize tests that override it so
/// they don't race each other or other tests that read `HOME`.
///
/// NOTE: `std::env::set_var` is deprecated as of Rust 1.80 on soundness
/// grounds — it's UB in multithreaded programs because another thread
/// can read `HOME` concurrently with the mutation. This mutex only
/// serializes *these tests*; it does not stop unrelated threads from
/// reading `HOME` mid-override. Acceptable today because each
/// `#[tokio::test]` here runs on a single task with no other threads
/// touching `HOME`, but a future edition is expected to make this a hard
/// error — at which point the override should move to per-spawn env
/// injection (e.g. `Command::env` on the sidecar subprocess) rather than
/// mutating the process-global `HOME`.
static SIDECAR_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Resolve the sidecar binary path that `tauri_plugin_shell`'s
/// `relative_command_path` will look for under `cargo test` — the workspace
/// `target/<profile>/flowforge` (no target-triple suffix at runtime; the
/// suffix is only used by the `tauri build` bundler).
fn sidecar_binary_path() -> std::path::PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    // The test binary lives in `target/<profile>/deps/<name>-<hash>`. Pop
    // `deps/<name>-<hash>` to land on `target/<profile>/`.
    path.pop(); // <name>-<hash>
    path.pop(); // deps
    path.push("flowforge");
    path
}

/// Write a `provider.json` that points the CLI's `CandleVllm` provider at the
/// mock server, under the temp `HOME`'s platform-specific config dir. Returns
/// the temp dir (kept alive for the test duration).
fn stage_provider_config(base_url: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let temp_home = tempfile::tempdir().expect("temp HOME");

    // `dirs::config_dir()` respects `HOME` on Unix and macOS — so set it
    // before computing the path.
    let old_home = std::env::var_os("HOME");
    std::env::set_var("HOME", temp_home.path());

    let config_dir = dirs::config_dir()
        .expect("config dir resolves under temp HOME")
        .join("flowforge");
    std::fs::create_dir_all(&config_dir).expect("create flowforge config dir");

    let provider_json = serde_json::json!({
        "kind": "candleVllm",
        "baseUrl": base_url,
        "model": "test-sidecar-model",
        "hasKey": false,
        "thinking": false,
    });
    let provider_path = config_dir.join("provider.json");
    std::fs::write(&provider_path, provider_json.to_string()).expect("write provider.json");

    // Restore HOME — the override is re-applied for the actual sidecar spawn
    // in the test body. We only needed it here to compute the platform path.
    match old_home {
        Some(h) => std::env::set_var("HOME", h),
        None => std::env::remove_var("HOME"),
    }

    (temp_home, provider_path)
}

#[tokio::test]
#[ignore = "requires staged sidecar — run ./scripts/stage-sidecar.sh, then `cargo test ... -- --ignored`"]
#[allow(clippy::await_holding_lock)]
async fn sidecar_turn_emits_turn_done_event() {
    // The `std::sync::Mutex` guard is held across `.await` points because
    // the `HOME` override must stay in place for the entire sidecar spawn
    // + turn duration. This is safe: the lock is only acquired by this one
    // test, runs on a single tokio task, and guards a process-global env
    // var — no other async task contends on it.
    //
    // `#[ignore]` makes this opt-in: `cargo test --workspace` reports it as
    // ignored (not passed), so CI can't silently drop it behind a soft-skip
    // `return` and advertise false confidence. Run it explicitly with
    // `--ignored` after staging the sidecar.
    let sidecar = sidecar_binary_path();
    assert!(
        sidecar.exists(),
        "sidecar binary not found at {} — run `./scripts/stage-sidecar.sh` \
         (or `cargo build -p ff-cli`) first",
        sidecar.display(),
    );

    let _guard = SIDECAR_TEST_LOCK.lock().unwrap();

    // Mock OpenAI-compatible endpoint: one content delta, then [DONE].
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/chat/completions"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n\
                 data: [DONE]\n\n",
        ))
        .mount(&server)
        .await;

    // Stage provider.json under a temp HOME.
    let (temp_home, _provider_path) = stage_provider_config(&server.uri());

    // Override HOME for the sidecar subprocess (it inherits the parent's env).
    let old_home = std::env::var_os("HOME");
    std::env::set_var("HOME", temp_home.path());

    // Mock Tauri app with the shell plugin — `run_sidecar_turn` is generic
    // over `R: tauri::Runtime` so it accepts the `MockRuntime` app handle.
    let app = tauri::test::mock_builder()
        .plugin(tauri_plugin_shell::init())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock app builds");

    // Listen for the `turn:done` event that `emit_agent_event` emits.
    use tauri::Listener;
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done_clone = done.clone();
    app.listen("turn:done", move |_| {
        done_clone.store(true, Ordering::SeqCst);
    });

    // Also track the total event count for a richer assertion.
    let token = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let token_clone = token.clone();
    app.listen("turn:token", move |_| {
        token_clone.fetch_add(1, Ordering::SeqCst);
    });

    let handle = app.handle().clone();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        run_sidecar_turn(handle, "hello".into()),
    )
    .await;

    // Restore HOME before asserting so a panic doesn't leak the temp HOME.
    match old_home {
        Some(h) => std::env::set_var("HOME", h),
        None => std::env::remove_var("HOME"),
    }
    drop(_guard);

    let result = result.expect("run_sidecar_turn timed out after 30s");
    assert!(
        result.is_ok(),
        "run_sidecar_turn failed: {:?}",
        result.err()
    );
    assert!(
        done.load(Ordering::SeqCst),
        "turn:done event was not received — the sidecar spawned but the \
         AgentEvent → Tauri event pipeline did not deliver the terminal event"
    );
    assert!(
        token.load(Ordering::SeqCst) >= 1,
        "expected at least one turn:token event from the sidecar"
    );
}

#[tokio::test]
#[ignore = "requires the sidecar NOT be staged — inverse of the happy-path test; run with `--ignored` only when target/flowforge is absent"]
async fn sidecar_turn_returns_error_without_sidecar_binary() {
    // When the sidecar binary is absent, `run_sidecar_turn` must surface a
    // clear error rather than panicking or hanging. This guards the resolver
    // path (`app.shell().sidecar`) against silent failures.
    //
    // `#[ignore]` + a hard assert (not a soft-skip `return`) so CI can't
    // silently drop it: it fails loudly if the binary is present. Mutually
    // exclusive with `sidecar_turn_emits_turn_done_event` — don't run both
    // via `--ignored` together (one needs the binary staged, the other absent).
    let sidecar = sidecar_binary_path();
    assert!(
        !sidecar.exists(),
        "sidecar binary found at {} — this test requires the sidecar NOT be \
         staged (it is the inverse of `sidecar_turn_emits_turn_done_event`)",
        sidecar.display(),
    );

    let app = tauri::test::mock_builder()
        .plugin(tauri_plugin_shell::init())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock app builds");

    let handle = app.handle().clone();
    let result = run_sidecar_turn(handle, "hello".into()).await;
    assert!(
        result.is_err(),
        "run_sidecar_turn should fail when the sidecar binary is missing, got: {:?}",
        result
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("sidecar") || err.contains("failed to"),
        "error should mention the sidecar resolution failure, got: {err}"
    );
}

// `emit_agent_event` is generic over `R: tauri::Runtime`; the sidecar path
// and the in-process turn path both feed through it. A direct unit-level
// assertion that the `Done` variant maps to `turn:done` (not, say, silently
// dropped) catches a mapping regression without the subprocess overhead.
#[tokio::test]
async fn emit_agent_event_maps_done_to_turn_done_event() {
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock app builds");

    use tauri::Listener;
    let received = Arc::new(Mutex::new(None));
    let received_clone = received.clone();
    app.listen("turn:done", move |event| {
        let payload: TurnDoneEvent =
            serde_json::from_str(event.payload()).expect("payload deserializes");
        *received_clone.lock().unwrap() = Some(payload);
    });

    let done_event = AgentEvent::Done {
        message_id: "msg-1".into(),
        final_message: Some("hello".into()),
        stop_reason: None,
        turns: Some(1),
        token_count: Some(42),
        prefill_estimates: None,
        prompt_latency_ms: None,
        tier2_ms: None,
        tier1_fires: None,
        retrieve_calls: None,
        tier2_fires: None,
        cache_hit_tokens: Some(10),
        cache_miss_tokens: Some(32),
        breakdown: None,
        usage: None,
        budget_tokens: Some(160_000),
    };
    emit_agent_event(app.handle(), "session-1", done_event);

    // The mock runtime may deliver events asynchronously; yield once.
    tokio::task::yield_now().await;

    let received = received.lock().unwrap().clone();
    let payload = received.expect("turn:done event was not delivered");
    assert_eq!(payload.session_id, "session-1");
    assert_eq!(payload.message_id, "msg-1");
}

// ── Files panel commands (#872) ──────────────────────────────────────────────

fn write_file(dir: &std::path::Path, rel: &str, body: &[u8]) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, body).unwrap();
}

#[test]
fn list_directory_sorts_dirs_first_then_alphabetical() {
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "zebra.txt", b"z");
    write_file(dir.path(), "Alpha.txt", b"a");
    write_file(dir.path(), "src/main.rs", b"");
    std::fs::create_dir_all(dir.path().join("assets")).unwrap();

    let entries = list_directory_in(dir.path(), "").unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    // Directories first (case-insensitive alphabetical), then files.
    assert_eq!(names, vec!["assets", "src", "Alpha.txt", "zebra.txt"]);
    assert!(entries[0].is_dir && entries[1].is_dir);
    assert_eq!(entries[3].size, 1); // "z"
}

#[test]
fn list_directory_respects_gitignore() {
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), ".gitignore", b"node_modules/\ntarget/\n");
    write_file(dir.path(), "src/lib.rs", b"");
    write_file(dir.path(), "node_modules/pkg/index.js", b"");
    write_file(dir.path(), "target/debug.log", b"");

    let names: Vec<String> = list_directory_in(dir.path(), "")
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert!(names.contains(&"src".to_string()));
    assert!(!names.contains(&"node_modules".to_string()), "{names:?}");
    assert!(!names.contains(&"target".to_string()), "{names:?}");
}

#[test]
fn list_directory_rejects_jail_escape() {
    let dir = tempfile::tempdir().unwrap();
    let err = list_directory_in(dir.path(), "../").unwrap_err();
    assert!(err.contains("access denied"), "{err}");
}

#[test]
fn read_file_returns_utf8_text() {
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "hello.txt", b"hello world");
    let fc = read_file_in(dir.path(), "hello.txt", None).unwrap();
    assert_eq!(fc.text.as_deref(), Some("hello world"));
    assert!(!fc.is_binary);
    assert!(!fc.truncated);
    assert_eq!(fc.size, 11);
}

#[test]
fn read_file_flags_binary() {
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "blob.bin", &[0xff, 0xfe, 0x00, 0x01]);
    let fc = read_file_in(dir.path(), "blob.bin", None).unwrap();
    assert!(fc.is_binary);
    assert!(fc.text.is_none());
    assert_eq!(fc.size, 4);
}

#[test]
fn read_file_truncates_to_cap() {
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "big.txt", b"abcdefghij"); // 10 bytes
    let fc = read_file_in(dir.path(), "big.txt", Some(4)).unwrap();
    assert!(fc.truncated);
    assert_eq!(fc.text.as_deref(), Some("abcd"));
    assert_eq!(fc.size, 10);
    assert!(!fc.is_binary);
}

#[test]
fn read_file_truncation_mid_multibyte_char_stays_text() {
    let dir = tempfile::tempdir().unwrap();
    // "é" is 2 bytes (0xC3 0xA9); cap at 2 lands between "a" and the é bytes...
    write_file(dir.path(), "accent.txt", "aé".as_bytes()); // 3 bytes total
    let fc = read_file_in(dir.path(), "accent.txt", Some(2)).unwrap();
    assert!(fc.truncated);
    assert!(
        !fc.is_binary,
        "mid-char truncation must not be flagged binary"
    );
    assert_eq!(fc.text.as_deref(), Some("a"));
}

#[test]
fn read_file_rejects_jail_escape() {
    let dir = tempfile::tempdir().unwrap();
    let err = read_file_in(dir.path(), "../secret.txt", None).unwrap_err();
    assert!(err.contains("access denied"), "{err}");
}
