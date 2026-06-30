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
        Some(200),
        "codon raises the iteration cap to 200 for long verify loops"
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

// codegraph's real tool surface as of v0.9.7 (verified against `codegraph serve
// --mcp` tools/list). The bundled skill and persona must reference only these
// names -- the binary returns `unknown tool` for anything else, so a phantom tool
// in the docs silently degrades the whole codon phenotype (#573). Update this set
// deliberately when codegraph's advertised tools change.
const CODEGRAPH_TOOLS: [&str; 5] = ["context", "explore", "node", "search", "trace"];

/// Extract the `<stem>` of every `codegraph_<stem>` reference in `text`, where a
/// stem is a run of lowercase ASCII. Handles the bridged `mcp__codegraph__codegraph_X`
/// form (the leading `codegraph__` yields no stem and is skipped).
fn codegraph_tool_stems(text: &str) -> std::collections::BTreeSet<String> {
    const PREFIX: &str = "codegraph_";
    let bytes = text.as_bytes();
    let mut out = std::collections::BTreeSet::new();
    let mut i = 0;
    while let Some(pos) = text[i..].find(PREFIX) {
        let start = i + pos + PREFIX.len();
        let mut j = start;
        while j < bytes.len() && bytes[j].is_ascii_lowercase() {
            j += 1;
        }
        if j > start {
            out.insert(text[start..j].to_string());
        }
        i = (i + pos + 1).max(start);
    }
    out
}

#[test]
fn codegraph_skill_documents_exactly_the_real_tools() {
    // The skill instructs the model which tools to call. A phantom name (e.g. the
    // long-shipped `codegraph_status`/`callers`/`callees`/`impact`/`files`) yields
    // `unknown tool` even when codegraph is healthy; a missing real tool hides a
    // capability. The documented set must equal the real surface exactly (#573).
    let skill = std::fs::read_to_string(examples_dir().join("skills/codegraph/SKILL.md"))
        .expect("read codegraph SKILL.md");
    let documented = codegraph_tool_stems(&skill);
    let expected: std::collections::BTreeSet<String> =
        CODEGRAPH_TOOLS.iter().map(|s| s.to_string()).collect();

    let phantom: Vec<_> = documented.difference(&expected).collect();
    assert!(
        phantom.is_empty(),
        "SKILL.md documents tools codegraph does not advertise: {phantom:?}"
    );
    let missing: Vec<_> = expected.difference(&documented).collect();
    assert!(
        missing.is_empty(),
        "SKILL.md omits real codegraph tools: {missing:?}"
    );
}

#[test]
fn pr_review_skill_names_only_real_codegraph_tools() {
    // pr-review forbids call-graph traversal and names specific codegraph tools to
    // forbid (and one to allow for a targeted check). Like the persona it need not
    // name every tool, but each name it uses must be real -- a phantom (the long-gone
    // callers/callees/impact) would present a nonexistent tool as usable and yield
    // `unknown tool` for a reviewer who follows it (#573).
    let skill = std::fs::read_to_string(examples_dir().join("skills/pr-review/SKILL.md"))
        .expect("read pr-review SKILL.md");
    let named = codegraph_tool_stems(&skill);
    let expected: std::collections::BTreeSet<String> =
        CODEGRAPH_TOOLS.iter().map(|s| s.to_string()).collect();

    let phantom: Vec<_> = named.difference(&expected).collect();
    assert!(
        phantom.is_empty(),
        "pr-review SKILL.md names tools codegraph does not advertise: {phantom:?}"
    );
}

#[test]
fn codon_persona_names_only_real_codegraph_tools() {
    // The persona pushes "codegraph first" and names specific tools; any phantom
    // name there steers the model to an `unknown tool` (#573). It need not name
    // every tool, but every tool it names must be real.
    let toml =
        std::fs::read_to_string(examples_dir().join("phenos/codon.toml")).expect("read codon.toml");
    let named = codegraph_tool_stems(&toml);
    let expected: std::collections::BTreeSet<String> =
        CODEGRAPH_TOOLS.iter().map(|s| s.to_string()).collect();

    let phantom: Vec<_> = named.difference(&expected).collect();
    assert!(
        phantom.is_empty(),
        "codon.toml persona names tools codegraph does not advertise: {phantom:?}"
    );
}
