//! Skill versioning + rollback for the manual optimize flow (M3.5, RFC 0001 §8).
//!
//! "A skill is never silently overwritten." [`bump_skill`] archives the current
//! `SKILL.md` before writing a new body, and [`rollback_skill`] archives the live
//! version before restoring an older one — so every transition is reversible.
//!
//! Layout: the live, registry-scanned skill stays at `<skills_root>/<name>/SKILL.md`.
//! Retained versions live in a **separate** tree at
//! `<history_root>/<name>/<version>/SKILL.md`. History is deliberately *outside* the
//! skills root: [`crate::SkillRegistry::load_dir`] treats every top-level dir under
//! the skills root as a skill keyed by its manifest name, so version copies kept as
//! siblings would collide on name (`DuplicateName`). Keeping history elsewhere leaves
//! the registry, watcher, and installer untouched.

use std::path::{Component, Path, PathBuf};

use crate::error::SkillError;
use crate::parse::parse_skill;

/// Why a versioning operation failed.
#[derive(Debug, thiserror::Error)]
pub enum VersionError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid skill name `{0}` (must be a single path segment, no `/` or `..`)")]
    InvalidName(String),
    #[error("skill `{0}` is not installed")]
    NotInstalled(String),
    #[error("skill `{skill}` has no retained version `{version}`")]
    VersionNotFound { skill: String, version: String },
    #[error(transparent)]
    Parse(#[from] SkillError),
}

fn io(context: impl Into<String>, source: std::io::Error) -> VersionError {
    VersionError::Io {
        context: context.into(),
        source,
    }
}

/// Reject a name that is not a single, normal path segment (so it can be safely
/// joined onto a root): `..`, an absolute path, or a name with separators escapes.
fn validated_segment(name: &str) -> Result<&str, VersionError> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(seg)), None) if seg.to_str() == Some(name) => Ok(name),
        _ => Err(VersionError::InvalidName(name.to_string())),
    }
}

/// Bump the trailing numeric component of a dotted version (`0.1.0` -> `0.1.1`). A
/// version whose last `.`-segment isn't a number falls back to appending `.1`, so a
/// bump always produces a distinct, non-empty string.
pub fn bump_patch(version: &str) -> String {
    match version.rsplit_once('.') {
        Some((head, last)) => match last.parse::<u64>() {
            Ok(n) => format!("{head}.{}", n + 1),
            Err(_) => format!("{version}.1"),
        },
        None => match version.parse::<u64>() {
            Ok(n) => (n + 1).to_string(),
            Err(_) => format!("{version}.1"),
        },
    }
}

/// Render a `SKILL.md` from a manifest + body: re-serialized YAML frontmatter inside
/// `---` fences, then the trimmed body and a trailing newline.
fn render_skill_md(manifest: &ff_core::SkillManifest, body: &str) -> Result<String, VersionError> {
    let yaml = serde_norway::to_string(manifest).map_err(|e| {
        io(
            "serializing manifest",
            std::io::Error::new(std::io::ErrorKind::InvalidData, e),
        )
    })?;
    // serde_norway emits no document markers, but strip a stray leading fence just
    // in case so we never double up the `---`.
    let yaml = yaml.strip_prefix("---\n").unwrap_or(&yaml);
    let yaml = if yaml.ends_with('\n') {
        yaml.to_string()
    } else {
        format!("{yaml}\n")
    };
    Ok(format!("---\n{yaml}---\n{}\n", body.trim()))
}

fn live_md(skills_root: &Path, name: &str) -> PathBuf {
    skills_root.join(name).join("SKILL.md")
}

fn version_dir(history_root: &Path, name: &str, version: &str) -> PathBuf {
    history_root.join(name).join(version)
}

