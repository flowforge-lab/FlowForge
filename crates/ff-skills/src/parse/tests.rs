use super::*;

fn dir() -> PathBuf {
    PathBuf::from("/skills/x")
}
fn file() -> PathBuf {
    PathBuf::from("/skills/x/SKILL.md")
}

const VALID: &str = "---\nname: rust-debugging\ndescription: Debug Rust.\nversion: 0.1.0\ntools:\n  - bash\n  - view\nkeywords: [rust, debug]\n---\n# Rust Debugging\n\nDo the thing.\n";

#[test]
fn parses_valid() {
    let s = parse_skill(VALID, dir(), file()).unwrap();
    assert_eq!(s.manifest.name, "rust-debugging");
    assert_eq!(s.manifest.tools, vec!["bash", "view"]);
    assert_eq!(s.manifest.keywords, vec!["rust", "debug"]);
    assert_eq!(s.body, "# Rust Debugging\n\nDo the thing.");
    assert_eq!(s.path, dir());
}

#[test]
fn body_may_contain_triple_dash() {
    let md = "---\nname: x\ndescription: d\nversion: 0.1.0\n---\nbefore\n---\nafter\n";
    let s = parse_skill(md, dir(), file()).unwrap();
    assert_eq!(s.body, "before\n---\nafter");
}

#[test]
fn missing_frontmatter_errors() {
    let err = parse_skill("# no frontmatter\n", dir(), file()).unwrap_err();
    assert!(matches!(err, SkillError::MissingFrontmatter { .. }));
}

#[test]
fn unterminated_frontmatter_errors() {
    let err = parse_skill("---\nname: x\n", dir(), file()).unwrap_err();
    assert!(matches!(err, SkillError::MissingFrontmatter { .. }));
}

#[test]
fn missing_required_field_errors() {
    let md = "---\nname: x\nversion: 0.1.0\n---\nbody\n";
    let err = parse_skill(md, dir(), file()).unwrap_err();
    assert!(matches!(err, SkillError::Frontmatter { .. }));
}

#[test]
fn defaults_collections_when_absent() {
    let md = "---\nname: x\ndescription: d\nversion: 0.1.0\n---\nbody\n";
    let s = parse_skill(md, dir(), file()).unwrap();
    assert!(s.manifest.tools.is_empty());
    assert!(s.manifest.keywords.is_empty());
    assert!(s.manifest.author.is_none());
}
