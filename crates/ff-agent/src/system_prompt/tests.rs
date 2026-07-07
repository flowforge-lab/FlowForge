use super::*;
use ff_core::{Skill, SkillManifest};
use std::path::PathBuf;

fn ctx() -> UserContext {
    UserContext {
        local_date: "2026-06-13".into(),
        timezone: "America/Chicago".into(),
        time_of_day: TimeOfDay::Evening,
        working_dir: String::new(),
    }
}

fn skill(name: &str, desc: &str, body: &str) -> Skill {
    Skill {
        manifest: SkillManifest {
            name: name.into(),
            description: desc.into(),
            version: "0.1.0".into(),
            author: None,
            tools: vec![],
            mcp: vec![],
            keywords: vec![],
        },
        body: body.into(),
        path: PathBuf::from(format!("/skills/{name}")),
    }
}

fn registry(skills: Vec<Skill>) -> SkillRegistry {
    let dir = tempfile::tempdir().unwrap();
    for s in &skills {
        let d = dir.path().join(&s.manifest.name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("SKILL.md"),
            format!(
                "---\nname: {}\ndescription: {}\nversion: {}\n---\n{}\n",
                s.manifest.name, s.manifest.description, s.manifest.version, s.body
            ),
        )
        .unwrap();
    }
    let (reg, errs) = SkillRegistry::load_dir(dir.path());
    assert!(errs.is_empty(), "{errs:?}");
    reg
}

#[test]
fn plan_mode_appends_a_plan_steer() {
    let reg = SkillRegistry::new();
    let out = build_system_prompt(None, &reg, &[], &ctx(), None, None, Mode::Plan);
    assert!(out.contains("## Mode: Plan"), "{out}");
    assert!(out.contains("Read-only tools run freely"), "{out}");
}

#[test]
fn mode_steer_precedes_skills_in_prompt() {
    // #828: mode steer must be in the high-attention prefix (before skills),
    // not buried after thousands of tokens of instructions and memory.
    let reg = registry(vec![skill("test-skill", "A test", "body")]);
    let out = build_system_prompt(None, &reg, &[], &ctx(), None, None, Mode::Plan);
    let mode_pos = out.find("## Mode: Plan").expect("mode steer missing");
    let skills_pos = out
        .find("## Available skills")
        .expect("skills section missing");
    assert!(
        mode_pos < skills_pos,
        "mode steer must appear before skills; mode at {mode_pos}, skills at {skills_pos}"
    );
}

#[test]
fn includes_the_large_file_writes_steer() {
    // #550: steer large file creation toward chunked write / edit so a giant
    // single `write` argument is not truncated at the output cap.
    let reg = SkillRegistry::new();
    let out = build_system_prompt(None, &reg, &[], &ctx(), None, None, Mode::default());
    assert!(out.contains("## Large file writes"), "{out}");
    assert!(out.contains("append the rest in chunks"), "{out}");
}

#[test]
fn every_mode_appends_a_steer() {
    let reg = SkillRegistry::new();
    for mode in [Mode::Plan, Mode::Auto, Mode::Act] {
        let out = build_system_prompt(None, &reg, &[], &ctx(), None, None, mode);
        assert!(
            out.contains("## Mode:"),
            "{mode:?} should add a mode steer: {out}"
        );
    }
}

#[test]
fn auto_mode_steer_states_the_tier_boundaries() {
    let steer = mode_steer(Mode::Auto).expect("Auto has a steer");
    assert!(steer.contains("## Mode: Auto"), "{steer}");
    // Local writes auto-run, Sensitive prompts, Dangerous is denied.
    assert!(steer.contains("auto-approved"), "{steer}");
    assert!(steer.contains("confirmation"), "{steer}");
    assert!(steer.contains("denied"), "{steer}");
    // The Sensitive-tier definition is spelled out so the agent can classify.
    assert!(steer.contains("externally-visible side effects"), "{steer}");
}

#[test]
fn act_mode_steer_confirms_only_dangerous() {
    let steer = mode_steer(Mode::Act).expect("Act has a steer");
    assert!(steer.contains("## Mode: Act"), "{steer}");
    assert!(steer.contains("Sensitive"), "{steer}");
    assert!(steer.contains("auto-approved"), "{steer}");
    // Only Dangerous still needs confirmation in Act.
    assert!(steer.contains("Dangerous"), "{steer}");
    assert!(steer.contains("confirmation"), "{steer}");
}

