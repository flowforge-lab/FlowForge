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
        if is_executable(&path, &name) {
            return Some(name);
        }
    }
    None
}

fn is_executable(path: &Path, name: &str) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if matches!(ext, "sh" | "py" | "rb" | "js" | "bash" | "exe" | "bin") {
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
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_skill(root: &Path, dir: &str, name: &str) {
        let d = root.join(dir);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: d\nversion: 0.1.0\n---\nbody\n"),
        )
        .unwrap();
    }

    #[test]
    fn loads_multiple_skills() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), "a", "alpha");
        write_skill(tmp.path(), "b", "beta");
        let (reg, errs) = SkillRegistry::load_dir(tmp.path());
        assert!(errs.is_empty());
        assert_eq!(reg.len(), 2);
        assert!(reg.get("alpha").is_some());
        let mut names = reg.names();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn missing_dir_is_empty_not_error() {
        let (reg, errs) = SkillRegistry::load_dir(Path::new("/no/such/skills/dir"));
        assert!(reg.is_empty());
        assert!(errs.is_empty());
    }

    #[test]
    fn dir_without_skill_md_collects_io_error() {
        let tmp = tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("empty")).unwrap();
        let (reg, errs) = SkillRegistry::load_dir(tmp.path());
        assert!(reg.is_empty());
        assert_eq!(errs.len(), 1);
        assert!(matches!(errs[0], SkillError::Io { .. }));
    }

    #[test]
    fn skill_with_executable_is_skipped() {
        let tmp = tempdir().unwrap();
        write_skill(tmp.path(), "danger", "danger");
        fs::write(tmp.path().join("danger").join("run.sh"), "#!/bin/sh\n").unwrap();
        let (reg, errs) = SkillRegistry::load_dir(tmp.path());
        assert!(reg.get("danger").is_none());
        assert_eq!(errs.len(), 1);
        assert!(matches!(errs[0], SkillError::ExecutablePresent { .. }));
    }

    #[test]
    fn duplicate_name_first_wins_and_errors() {
        let tmp = tempdir().unwrap();
        // dirs sort: "a" before "z", both declare name "dup" -> a wins.
        write_skill(tmp.path(), "a", "dup");
        write_skill(tmp.path(), "z", "dup");
        let (reg, errs) = SkillRegistry::load_dir(tmp.path());
        assert_eq!(reg.len(), 1);
        assert_eq!(errs.len(), 1);
        assert!(matches!(errs[0], SkillError::DuplicateName { .. }));
        // the winner is the one from dir "a"
        assert_eq!(reg.get("dup").unwrap().path, tmp.path().join("a"));
    }
}
