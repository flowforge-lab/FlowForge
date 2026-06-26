//! Anti-rot guard for the shipped `docs/examples/codon` content (#235 B1/B2).
//!
//! The codon phenotype and the codegraph skill are version-controlled examples
//! users copy into `~/.flowforge/`. This test loads them through the real loaders
//! so the files cannot drift out of the formats `load_phenotypes` / `SkillRegistry`
//! accept, and asserts the codon -> codegraph DNA link the activation warn (#235 B3)
//! depends on.

use std::path::PathBuf;

use ff_skills::{load_phenotypes, SkillRegistry};

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("docs/examples/codon")
}

#[test]
fn codon_phenotype_loads_and_declares_codegraph() {
    let (phenos, errors) = load_phenotypes(&examples_dir().join("phenos"));
    assert!(errors.is_empty(), "codon.toml failed to load: {errors:?}");

    let codon = phenos
        .get("codon")
        .expect("codon phenotype present (file stem is the name)");
    assert_eq!(
        codon.skills,
        vec!["codegraph".to_string()],
        "codon must activate the codegraph skill"
    );
    assert!(
        codon
            .persona
            .as_deref()
            .is_some_and(|p| !p.trim().is_empty()),
        "codon must carry a non-empty persona"
    );
    assert_eq!(
        codon.max_iterations,
        Some(25),
        "codon raises the iteration cap to 25 for long verify loops"
    );
}

#[test]
fn codegraph_skill_loads_and_requires_the_codegraph_server() {
    let (registry, errors) = SkillRegistry::load_dir(&examples_dir().join("skills"));
    assert!(
        errors.is_empty(),
        "codegraph SKILL.md failed to load: {errors:?}"
    );

    let skill = registry
        .get("codegraph")
        .expect("codegraph skill present in the examples");
    assert_eq!(
        skill.manifest.mcp,
        vec!["codegraph".to_string()],
        "the codegraph skill must declare the codegraph MCP server as its DNA"
    );
    assert!(
        !skill.body.trim().is_empty(),
        "the codegraph skill must document its tools"
    );
}

#[test]
fn pr_review_skill_loads_and_stays_diff_scoped() {
    // #426 RC2: a review skill for the codon ecosystem that overrides the
    // persona's "codegraph first" push and keeps the agent scoped to the diff.
    let (registry, errors) = SkillRegistry::load_dir(&examples_dir().join("skills"));
    assert!(errors.is_empty(), "skills failed to load: {errors:?}");

    let skill = registry
        .get("pr-review")
        .expect("pr-review skill present in the examples");
    assert!(
        skill.manifest.mcp.is_empty(),
        "pr-review must not declare MCP dependencies -- it is pure guidance"
    );
    assert!(
        skill.body.contains("overrides") && skill.body.contains("codegraph first"),
        "pr-review must explicitly override the codon codegraph-first guidance"
    );
    assert!(
        skill.body.contains("unified diff"),
        "pr-review must tell the agent to fetch the diff"
    );
    assert!(
        skill.body.contains("Do not spider the call graph"),
        "pr-review must forbid call-graph traversal during a review"
    );
}