#[test]
fn includes_user_context_from_supplied_clock() {
    let reg = SkillRegistry::new();
    let out = build_system_prompt(None, &reg, &[], &ctx(), None, None, Mode::default());
    assert!(out.contains("## User context"));
    assert!(
        out.contains("Current: 2026-06-13, evening (America/Chicago)."),
        "{out}"
    );
}

#[test]
fn time_of_day_bands_cover_every_boundary() {
    // RFC 0008 §6: Morning 05–11, Afternoon 12–16, Evening 17–20, Night 21–04.
    assert_eq!(TimeOfDay::from_hour(4), TimeOfDay::Night);
    assert_eq!(TimeOfDay::from_hour(5), TimeOfDay::Morning);
    assert_eq!(TimeOfDay::from_hour(11), TimeOfDay::Morning);
    assert_eq!(TimeOfDay::from_hour(12), TimeOfDay::Afternoon);
    assert_eq!(TimeOfDay::from_hour(16), TimeOfDay::Afternoon);
    assert_eq!(TimeOfDay::from_hour(17), TimeOfDay::Evening);
    assert_eq!(TimeOfDay::from_hour(20), TimeOfDay::Evening);
    assert_eq!(TimeOfDay::from_hour(21), TimeOfDay::Night);
    assert_eq!(TimeOfDay::from_hour(0), TimeOfDay::Night);
    assert_eq!(TimeOfDay::from_hour(23), TimeOfDay::Night);
}

#[test]
fn time_of_day_labels_are_lowercase() {
    assert_eq!(TimeOfDay::Morning.label(), "morning");
    assert_eq!(TimeOfDay::Afternoon.label(), "afternoon");
    assert_eq!(TimeOfDay::Evening.label(), "evening");
    assert_eq!(TimeOfDay::Night.label(), "night");
}

#[test]
fn user_context_renders_time_of_day_band() {
    let reg = SkillRegistry::new();
    let mut user = ctx();
    user.time_of_day = TimeOfDay::Morning;
    let out = build_system_prompt(None, &reg, &[], &user, None, None, Mode::default());
    assert!(
        out.contains("Current: 2026-06-13, morning (America/Chicago)."),
        "{out}"
    );
}

#[test]
fn now_captures_a_valid_band() {
    let band = UserContext::now().time_of_day;
    assert!(matches!(
        band,
        TimeOfDay::Morning | TimeOfDay::Afternoon | TimeOfDay::Evening | TimeOfDay::Night
    ));
}

#[test]
fn user_context_is_placed_last() {
    let reg = registry(vec![skill("alpha", "A things", "abody")]);
    let out = build_system_prompt(
        Some("You are a coding assistant."),
        &reg,
        &["alpha".into()],
        &ctx(),
        None,
        None,
        Mode::default(),
    );
    let persona = out.find("You are a coding assistant.").unwrap();
    let available = out.find("## Available skills").unwrap();
    let active = out.find("## Active skill instructions").unwrap();
    let user = out.find("## User context").unwrap();
    assert!(
        persona < available && available < active && active < user,
        "user context must come last for cache stability: {out}"
    );
}

#[test]
fn persona_is_prepended_when_set_and_absent_when_none() {
    let reg = SkillRegistry::new();
    let with = build_system_prompt(
        Some("You are a coding assistant."),
        &reg,
        &[],
        &ctx(),
        None,
        None,
        Mode::default(),
    );
    assert!(with.starts_with("You are a coding assistant.\n\n"));
    let without = build_system_prompt(None, &reg, &[], &ctx(), None, None, Mode::default());
    assert!(!without.contains("You are a coding assistant"));
    assert!(without.contains("## Compacted tool results"));
}

#[test]
fn blank_persona_is_ignored() {
    let reg = SkillRegistry::new();
    let out = build_system_prompt(
        Some("   \n  "),
        &reg,
        &[],
        &ctx(),
        None,
        None,
        Mode::default(),
    );
    assert!(
        !out.starts_with("You are"),
        "blank persona should not appear"
    );
    assert!(out.contains("## Compacted tool results"));
}

