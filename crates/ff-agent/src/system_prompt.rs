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

use ff_core::Mode;
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
        }
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

    out.push_str("## User context\n");
    out.push_str(&format!(
        "Current: {}, {} ({}).\n",
        user.local_date,
        user.time_of_day.label(),
        user.timezone
    ));

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

    if let Some(steer) = mode_steer(mode) {
        out.push('\n');
        out.push_str(steer);
        out.push('\n');
    }

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
        let out = build_system_prompt(None, &reg, &[], &ctx(), None, Mode::Plan);
        assert!(out.contains("## Mode: Plan"), "{out}");
        assert!(out.contains("Only read-only tools"), "{out}");
    }

    #[test]
    fn act_and_auto_modes_add_no_steer() {
        let reg = SkillRegistry::new();
        for mode in [Mode::Act, Mode::Auto] {
            let out = build_system_prompt(None, &reg, &[], &ctx(), None, mode);
            assert!(
                !out.contains("## Mode:"),
                "{mode:?} should add no mode steer: {out}"
            );
        }
    }

    #[test]
    fn includes_user_context_from_supplied_clock() {
        let reg = SkillRegistry::new();
        let out = build_system_prompt(None, &reg, &[], &ctx(), None, Mode::default());
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
        let out = build_system_prompt(None, &reg, &[], &user, None, Mode::default());
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
            Mode::default(),
        );
        assert!(with.starts_with("You are a coding assistant.\n\n"));
        let without = build_system_prompt(None, &reg, &[], &ctx(), None, Mode::default());
        assert!(without.starts_with("## User context"));
    }

    #[test]
    fn blank_persona_is_ignored() {
        let reg = SkillRegistry::new();
        let out = build_system_prompt(Some("   \n  "), &reg, &[], &ctx(), None, Mode::default());
        assert!(out.starts_with("## User context"), "{out}");
    }

    #[test]
    fn lists_installed_descriptions_sorted() {
        let reg = registry(vec![
            skill("zeta", "Z things", "zbody"),
            skill("alpha", "A things", "abody"),
        ]);
        let out = build_system_prompt(None, &reg, &[], &ctx(), None, Mode::default());
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
    fn no_active_section_when_none_active() {
        let reg = registry(vec![skill("a", "desc", "body")]);
        let out = build_system_prompt(None, &reg, &[], &ctx(), None, Mode::default());
        assert!(!out.contains("## Active skill instructions"), "{out}");
    }

    #[test]
    fn unknown_active_name_is_skipped() {
        let reg = registry(vec![skill("a", "desc", "body")]);
        let out = build_system_prompt(None, &reg, &["ghost".into()], &ctx(), None, Mode::default());
        assert!(!out.contains("## Active skill instructions"), "{out}");
    }

    #[test]
    fn memory_block_is_appended_after_user_context() {
        let reg = SkillRegistry::new();
        let mem = "## Memory\n\nUser prefers Rust.";
        let out = build_system_prompt(None, &reg, &[], &ctx(), Some(mem), Mode::default());
        let user = out.find("## User context").unwrap();
        let memory = out.find("## Memory").unwrap();
        assert!(user < memory, "memory must follow user context: {out}");
        assert!(out.contains("User prefers Rust."));
    }

    #[test]
    fn none_or_blank_memory_adds_nothing() {
        let reg = SkillRegistry::new();
        let without = build_system_prompt(None, &reg, &[], &ctx(), None, Mode::default());
        assert!(!without.contains("## Memory"));
        let blank = build_system_prompt(None, &reg, &[], &ctx(), Some("   \n  "), Mode::default());
        assert!(!blank.contains("## Memory"));
    }
}
