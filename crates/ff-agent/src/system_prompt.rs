//! System-prompt construction (RFC 0001 §4, RFC 0002 phase 1).
//!
//! [`run_turn`](crate::run_turn) injects a single leading system message built
//! here from the active phenotype persona, the installed skills, and an ambient
//! [`UserContext`]. The host computes the inputs; this module is pure string
//! assembly so the result is deterministic and testable.
//!
//! Section order is chosen to maximize server-side prefix-cache reuse: the
//! stable parts (persona, skill listings, active instructions) come first, and
//! the ambient [`UserContext`] — the only part that changes day to day — comes
//! last. The clock is also coarsened to date granularity so the entire prompt
//! is byte-stable across a session, letting the inference server reuse the KV
//! cache for the system prompt (and the tools block that follows it) on every
//! turn after the first.

use ff_core::{Goal, GoalStatus, Mode};
use ff_skills::SkillRegistry;

/// Coarse time-of-day band for the ambient context (RFC 0008 §6). A *band*, not a
/// timestamp: it transitions at most a few times per session, so it adds
/// human-meaningful "evening" awareness without busting the system prompt's
/// prefix cache the way minute-precision would. It is situational context only —
/// never a directive the agent gates behavior on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeOfDay {
    Morning,
    Afternoon,
    Evening,
    Night,
}

impl TimeOfDay {
    /// Map a local-clock hour (0–23) to its band (RFC 0008 §6):
    /// Morning 05:00–11:59, Afternoon 12:00–16:59, Evening 17:00–20:59,
    /// Night 21:00–04:59. Pure so the bands are testable without a clock.
    pub fn from_hour(hour: u32) -> Self {
        match hour % 24 {
            5..=11 => TimeOfDay::Morning,
            12..=16 => TimeOfDay::Afternoon,
            17..=20 => TimeOfDay::Evening,
            _ => TimeOfDay::Night,
        }
    }

    /// Lowercase label used in the ambient render, e.g. `"evening"`.
    pub fn label(self) -> &'static str {
        match self {
            TimeOfDay::Morning => "morning",
            TimeOfDay::Afternoon => "afternoon",
            TimeOfDay::Evening => "evening",
            TimeOfDay::Night => "night",
        }
    }
}

/// Ambient, zero-permission context handed to the model so it stops assuming its
/// training-cutoff date. M3.1b scope is **time only** (RFC 0002 phase 1); location
/// is a separate post-M3 track. The fields are preformatted strings so the prompt
/// builder stays pure — [`UserContext::now`] does the clock/timezone lookup.
///
/// The clock is captured at **date** granularity (not minutes) on purpose: a
/// finer timestamp would change every turn and bust the inference server's
/// prefix cache for the whole system prompt. Date is enough for the model to
/// reason about "today".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserContext {
    /// Local date, e.g. `2026-06-13`.
    pub local_date: String,
    /// IANA timezone name, e.g. `America/Chicago`.
    pub timezone: String,
    /// Coarse local time-of-day band (RFC 0008 §6).
    pub time_of_day: TimeOfDay,
    /// Absolute path of the session's working directory -- the cwd  runs in
    /// and the root file tools are jailed to. Stated in the prompt so the model
    /// works from the real checkout instead of guessing a path (and prepending a
    /// wrong ). Empty when the host did not supply one; then it is not rendered.
    pub working_dir: String,
}

impl UserContext {
    /// Capture the current local date and IANA timezone from the host clock.
    pub fn now() -> Self {
        use chrono::Timelike;
        let now = chrono::Local::now();
        let local_date = now.format("%Y-%m-%d").to_string();
        let time_of_day = TimeOfDay::from_hour(now.hour());
        let timezone =
            iana_time_zone::get_timezone().unwrap_or_else(|_| "unknown timezone".to_string());
        Self {
            local_date,
            timezone,
            time_of_day,
            working_dir: String::new(),
        }
    }

