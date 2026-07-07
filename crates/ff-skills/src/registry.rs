//! Loading a directory of skills into a queryable registry.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::path::Path;

use ff_core::Skill;

use crate::error::SkillError;
use crate::parse::parse_skill;

/// Name -> loaded skill. Built by [`SkillRegistry::load_dir`] over
/// `~/.flowforge/skills/` and queried by the agent for description injection and
/// activation.
#[derive(Debug, Default, Clone)]
pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load every `<root>/<name>/SKILL.md`. Resilient: a skill that fails to parse,
    /// is missing its `SKILL.md`, carries an executable, or collides on name is
    /// skipped and its error collected — one bad skill never blanks the registry.
    pub fn load_dir(root: &Path) -> (Self, Vec<SkillError>) {
        let mut reg = Self::new();
        let mut errors = Vec::new();

        let entries = match std::fs::read_dir(root) {
            Ok(e) => e,
            // A missing skills dir is a normal first-run state, not an error.
            Err(_) => return (reg, errors),
        };

        let mut dirs: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort(); // deterministic load order so "first wins" is stable

        for dir in dirs {
            match load_one(&dir) {
                Ok(skill) => match reg.skills.entry(skill.manifest.name.clone()) {
                    Entry::Occupied(e) => errors.push(SkillError::DuplicateName {
                        name: e.key().clone(),
                        path: dir,
                    }),
                    Entry::Vacant(e) => {
                        e.insert(skill);
                    }
                },
                Err(e) => errors.push(e),
            }
        }
        (reg, errors)
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.skills.keys().map(String::as_str).collect()
    }

    pub fn list(&self) -> impl Iterator<Item = &Skill> {
        self.skills.values()
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Test-only: insert a pre-built skill without touching the filesystem, so
    /// ranking/query tests don't need a tempdir of `SKILL.md` files.
    #[cfg(test)]
    pub(crate) fn insert_for_test(&mut self, skill: Skill) {
        self.skills.insert(skill.manifest.name.clone(), skill);
    }
}

fn load_one(dir: &Path) -> Result<Skill, SkillError> {
    if let Some(file) = first_executable(dir) {
        return Err(SkillError::ExecutablePresent {
            path: dir.to_path_buf(),
            file,
        });
    }
    let md_path = dir.join("SKILL.md");
    let content = std::fs::read_to_string(&md_path).map_err(|source| SkillError::Io {
        path: md_path.clone(),
        source,
    })?;
    parse_skill(&content, dir.to_path_buf(), md_path)
}

/// First executable file in `dir` (by extension or, on Unix, the execute bit).
/// M3 skills are instructions only; the installer (M3.2) hard-rejects on this,
/// and the loader skips defensively.
pub fn first_executable(dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        // SKILL.md is the manifest, not a payload — an exec bit on it (some
        // checkouts/fileshares) must not flag the whole skill.
        if name == "SKILL.md" {
            continue;
        }
        if is_executable(&path, &name) {
            return Some(name);
        }
    }
    None
}

/// Recursively scan a fetched bundle for executables before install, skipping the
/// .git metadata dir and the SKILL.md manifest. Returns the offending file's path
/// relative to dir. The installer (M3.2) hard-rejects on a match.
pub fn first_executable_recursive(dir: &Path) -> Option<String> {
    fn walk(dir: &Path, base: &Path) -> Option<String> {
        for entry in std::fs::read_dir(dir).ok()?.filter_map(Result::ok) {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if name == ".git" {
                    continue;
                }
                if let Some(found) = walk(&path, base) {
                    return Some(found);
                }
            } else if path.is_file() && name != "SKILL.md" && is_executable(&path, &name) {
                return Some(
                    path.strip_prefix(base)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string(),
                );
            }
        }
        None
    }
    walk(dir, dir)
}

fn is_executable(path: &Path, name: &str) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if matches!(
            ext,
            "sh" | "bash"
                | "py"
                | "rb"
                | "pl"
                | "php"
                | "js"
                | "exe"
                | "bin"
                | "bat"
                | "cmd"
                | "com"
                | "command"
                | "ps1"
                | "desktop"
        ) {
            return true;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.permissions().mode() & 0o111 != 0 {
                return true;
            }
        }
    }
    let _ = name;
    false
}

#[cfg(test)]
mod tests;