#[test]
fn lists_installed_descriptions_sorted() {
    let reg = registry(vec![
        skill("zeta", "Z things", "zbody"),
        skill("alpha", "A things", "abody"),
    ]);
    let out = build_system_prompt(None, &reg, &[], &ctx(), None, None, Mode::default());
    assert!(out.contains("## Available skills"));
    let a = out.find("- alpha: A things").unwrap();
    let z = out.find("- zeta: Z things").unwrap();
    assert!(a < z, "skills not sorted: {out}");
}

#[test]
fn active_bodies_only_for_active_skills() {
    let reg = registry(vec![
        skill("rust-debug", "Debug Rust", "Use bash and view to bisect."),
        skill("idle", "Unused", "SHOULD_NOT_APPEAR"),
    ]);
    let out = build_system_prompt(
        None,
        &reg,
        &["rust-debug".into()],
        &ctx(),
        None,
        None,
        Mode::default(),
    );
    assert!(out.contains("## Active skill instructions"));
    assert!(out.contains("### rust-debug"));
    assert!(out.contains("Use bash and view to bisect."));
    assert!(
        !out.contains("SHOULD_NOT_APPEAR"),
        "inactive body leaked: {out}"
    );
    // Description of the inactive skill still appears in Available skills.
    assert!(out.contains("- idle: Unused"));
}

#[test]
fn working_dir_renders_when_set_and_is_absent_when_empty() {
    let reg = registry(vec![skill("a", "desc", "body")]);
    let with = build_system_prompt(
        None,
        &reg,
        &[],
        &ctx().with_working_dir("/Users/me/projects/flowforge_abid"),
        None,
        None,
        Mode::default(),
    );
    assert!(
        with.contains("Working directory: /Users/me/projects/flowforge_abid"),
        "{with}"
    );
    let without = build_system_prompt(None, &reg, &[], &ctx(), None, None, Mode::default());
    assert!(!without.contains("Working directory:"), "{without}");
}

#[test]
fn no_active_section_when_none_active() {
    let reg = registry(vec![skill("a", "desc", "body")]);
    let out = build_system_prompt(None, &reg, &[], &ctx(), None, None, Mode::default());
    assert!(!out.contains("## Active skill instructions"), "{out}");
}

#[test]
fn unknown_active_name_is_skipped() {
    let reg = registry(vec![skill("a", "desc", "body")]);
    let out = build_system_prompt(
        None,
        &reg,
        &["ghost".into()],
        &ctx(),
        None,
        None,
        Mode::default(),
    );
    assert!(!out.contains("## Active skill instructions"), "{out}");
}

#[test]
fn memory_block_is_appended_after_user_context() {
    let reg = SkillRegistry::new();
    let mem = "## Memory\n\nUser prefers Rust.";
    let out = build_system_prompt(None, &reg, &[], &ctx(), Some(mem), None, Mode::default());
    let user = out.find("## User context").unwrap();
    let memory = out.find("## Memory").unwrap();
    assert!(user < memory, "memory must follow user context: {out}");
    assert!(out.contains("User prefers Rust."));
}

#[test]
fn none_or_blank_memory_adds_nothing() {
    let reg = SkillRegistry::new();
    let without = build_system_prompt(None, &reg, &[], &ctx(), None, None, Mode::default());
    assert!(!without.contains("## Memory"));
    let blank = build_system_prompt(
        None,
        &reg,
        &[],
        &ctx(),
        Some("   \n  "),
        None,
        Mode::default(),
    );
    assert!(!blank.contains("## Memory"));
}

#[test]
fn review_scoping_guidance_is_in_the_stable_prefix() {
    // #426 RC2: the agent over-explored during PR reviews (PR #452) because the
    // system prompt carried zero diff-scoping guidance. The review-scoping
    // section must sit in the cache-stable prefix -- after the other stable
    // guidance and before the volatile User context -- so it is always present
    // and never falls out of the prompt.
    let reg = SkillRegistry::new();
    let out = build_system_prompt(None, &reg, &[], &ctx(), None, None, Mode::default());
    let shell = out.find("## Shell environment").unwrap();
    let review = out
        .find("## Reviewing pull requests")
        .expect("review-scoping guidance present");
    let user = out.find("## User context").unwrap();
    assert!(
        shell < review && review < user,
        "review guidance must sit in the stable prefix, after Shell environment \
         and before User context: {out}"
    );
    // The four load-bearing invariants from the issue's Fix.
    assert!(
        out.contains("unified diff"),
        "must say to fetch the diff once"
    );
    assert!(
        out.contains("changed hunks"),
        "must scope reasoning to hunks"
    );
    assert!(
        out.contains("Do not spider the call graph"),
        "must forbid call-graph spidering"
    );
    assert!(
        out.contains("Reuse those single results"),
        "must tell the agent to reuse the single fetch"
    );
    assert!(
        out.contains("application/vnd.github.diff"),
        "must offer a gh-free diff fetch path"
    );
    assert!(
        out.contains(".../pulls/<n>/files"),
        "must name the heavy file-listing anti-pattern"
    );
}

