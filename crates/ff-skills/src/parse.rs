//! `SKILL.md` parsing: split the leading `---` YAML frontmatter from the body.

use std::path::PathBuf;

use ff_core::{Skill, SkillManifest};

use crate::error::SkillError;

/// Parse a `SKILL.md` document. `dir` is the skill's directory (stored on the
/// returned [`Skill`]); `file` identifies the source for error messages.
///
/// Format: a frontmatter block delimited by `---` lines, followed by the body.
/// The first line must be exactly `---`; the block ends at the next `---` line.
/// Everything after is the (trimmed) instruction body.
pub fn parse_skill(content: &str, dir: PathBuf, file: PathBuf) -> Result<Skill, SkillError> {
    let (frontmatter, body) = split_frontmatter(content)
        .ok_or_else(|| SkillError::MissingFrontmatter { path: file.clone() })?;

    let manifest: SkillManifest = serde_norway::from_str(frontmatter)
        .map_err(|source| SkillError::Frontmatter { path: file, source })?;

    Ok(Skill {
        manifest,
        body: body.trim().to_string(),
        path: dir,
    })
}

/// Returns `(frontmatter, body)` when `content` opens with a `---` fenced block.
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix("---")?;
    // The opening fence must be its own line.
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))?;

    // Find the closing `---` that sits on its own line.
    let mut idx = 0;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            let frontmatter = &rest[..idx];
            let body = &rest[idx + line.len()..];
            return Some((frontmatter, body));
        }
        idx += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
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
}