    /// Attach the session's working directory (absolute path) so the prompt can
    /// state where  runs and file tools are rooted. Builder-style; stable
    /// within a session, so it sits in the volatile tail without busting the
    /// prefix cache any more than the date already does.
    pub fn with_working_dir(mut self, dir: impl Into<String>) -> Self {
        self.working_dir = dir.into();
        self
    }
}

/// Build the system prompt prepended to every turn's request.
///
/// Sections, in order: optional `persona`, the `description` of every installed
/// skill (for discovery), the full `body` of each skill named in `active`
/// (resolved against `skills`), and finally the ambient [`UserContext`]. Skill
/// listings are sorted by name so the output is deterministic. The ambient
/// context is placed last so the stable prefix stays byte-identical across a
/// session for prefix-cache reuse (see module docs).
pub fn build_system_prompt(
    persona: Option<&str>,
    skills: &SkillRegistry,
    active: &[String],
    user: &UserContext,
    memory: Option<&str>,
    goal: Option<&Goal>,
    mode: Mode,
) -> String {
    let mut out = String::new();

    if let Some(persona) = persona {
        let persona = persona.trim();
        if !persona.is_empty() {
            out.push_str(persona);
            out.push_str("\n\n");
        }
    }

    let mut installed: Vec<_> = skills.list().collect();
    installed.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    if !installed.is_empty() {
        out.push_str("## Available skills\n");
        for skill in &installed {
            out.push_str(&format!(
                "- {}: {}\n",
                skill.manifest.name, skill.manifest.description
            ));
        }
        out.push('\n');
    }

    let mut active_sorted: Vec<&String> = active.iter().collect();
    active_sorted.sort();
    let mut active_section = String::new();
    for name in active_sorted {
        if let Some(skill) = skills.get(name) {
            active_section.push_str(&format!("\n### {}\n{}\n", name, skill.body.trim_end()));
        }
    }
    if !active_section.is_empty() {
        out.push_str("## Active skill instructions");
        out.push_str(&active_section);
        out.push('\n');
    }

    // Stable guidance (kept in the cache-stable prefix): large tool results are
    // compacted at ingest (RFC 0016 Tier 1) and carry a retrieve marker, so the
    // model knows it can recover dropped detail on demand.
    out.push_str(
        "## Compacted tool results\n\
         Large tool results are abbreviated to save context and end with a \
         `[compacted; retrieve key=<HEX>]` marker. When you need detail the \
         abbreviation dropped, call `compaction_retrieve` with that key to read \
         the verbatim original. These markers and any `[N lines elided]` \
         placeholders are scaffolding, not content -- never copy them into your \
         reply. If your answer needs that detail, retrieve it first.\n\n",
    );

    // Stable guidance (cache-stable prefix): batching independent tool calls into
    // a single turn lets the agent run them concurrently, collapsing many slow
    // provider round-trips into one. The biggest, most model-agnostic latency win.
    out.push_str(
        "## Batch independent tool calls\n\
         When you need to inspect several files or run independent searches, issue \
         all those tool calls together in a single turn rather than one at a time. \
         Independent read-only calls run concurrently, so batching them is much \
         faster than sequential one-call-per-turn round-trips.\n\n",
    );

    // Stable guidance (cache-stable prefix): shell environment conventions (#458).
    // Pre-empts the two highest-frequency self-inflicted frictions -- a redundant
    // `cd <workspace>` prefix and reaching for a sandbox-denied `/tmp`.
    out.push_str(
        "## Shell environment\n\
         The `bash` tool already runs from the workspace root. Issue bare commands; \
         do not prefix `cd <workspace>` (use the tool's `working_dir` for a \
         subdirectory). For temporary files, use the workspace scratch dir \
         `.ff-scratch/` (created for you) rather than `/tmp`.\n\n",
    );

    // Stable guidance (cache-stable prefix): steer large file creation away from a
    // single giant `write` argument (#550). Tool-call arguments share the model's
    // output budget, so a whole-file `write` can be cut off mid-JSON; chunking or
    // editing the delta keeps each call comfortably within the cap.
    out.push_str(
        "## Large file writes\nTool-call arguments share the model's output-token budget, so a very large `write` (the whole file body is one argument) can be truncated mid-JSON. For a big new file, create it with a short `write`, then append the rest in chunks with `bash` (e.g. a `>>` heredoc). To change an existing file, prefer `edit` or `apply_patch` -- they carry only the delta, not the whole file.\n\n",
    );

    // Stable guidance (cache-stable prefix): PR-review scoping (#426 RC2).
    // Without this the agent over-explored during reviews -- reading entire
    // unchanged files and spidering the call graph (PR #452). Appended
    // unconditionally but phrased as conditional ("When your task is to review
    // ..."), so it is inert on implementation turns yet bounds a review to the
    // changed hunks.
    out.push_str(
        "## Reviewing pull requests\n\
         When your task is to review a pull request or a diff, stay scoped to the \
         change:\n\
         - Fetch what you need once, as compactly as possible:\n\
           - The change itself as a unified diff: `Accept: application/vnd.github.diff` \
         on `.../pulls/<n>` returns the raw diff text (not JSON). If the `gh` CLI is \
         available, `gh pr diff` is equivalent.\n\
           - Title/body and review comments: `.../pulls/<n>` (without the diff media \
         type) and `.../issues/<n>/comments`, or `gh pr view --json title,body,comments` \
         if `gh` is available.\n\
         Reuse those single results for the whole review; do not re-read the same files \
         or re-run the same diff piecemeal across turns.\n\
         - Never request the JSON file listing (`.../pulls/<n>/files`): that payload is \
         many times larger than the diff text, floods the context, and forces \
         compaction that drops the very review you are writing. Use it only if you \
         specifically need per-file metadata the diff cannot give.\n\
         - Reason about the changed hunks first. The diff is the review's subject; \
         everything else is supporting evidence, not the thing under review.\n\
         - Read wider context only when a specific comment or suspected defect \
         requires it -- to confirm a caller's behaviour, a type contract, or a test \
         that should have changed. Before opening a file, name the hunk and the \
         concern it serves.\n\
         - Do not spider the call graph or read entire unchanged files to \
         \"understand the area\". A review verifies the change, not the codebase.\n\n",
    );

    out.push_str("## User context\n");
    out.push_str(&format!(
        "Current: {}, {} ({}).\n",
        user.local_date,
        user.time_of_day.label(),
        user.timezone
    ));
    if !user.working_dir.is_empty() {
        out.push_str(&format!(
            "Working directory: {}\n\
             Shell commands run here and file tools are rooted here; use paths \
             relative to it and do not prepend a  to another directory.\n",
            user.working_dir
        ));
    }

    // Durable memory (RFC 0006) sits in the volatile tail beside the user
    // context: like the date, it changes between sessions, so keeping it after
    // the stable persona/skill prefix preserves prefix-cache reuse for the rest.
    if let Some(memory) = memory {
        let memory = memory.trim();
        if !memory.is_empty() {
            out.push('\n');
            out.push_str(memory);
            out.push('\n');
        }
    }

    if let Some(block) = goal
        .filter(|g| g.status == GoalStatus::Active)
        .map(goal_block)
    {
        out.push('\n');
        out.push_str(&block);
    }

    if let Some(steer) = mode_steer(mode) {
        out.push('\n');
        out.push_str(steer);
        out.push('\n');
    }

    out
}

