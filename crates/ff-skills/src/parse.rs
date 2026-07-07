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
mod tests;