/// Copy the live `SKILL.md` into the history tree under its current version. No-op
/// when the live file is missing. Returns the archived version (if any).
fn archive_live(
    skills_root: &Path,
    history_root: &Path,
    name: &str,
) -> Result<Option<String>, VersionError> {
    let src = live_md(skills_root, name);
    let content = match std::fs::read_to_string(&src) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(format!("reading {}", src.display()), e)),
    };
    let skill = parse_skill(&content, src.parent().unwrap().to_path_buf(), src.clone())?;
    let version = skill.manifest.version.clone();
    let dir = version_dir(history_root, name, &version);
    std::fs::create_dir_all(&dir).map_err(|e| io(format!("creating {}", dir.display()), e))?;
    std::fs::write(dir.join("SKILL.md"), &content)
        .map_err(|e| io(format!("archiving {name}@{version}"), e))?;
    Ok(Some(version))
}

/// Replace a skill's body with `new_body`, bumping its version and retaining the
/// previous version in the history tree. Returns the new version. Errors if the
/// skill is not installed. The old `SKILL.md` is always archived first — it is never
/// silently overwritten (RFC 0001 §8).
pub fn bump_skill(
    skills_root: &Path,
    history_root: &Path,
    name: &str,
    new_body: &str,
) -> Result<String, VersionError> {
    let name = validated_segment(name)?;
    let live = live_md(skills_root, name);
    let content = match std::fs::read_to_string(&live) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(VersionError::NotInstalled(name.to_string()))
        }
        Err(e) => return Err(io(format!("reading {}", live.display()), e)),
    };
    let skill = parse_skill(&content, live.parent().unwrap().to_path_buf(), live.clone())?;

    // Retain the current version before any write.
    archive_live(skills_root, history_root, name)?;

    let mut manifest = skill.manifest;
    let new_version = bump_patch(&manifest.version);
    manifest.version = new_version.clone();
    let rendered = render_skill_md(&manifest, new_body)?;
    std::fs::write(&live, rendered).map_err(|e| io(format!("writing {}", live.display()), e))?;
    Ok(new_version)
}

/// Restore a retained version as the live skill, archiving the current live version
/// first (so a rollback is itself reversible). Errors if the skill is not installed
/// or has no such retained version.
pub fn rollback_skill(
    skills_root: &Path,
    history_root: &Path,
    name: &str,
    version: &str,
) -> Result<(), VersionError> {
    let name = validated_segment(name)?;
    let version = validated_segment(version)?;
    let archived = version_dir(history_root, name, version).join("SKILL.md");
    let restored = match std::fs::read_to_string(&archived) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(VersionError::VersionNotFound {
                skill: name.to_string(),
                version: version.to_string(),
            })
        }
        Err(e) => return Err(io(format!("reading {}", archived.display()), e)),
    };
    let live = live_md(skills_root, name);
    if !live.exists() {
        return Err(VersionError::NotInstalled(name.to_string()));
    }
    // Retain the version we're rolling away from.
    archive_live(skills_root, history_root, name)?;
    std::fs::write(&live, restored).map_err(|e| io(format!("writing {}", live.display()), e))?;
    Ok(())
}