/// Render the goal-injection block for the system prompt (RFC 0020 §8, #718).
/// Shows the objective, iteration progress, recent ledger entries, and any
/// pending user steer so the agent stays on track and knows when to call
/// `goal_complete`.
fn goal_block(goal: &Goal) -> String {
    use ff_core::Verdict;
    use std::fmt::Write;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "## Active goal (iteration {} of {})",
        goal.iteration + 1,
        goal.budget.max_iterations
    );
    let _ = writeln!(out, "Objective: {}", goal.objective);

    if !goal.ledger.is_empty() {
        out.push_str("Progress so far:\n");
        // Show last 5 ledger entries to keep the block bounded.
        let start = goal.ledger.len().saturating_sub(5);
        for entry in &goal.ledger[start..] {
            let verdict = entry
                .verdict
                .as_ref()
                .map(|v| match v {
                    Verdict::Match => "done",
                    Verdict::Drift => "drift",
                    Verdict::Unverifiable => "unverifiable",
                })
                .unwrap_or("pending");
            let _ = writeln!(out, "- {} [{}]", entry.claim, verdict);
        }
    }

    if let Some(steer) = &goal.pending_steer {
        let _ = writeln!(out, "\nUser steer: {}", steer);
    }

    out.push_str(
        "\nContinue toward the objective. If it is fully met, call `goal_complete`.\n State your reasoning before each action.\n",
    );
    out
}

