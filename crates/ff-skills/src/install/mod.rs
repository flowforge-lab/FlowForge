//! Skill installation (RFC 0001 §5, §9). A *core capability*, not a skill: it is
//! deterministic and cannot depend on an LLM following instructions.
//!
//! The flow is deliberately split so the host can insert the M2 approval gate in
//! the middle: [`prepare_install`] fetches the source into a temp dir and fully
//! *validates* it (rejecting executables and bad manifests) **before** anything
//! touches `~/.flowforge/skills/`; [`commit_install`] then moves the validated tree
//! into place only after the user approves. [`uninstall`] removes an installed
//! skill directory.

mod fetch;

use std::path::{Component, Path, PathBuf};

use ff_core::SkillManifest;
use tempfile::TempDir;

use crate::error::SkillError;
use crate::parse::parse_skill;
use crate::registry::first_executable_recursive;

/// Why an install or uninstall failed.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("source not found: {0}")]
    NotFound(String),
    #[error("unsupported source (expected a path, git URL, or http(s) URL): {0}")]
    UnsupportedSource(String),
    #[error("git is required to install from a git URL but was not found on PATH")]
    GitUnavailable,
    #[error("failed to fetch `{url}`: {detail}")]
    Fetch { url: String, detail: String },
    #[error("bundle rejected: contains an executable file ({0}) — skills are instructions only")]
    ExecutablePresent(String),
    #[error("invalid skill: {0}")]
    Invalid(#[from] SkillError),
    #[error("a skill named `{0}` is already installed (uninstall it first)")]
    AlreadyInstalled(String),
    #[error("invalid skill name `{0}`: must be a single path segment (no `/`, `\\`, `..`, or absolute path)")]
    InvalidName(String),
    #[error("no skill named `{0}` is installed")]
    NotInstalled(String),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

/// A fetched, validated bundle awaiting the approval gate. Owns its temp dir, which
/// is cleaned up on drop if the install is never committed.
#[derive(Debug)]
pub struct StagedInstall {
    manifest: SkillManifest,
    body: String,
    skill_dir: PathBuf,
    _temp: TempDir,
}

impl StagedInstall {
    /// The parsed manifest — the declared name, version, tools, and permissions the
    /// approval UI presents to the user before anything is trusted.
    pub fn manifest(&self) -> &SkillManifest {
        &self.manifest
    }

    /// The instruction body, for an optional preview.
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// Fetch `source` into a temp dir and validate it: no executables, a parseable
/// `SKILL.md`. Returns the staged bundle for the caller to gate on approval, then
/// pass to [`commit_install`]. A bad bundle errors here, before anything is placed.
pub fn prepare_install(source: &str) -> Result<StagedInstall, InstallError> {
    let (temp, skill_dir) = fetch::materialize(source)?;

    if let Some(file) = first_executable_recursive(&skill_dir) {
        return Err(InstallError::ExecutablePresent(file));
    }

    let md_path = skill_dir.join("SKILL.md");
    let content = std::fs::read_to_string(&md_path).map_err(|source| {
        InstallError::Invalid(SkillError::Io {
            path: md_path.clone(),
            source,
        })
    })?;
    let skill = parse_skill(&content, skill_dir.clone(), md_path)?;

    Ok(StagedInstall {
        manifest: skill.manifest,
        body: skill.body,
        skill_dir,
        _temp: temp,
    })
}

/// Reject any name that isn't a single normal path segment. `manifest.name` (remote
/// frontmatter) and the uninstall `name` (model/UI-supplied) are both untrusted and
/// get joined onto `skills_root`; `..` or an absolute path would otherwise escape it.
fn validated_segment(name: &str) -> Result<&str, InstallError> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(seg)), None) if seg.to_str() == Some(name) => Ok(name),
        _ => Err(InstallError::InvalidName(name.to_string())),
    }
}

/// Move a validated bundle into `<skills_root>/<name>/`. Rejects a name that is
/// already installed rather than clobbering it. Returns the installed path.
pub fn commit_install(staged: StagedInstall, skills_root: &Path) -> Result<PathBuf, InstallError> {
    let name = validated_segment(&staged.manifest.name)?;
    std::fs::create_dir_all(skills_root).map_err(|e| io("creating skills dir", e))?;
    let target = skills_root.join(name);
    if target.exists() {
        return Err(InstallError::AlreadyInstalled(staged.manifest.name.clone()));
    }

    // Same-filesystem rename is atomic; fall back to a recursive copy across devices.
    if std::fs::rename(&staged.skill_dir, &target).is_err() {
        copy_into(&staged.skill_dir, &target)?;
    }
    Ok(target)
}

/// Convenience for the agent-tool path: prepare + commit in one shot. The approval
/// gate around it is the agent loop's (the tool is classified `Dangerous`).
pub fn install(source: &str, skills_root: &Path) -> Result<PathBuf, InstallError> {
    let staged = prepare_install(source)?;
    commit_install(staged, skills_root)
}

/// Remove an installed skill directory. Errors if no such skill is installed.
pub fn uninstall(name: &str, skills_root: &Path) -> Result<PathBuf, InstallError> {
    let name = validated_segment(name)?;
    let target = skills_root.join(name);
    if !target.is_dir() {
        return Err(InstallError::NotInstalled(name.to_string()));
    }
    std::fs::remove_dir_all(&target).map_err(|e| io("removing skill dir", e))?;
    Ok(target)
}

fn copy_into(src: &Path, dest: &Path) -> Result<(), InstallError> {
    std::fs::create_dir_all(dest).map_err(|e| io("creating target dir", e))?;
    for entry in std::fs::read_dir(src).map_err(|e| io("reading staged dir", e))? {
        let entry = entry.map_err(|e| io("reading staged entry", e))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_into(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| io("copying file", e))?;
        }
    }
    Ok(())
}

fn io(context: &str, source: std::io::Error) -> InstallError {
    InstallError::Io {
        context: context.to_string(),
        source,
    }
}

#[cfg(test)]
mod tests;