/// Retained version names for a skill, descending (newest-looking first by string
/// order is not meaningful for semver, so this sorts lexicographically and reverses).
/// Missing history is an empty list, not an error.
pub fn list_skill_versions(history_root: &Path, name: &str) -> Result<Vec<String>, VersionError> {
    let name = validated_segment(name)?;
    let dir = history_root.join(name);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(format!("reading {}", dir.display()), e)),
    };
    let mut versions: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    versions.sort();
    versions.reverse();
    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY_V1: &str = "Original instructions.";

    fn write_skill(skills_root: &Path, name: &str, version: &str, body: &str) {
        let dir = skills_root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let md = format!(
            "---\nname: {name}\ndescription: A test skill.\nversion: {version}\n---\n{body}\n"
        );
        std::fs::write(dir.join("SKILL.md"), md).unwrap();
    }

    fn read_live(skills_root: &Path, name: &str) -> ff_core::Skill {
        let p = live_md(skills_root, name);
        let c = std::fs::read_to_string(&p).unwrap();
        parse_skill(&c, p.parent().unwrap().to_path_buf(), p).unwrap()
    }

    #[test]
    fn bump_patch_cases() {
        assert_eq!(bump_patch("0.1.0"), "0.1.1");
        assert_eq!(bump_patch("1.2.9"), "1.2.10");
        assert_eq!(bump_patch("2"), "3");
        assert_eq!(bump_patch("1.0.x"), "1.0.x.1");
    }

    #[test]
    fn bump_retains_old_version_and_writes_new_body() {
        let skills = tempfile::tempdir().unwrap();
        let history = tempfile::tempdir().unwrap();
        write_skill(skills.path(), "alpha", "0.1.0", BODY_V1);

        let new_version = bump_skill(
            skills.path(),
            history.path(),
            "alpha",
            "Streamlined instructions.",
        )
        .unwrap();
        assert_eq!(new_version, "0.1.1");

        // Live skill carries the new body + bumped version.
        let live = read_live(skills.path(), "alpha");
        assert_eq!(live.manifest.version, "0.1.1");
        assert_eq!(live.body, "Streamlined instructions.");

        // The previous version is retained verbatim — never silently overwritten.
        let archived = version_dir(history.path(), "alpha", "0.1.0").join("SKILL.md");
        let archived_skill = parse_skill(
            &std::fs::read_to_string(&archived).unwrap(),
            archived.parent().unwrap().to_path_buf(),
            archived.clone(),
        )
        .unwrap();
        assert_eq!(archived_skill.manifest.version, "0.1.0");
        assert_eq!(archived_skill.body, BODY_V1);

        assert_eq!(
            list_skill_versions(history.path(), "alpha").unwrap(),
            vec!["0.1.0".to_string()]
        );
    }

    #[test]
    fn rollback_restores_old_body_and_archives_current() {
        let skills = tempfile::tempdir().unwrap();
        let history = tempfile::tempdir().unwrap();
        write_skill(skills.path(), "alpha", "0.1.0", BODY_V1);
        bump_skill(skills.path(), history.path(), "alpha", "V2 body").unwrap();

        // Roll back to 0.1.0.
        rollback_skill(skills.path(), history.path(), "alpha", "0.1.0").unwrap();
        let live = read_live(skills.path(), "alpha");
        assert_eq!(live.manifest.version, "0.1.0");
        assert_eq!(live.body, BODY_V1);

        // The 0.1.1 we rolled away from is now retained too (rollback is reversible).
        assert!(list_skill_versions(history.path(), "alpha")
            .unwrap()
            .contains(&"0.1.1".to_string()));
    }

    #[test]
    fn bump_unknown_skill_errors() {
        let skills = tempfile::tempdir().unwrap();
        let history = tempfile::tempdir().unwrap();
        let err = bump_skill(skills.path(), history.path(), "ghost", "x").unwrap_err();
        assert!(matches!(err, VersionError::NotInstalled(n) if n == "ghost"));
    }

    #[test]
    fn rollback_missing_version_errors() {
        let skills = tempfile::tempdir().unwrap();
        let history = tempfile::tempdir().unwrap();
        write_skill(skills.path(), "alpha", "0.1.0", BODY_V1);
        let err = rollback_skill(skills.path(), history.path(), "alpha", "9.9.9").unwrap_err();
        assert!(matches!(err, VersionError::VersionNotFound { .. }));
    }

    #[test]
    fn rejects_traversal_names() {
        let skills = tempfile::tempdir().unwrap();
        let history = tempfile::tempdir().unwrap();
        assert!(matches!(
            bump_skill(skills.path(), history.path(), "../escape", "x"),
            Err(VersionError::InvalidName(_))
        ));
        assert!(matches!(
            list_skill_versions(history.path(), "a/b"),
            Err(VersionError::InvalidName(_))
        ));
    }

    #[test]
    fn list_versions_empty_when_no_history() {
        let history = tempfile::tempdir().unwrap();
        assert!(list_skill_versions(history.path(), "alpha")
            .unwrap()
            .is_empty());
    }
}