/// Per-mode behavioural steer appended to the prompt (RFC 0011). Only Plan adds
/// text; Act and Auto rely on the default behaviour, so their prompt is unchanged.
fn mode_steer(mode: Mode) -> Option<&'static str> {
    match mode {
        Mode::Plan => Some(
            "## Mode: Plan\n\nYou are in Plan mode. Only read-only tools are \
             available to you; you cannot edit files, run commands, or otherwise \
             change the world. Investigate the request and produce a clear, concrete \
             plan the user can review. End your turn with that plan. Do not attempt \
             to make changes -- the user will switch you to Act or Auto to execute.",
        ),
        Mode::Act | Mode::Auto => None,
    }
}

/// The system prompt for a pre-compaction memory-flush turn (RFC 0006 §7.2).
///
/// Steers the model to persist only durable, non-obvious facts, and to favor the
/// daily log over `MEMORY.md` so the always-injected curated file stays small and
/// high-signal (RFC 0006 §7.1). `NO_REPLY` is the explicit "nothing worth keeping"
/// escape hatch — the flush turn never surfaces text to the user, so a `NO_REPLY`
/// simply writes nothing.
pub fn build_flush_prompt() -> String {
    "This conversation is about to be summarized and older detail will be lost. Before that happens, persist anything durable using the `memory_write` tool.

Save only facts that should outlive this conversation: stable user preferences, decisions made, identity or project details, and concrete commitments. Write each fact to the daily log (the default `memory_write` target). Reserve `MEMORY.md` for clearly enduring preferences the user asked you to remember — when unsure, use the daily log.

Do NOT save transient chatter, restate the obvious, or duplicate facts already in memory (use `memory_search` first if unsure). If nothing is worth keeping, reply with exactly `NO_REPLY` and write nothing."
        .to_string()
}

#[cfg(test)]
mod tests {
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
        assert!(out.contains("Only read-only tools"), "{out}");
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
    fn act_and_auto_modes_add_no_steer() {
        let reg = SkillRegistry::new();
        for mode in [Mode::Act, Mode::Auto] {
            let out = build_system_prompt(None, &reg, &[], &ctx(), None, None, mode);
            assert!(
                !out.contains("## Mode:"),
                "{mode:?} should add no mode steer: {out}"
            );
        }
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
        assert!(!without.starts_with("You are"));
        assert!(without.starts_with("## Compacted tool results"));
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
        assert!(out.starts_with("## Compacted tool results"), "{out}");
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
        // compaction_retrieve (see #512). The guidance must explicitly forbid
        // copying the markers into the reply.
        let reg = SkillRegistry::new();
        let out = build_system_prompt(None, &reg, &[], &ctx(), None, None, Mode::default());
        assert!(
            out.contains("never copy them into your reply"),
            "must forbid reproducing compaction markers: {out}"
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
}
