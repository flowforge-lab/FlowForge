use super::*;
use std::path::PathBuf;

use ff_core::Skill;

fn skill(name: &str, description: &str, keywords: &[&str]) -> Skill {
    Skill {
        manifest: SkillManifest {
            name: name.into(),
            description: description.into(),
            version: "0.1.0".into(),
            author: None,
            tools: vec![],
            mcp: vec![],
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
        },
        body: String::new(),
        path: PathBuf::from("/tmp").join(name),
    }
}

fn registry(skills: Vec<Skill>) -> SkillRegistry {
    let mut reg = SkillRegistry::new();
    for s in skills {
        reg.insert_for_test(s);
    }
    reg
}

#[test]
fn empty_query_lists_all_name_sorted() {
    let reg = registry(vec![
        skill("zeta", "z", &[]),
        skill("alpha", "a", &[]),
        skill("mid", "m", &[]),
    ]);
    let names: Vec<_> = search_skills(&reg, "")
        .iter()
        .map(|h| h.manifest.name.as_str())
        .collect();
    assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    assert!(search_skills(&reg, "   ").len() == 3);
}

#[test]
fn ranks_keyword_over_name_over_description() {
    let reg = registry(vec![
        skill("debugger", "helps with rust troubleshooting", &[]),
        skill("crusty-tools", "misc", &[]),
        skill("triage", "incident triage", &["rust"]),
        skill("rust-helper", "misc", &[]),
    ]);
    let names: Vec<_> = search_skills(&reg, "rust")
        .iter()
        .map(|h| h.manifest.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["triage", "rust-helper", "crusty-tools", "debugger"]
    );
}

#[test]
fn non_matching_query_returns_empty() {
    let reg = registry(vec![skill("alpha", "a", &["x"])]);
    assert!(search_skills(&reg, "nomatch").is_empty());
}

#[test]
fn tie_break_is_by_name() {
    let reg = registry(vec![
        skill("rust-z", "misc", &[]),
        skill("rust-a", "misc", &[]),
    ]);
    let names: Vec<_> = search_skills(&reg, "rust")
        .iter()
        .map(|h| h.manifest.name.as_str())
        .collect();
    assert_eq!(names, vec!["rust-a", "rust-z"]);
}

#[test]
fn search_is_case_insensitive() {
    let reg = registry(vec![skill("Rust-Helper", "Systematic RUST debugging", &[])]);
    assert_eq!(search_skills(&reg, "RUST").len(), 1);
    assert_eq!(search_skills(&reg, "rust").len(), 1);
}
