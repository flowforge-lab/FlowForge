//! System-prompt construction (RFC 0001 §4, RFC 0002 phase 1).
//!
//! [`run_turn`](crate::run_turn) injects a single leading system message built
//! here from the active phenotype persona, the installed skills, and an ambient
//! [`UserContext`]. The host computes the inputs; this module is pure string
//! assembly so the result is deterministic and testable.

use ff_skills::SkillRegistry;

/// Ambient, zero-permission context handed to the model so it stops assuming its
/// training-cutoff date. M3.1b scope is **time only** (RFC 0002 phase 1); location
/// is a separate post-M3 track. The fields are preformatted strings so the prompt
/// builder stays pure — [`UserContext::now`] does the clock/timezone lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserContext {
    /// Local date and time, e.g. `2026-06-13 14:05`.
    pub local_datetime: String,
    /// IANA timezone name, e.g. `America/Chicago`.
    pub timezone: String,
}

impl UserContext {
    /// Capture the current local time and IANA timezone from the host clock.
    pub fn now() -> Self {
        let local_datetime = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
        let timezone =
            iana_time_zone::get_timezone().unwrap_or_else(|_| "unknown timezone".to_string());
        Self {
            local_datetime,
            timezone,
        }
    }
}

/// Build the system prompt prepended to every turn's request.
///
/// Sections, in order: optional `persona`, the ambient [`UserContext`], the
/// `description` of every installed skill (for discovery), and the full `body` of
/// each skill named in `active` (resolved against `skills`). Skill listings are
/// sorted by name so the output is deterministic.
pub fn build_system_prompt(
    persona: Option<&str>,
    skills: &SkillRegistry,
    active: &[String],
    user: &UserContext,
) -> String {
    let mut out = String::new();

    if let Some(persona) = persona {
        let persona = persona.trim();
        if !persona.is_empty() {
            out.push_str(persona);
            out.push_str("\n\n");
        }
    }

    out.push_str("## User context\n");
    out.push_str(&format!(
        "Current date and time: {} ({}).\n",
        user.local_datetime, user.timezone
    ));

    let mut installed: Vec<_> = skills.list().collect();
    installed.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    if !installed.is_empty() {
        out.push_str("\n## Available skills\n");
        for skill in &installed {
            out.push_str(&format!(
                "- {}: {}\n",
                skill.manifest.name, skill.manifest.description
            ));
        }
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
        out.push_str("\n## Active skill instructions");
        out.push_str(&active_section);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ff_core::{Skill, SkillManifest};
    use std::path::PathBuf;

    fn ctx() -> UserContext {
        UserContext {
            local_datetime: "2026-06-13 14:05".into(),
            timezone: "America/Chicago".into(),
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
    fn includes_user_context_from_supplied_clock() {
        let reg = SkillRegistry::new();
        let out = build_system_prompt(None, &reg, &[], &ctx());
        assert!(out.contains("## User context"));
        assert!(
            out.contains("Current date and time: 2026-06-13 14:05 (America/Chicago)."),
            "{out}"
        );
    }

    #[test]
    fn persona_is_prepended_when_set_and_absent_when_none() {
        let reg = SkillRegistry::new();
        let with = build_system_prompt(Some("You are Akisa."), &reg, &[], &ctx());
        assert!(with.starts_with("You are Akisa.\n\n"));
        let without = build_system_prompt(None, &reg, &[], &ctx());
        assert!(without.starts_with("## User context"));
    }

    #[test]
    fn blank_persona_is_ignored() {
        let reg = SkillRegistry::new();
        let out = build_system_prompt(Some("   \n  "), &reg, &[], &ctx());
        assert!(out.starts_with("## User context"), "{out}");
    }

    #[test]
    fn lists_installed_descriptions_sorted() {
        let reg = registry(vec![
            skill("zeta", "Z things", "zbody"),
            skill("alpha", "A things", "abody"),
        ]);
        let out = build_system_prompt(None, &reg, &[], &ctx());
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
        let out = build_system_prompt(None, &reg, &["rust-debug".into()], &ctx());
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
        let out = build_system_prompt(None, &reg, &[], &ctx());
        assert!(!out.contains("## Active skill instructions"), "{out}");
    }

    #[test]
    fn unknown_active_name_is_skipped() {
        let reg = registry(vec![skill("a", "desc", "body")]);
        let out = build_system_prompt(None, &reg, &["ghost".into()], &ctx());
        assert!(!out.contains("## Active skill instructions"), "{out}");
    }
}
