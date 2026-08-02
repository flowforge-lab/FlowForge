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

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every file that steers an agent toward codegraph, whatever tree it lives in.
///
/// This list is the guard's *scope*, and the scope is the part that failed. #1170
/// corrected `docs/examples/codon` and left `AGENTS.md:19` advertising
/// `callers`/`callees`/`impact`, because the phantom scan only ever walked
/// `examples_dir()` -- so the stale copy was not wrong-but-caught, it was
/// structurally invisible (#1173). `AGENTS.md` is the worse place to miss: it
/// rides the volatile prompt tail and is re-sent every turn, while the codon
/// persona sits in the cached stable prefix.
///
/// Add a file here whenever it starts naming codegraph tools; a doc outside this
/// list is a doc the guard cannot see.
fn codegraph_steering_docs() -> Vec<(&'static str, PathBuf)> {
    vec![
        (
            "docs/examples/codon/skills/codegraph/SKILL.md",
            examples_dir().join("skills/codegraph/SKILL.md"),
        ),
        (
            "docs/examples/codon/skills/pr-review/SKILL.md",
            examples_dir().join("skills/pr-review/SKILL.md"),
        ),
        (
            "docs/examples/codon/phenos/codon.toml",
            examples_dir().join("phenos/codon.toml"),
        ),
        ("AGENTS.md", repo_root().join("AGENTS.md")),
        ("CONTRIBUTING.md", repo_root().join("CONTRIBUTING.md")),
    ]
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
// Verified against `tools/list` on codegraph v1.5.0 (2026-07-31): the server
// advertises exactly one tool. Upstream unlists the narrower ones deliberately --
// "one strong tool steers agents better than a menu of narrower ones" -- and folds
// what they returned into `explore`'s inline output. Re-probe the server, do not
// trust its README or a prior version of this list, before editing this set.
const CODEGRAPH_TOOLS: [&str; 1] = ["explore"];

/// Stems that may appear in the docs *only* as names to reject. The docs have to be
/// able to say "a `codegraph_callers` is a name you invented" -- naming a phantom in
/// order to forbid it is the opposite of the #573 failure, so a blanket ban on the
/// string would forbid the very warning that prevents the bug. Each of these must
/// appear inside a sentence that marks it unavailable; `phantom_names_only_appear_as_warnings`
/// enforces that, so this list cannot be used to smuggle a phantom back in as usable.
const PHANTOM_TOOLS_NAMED_AS_WARNINGS: [&str; 8] = [
    "callers", "callees", "impact", "files", "status", "context", "trace", "node",
];

/// Wording that marks a nearby tool name as unavailable rather than callable.
const WARNING_MARKERS: [&str; 7] = [
    "invented",
    "does not advertise",
    "not a tool you have",
    "unlists",
    "phantom",
    "did not exist",
    "does not exist",
];

/// The real tool stems plus the phantoms the docs are allowed to *name in order to
/// reject*. Widening the set here is only safe because
/// `phantom_names_only_appear_as_warnings` independently proves every phantom
/// mention sits next to a `WARNING_MARKERS` phrase.
fn allowed_stems(
    expected: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeSet<String> {
    expected
        .iter()
        .cloned()
        .chain(
            PHANTOM_TOOLS_NAMED_AS_WARNINGS
                .iter()
                .map(|s| s.to_string()),
        )
        .collect()
}

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

    let allowed: std::collections::BTreeSet<String> = expected
        .iter()
        .cloned()
        .chain(
            PHANTOM_TOOLS_NAMED_AS_WARNINGS
                .iter()
                .map(|s| s.to_string()),
        )
        .collect();
    let phantom: Vec<_> = documented.difference(&allowed).collect();
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

    let allowed = allowed_stems(&expected);
    let phantom: Vec<_> = named.difference(&allowed).collect();
    assert!(
        phantom.is_empty(),
        "pr-review SKILL.md names tools codegraph does not advertise: {phantom:?}"
    );
}

/// Split prose into the smallest unit that can carry a warning on its own. Blank lines
/// are not enough: `codon.toml`'s persona is one TOML multi-line string whose four
/// codegraph facts are consecutive `- **...**` bullets with no blank line between
/// them, so a blank-line split lumps all four into a single "paragraph" and a marker
/// in a neighbouring bullet satisfies the check for a bullet that no longer warns at
/// all. A mutation stripping one bullet's warning survived exactly that way.
fn warning_scopes(text: &str) -> Vec<String> {
    let mut scopes = Vec::new();
    for para in text.split("\n\n") {
        let mut current = String::new();
        for line in para.lines() {
            // A new top-level bullet starts a new scope; indented continuations of the
            // same bullet stay with it.
            if line.trim_start().starts_with("- ")
                && !line.starts_with("  ")
                && !current.trim().is_empty()
            {
                scopes.push(std::mem::take(&mut current));
            }
            current.push_str(line);
            current.push('\n');
        }
        if !current.trim().is_empty() {
            scopes.push(current);
        }
    }
    scopes
}

#[test]
fn phantom_names_only_appear_as_warnings() {
    // `PHANTOM_TOOLS_NAMED_AS_WARNINGS` widens all three "documents only real tools"
    // assertions, so on its own it would be a hole: a doc could reintroduce
    // "call `codegraph_impact` before an edit" and stay green. Close it here --
    // every phantom mention must sit in a *paragraph* that marks it unavailable.
    //
    // Scoped to the paragraph, not a byte window: an earlier draft scanned +/-600
    // bytes and a mutation that stripped the warning still passed, because a marker
    // in the neighbouring bullet fell inside the window. A phantom is only safe when
    // the prose *around it* rejects it, so the unit has to be the paragraph.
    for (rel, path) in codegraph_steering_docs() {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        for stem in PHANTOM_TOOLS_NAMED_AS_WARNINGS {
            let needle = format!("codegraph_{stem}");
            for para in warning_scopes(&text) {
                if !para.contains(&needle) {
                    continue;
                }
                assert!(
                    WARNING_MARKERS.iter().any(|m| para.contains(m)),
                    "{rel} mentions phantom `{needle}` in a paragraph that does not mark \
                     it unavailable; the paragraph must contain one of {WARNING_MARKERS:?}.\n\
                     --- paragraph ---\n{para}"
                );
            }
        }

        // The docs list phantoms elliptically -- "`codegraph_callers` / `_impact` /
        // `_search`" -- and the `codegraph_` stem scanner is blind to those shortened
        // forms, so a mutation could leave `_impact` presented as usable while every
        // stem-based assertion stayed green. Check the shorthand on its own terms.
        for stem in PHANTOM_TOOLS_NAMED_AS_WARNINGS {
            let needle = format!("`_{stem}`");
            for para in warning_scopes(&text) {
                if !para.contains(&needle) {
                    continue;
                }
                assert!(
                    WARNING_MARKERS.iter().any(|m| para.contains(m)),
                    "{rel} lists phantom shorthand {needle} in a paragraph that does not \
                     mark it unavailable; the paragraph must contain one of \
                     {WARNING_MARKERS:?}.\n--- paragraph ---\n{para}"
                );
            }
        }

        // Third form, and the one that actually shipped: a bare word in a
        // slash-separated list keyed off a nearby "codegraph", as in #1173's
        // `codegraph (codegraph_explore / callers / callees / impact)`. Neither the
        // `codegraph_` scanner nor the `` `_stem` `` scanner matches "callers" there,
        // so the real regression sat in `AGENTS.md` fully green -- widening the file
        // list alone would not have caught it. Only inspect paragraphs that mention
        // codegraph, so ordinary English ("the callers of this function") stays legal.
        for stem in PHANTOM_TOOLS_NAMED_AS_WARNINGS {
            for para in warning_scopes(&text) {
                if !para.contains("codegraph") {
                    continue;
                }
                let bare_listed = para.contains(&format!("/ {stem}"))
                    || para.contains(&format!("{stem} /"))
                    || para.contains(&format!("/ {stem})"));
                if !bare_listed {
                    continue;
                }
                assert!(
                    WARNING_MARKERS.iter().any(|m| para.contains(m)),
                    "{rel} lists bare `{stem}` alongside codegraph as if it were \
                     callable; the paragraph must contain one of {WARNING_MARKERS:?}.\n\
                     --- paragraph ---\n{para}"
                );
            }
        }
    }
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

    let allowed = allowed_stems(&expected);
    let phantom: Vec<_> = named.difference(&allowed).collect();
    assert!(
        phantom.is_empty(),
        "codon.toml persona names tools codegraph does not advertise: {phantom:?}"
    );
}

#[test]
fn codegraph_docs_keep_the_flowforge_only_facts() {
    // The codegraph server ships its own usage guide in the MCP `initialize` response,
    // and these docs deliberately defer to it rather than paraphrase it -- a stale
    // paraphrase is what produced the phantom tool menu in the first place. What the
    // server cannot know is FlowForge's own wiring, so those facts are the docs' whole
    // remaining job and each one is load-bearing:
    //
    // - `tool_search`: codegraph is not in the default tool set, so skipping this step
    //   fails *silently* -- the agent just greps instead, every call succeeding.
    // - `maxFiles`: measured no-op above its default (it can only trim), so "raise it
    //   to see more" is advice that quietly does nothing.
    // - `affected`: identifies tests by `tests/` placement, so Rust's in-`src`
    //   `#[cfg(test)] mod tests` is invisible and it returns a constant wrong answer.
    //
    // Each fact was verified against codegraph v1.5.0 on this workspace; if one is
    // reworded, keep a probe for it rather than deleting the assertion.
    let skill = std::fs::read_to_string(examples_dir().join("skills/codegraph/SKILL.md"))
        .expect("read codegraph SKILL.md");
    let persona =
        std::fs::read_to_string(examples_dir().join("phenos/codon.toml")).expect("read codon.toml");

    for (label, text) in [("SKILL.md", &skill), ("codon.toml", &persona)] {
        // Assert the *instruction*, not the mere token. `tool_search` also appears in
        // SKILL.md's frontmatter description, so `contains("tool_search")` stayed true
        // when a mutation gutted the body -- a bare-token assertion is satisfied by
        // prose that no longer tells the agent to do anything.
        let body = text
            .split("## ")
            .filter(|s| s.contains("tool_search"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("tool_search \"codegraph\"") || body.contains("tool_search `codegraph`"),
            "{label} must spell out the actual call -- `tool_search \"codegraph\"` -- \
             in a body section, not just mention the tool by name: codegraph is absent \
             from the default tool set, so omitting the step degrades silently into a \
             grep loop instead of erroring"
        );
        assert!(
            text.contains("maxFiles"),
            "{label} must record that `maxFiles` cannot widen the result (byte budget \
             bounds it), or the agent will raise it and believe that did something"
        );
        assert!(
            text.contains("affected"),
            "{label} must warn that `codegraph affected` misidentifies Rust tests"
        );
    }

    // The deferral itself is the anti-drift mechanism, so it has to survive edits too.
    for (label, text) in [("SKILL.md", &skill), ("codon.toml", &persona)] {
        assert!(
            text.contains("initialize"),
            "{label} must point at the server's own `initialize` guidance as the \
             authority instead of restating it"
        );
    }
}