#[test]
fn compaction_guidance_forbids_reproducing_markers() {
    // A weaker model regurgitated [N lines elided] / [compacted; retrieve
    // key=...] placeholders as its answer instead of calling
    // compaction_retrieve (see #512, #783). The guidance must explicitly
    // forbid copying the markers into the reply AND prohibit the model from
    // abbreviating its own output using similar patterns.
    let reg = SkillRegistry::new();
    let out = build_system_prompt(None, &reg, &[], &ctx(), None, None, Mode::default());
    assert!(
        out.contains("never copy them into"),
        "must forbid reproducing compaction markers: {out}"
    );
    assert!(
        out.contains("Your own replies must always be complete"),
        "must prohibit model self-abbreviation (#783): {out}"
    );
}

// ── Goal injection (#718) ────────────────────────────────────────────────

fn active_goal() -> Goal {
    use ff_core::{GoalBudget, GoalLedgerEntry, GoalSpend, StepStatus};
    Goal {
        session_id: "s1".into(),
        objective: "Ship the prefix cache PR".into(),
        status: GoalStatus::Active,
        iteration: 2,
        budget: GoalBudget {
            max_iterations: 25,
            max_tokens: None,
            max_wall_ms: None,
        },
        spent: GoalSpend::default(),
        ledger: vec![
            GoalLedgerEntry {
                id: "step-1".into(),
                status: StepStatus::Done,
                claim: "Add cache_messages field".into(),
                action: Some("edited lib.rs".into()),
                evidence: vec!["cargo check passed".into()],
                verdict: Some(ff_core::Verdict::Match),
                next: None,
                created_ms: 0,
                updated_ms: 0,
            },
            GoalLedgerEntry {
                id: "step-2".into(),
                status: StepStatus::Active,
                claim: "Wire breakpoints in anthropic.rs".into(),
                action: None,
                evidence: vec![],
                verdict: None,
                next: None,
                created_ms: 0,
                updated_ms: 0,
            },
        ],
        pending_steer: None,
        created_ms: 0,
        updated_ms: 0,
    }
}

#[test]
fn goal_block_present_when_active() {
    let reg = SkillRegistry::new();
    let goal = active_goal();
    let out = build_system_prompt(None, &reg, &[], &ctx(), None, Some(&goal), Mode::default());
    assert!(out.contains("## Active goal (iteration 3 of 25)"), "{out}");
    assert!(out.contains("Objective: Ship the prefix cache PR"), "{out}");
    assert!(out.contains("Add cache_messages field [done]"), "{out}");
    assert!(
        out.contains("Wire breakpoints in anthropic.rs [pending]"),
        "{out}"
    );
    assert!(out.contains("call `goal_complete`"), "{out}");
}

#[test]
fn goal_block_absent_when_none() {
    let reg = SkillRegistry::new();
    let out = build_system_prompt(None, &reg, &[], &ctx(), None, None, Mode::default());
    assert!(!out.contains("## Active goal"), "{out}");
}

#[test]
fn goal_block_absent_when_completed() {
    let reg = SkillRegistry::new();
    let mut goal = active_goal();
    goal.status = GoalStatus::Completed;
    let out = build_system_prompt(None, &reg, &[], &ctx(), None, Some(&goal), Mode::default());
    assert!(!out.contains("## Active goal"), "{out}");
}

#[test]
fn goal_block_includes_pending_steer() {
    let reg = SkillRegistry::new();
    let mut goal = active_goal();
    goal.pending_steer = Some("Focus on the Bedrock path first".into());
    let out = build_system_prompt(None, &reg, &[], &ctx(), None, Some(&goal), Mode::default());
    assert!(
        out.contains("User steer: Focus on the Bedrock path first"),
        "{out}"
    );
}
