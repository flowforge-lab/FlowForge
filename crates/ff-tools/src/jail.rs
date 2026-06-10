//! Path containment. Tool file access is confined to an explicit per-session
//! workspace root — never the ambient process CWD, which is unpredictable for a
//! packaged desktop app. Resolution canonicalizes both sides so `..` traversal
//! and symlink escapes are rejected, not just literal prefixes.

use std::path::{Path, PathBuf};

/// Resolve `candidate` (relative to `root`, or absolute) and guarantee the result
/// stays inside `root`. The target file need not exist yet (for create-on-edit):
/// in that case the *parent* directory is the containment anchor.
pub fn resolve_in_root(root: &Path, candidate: &str) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("workspace root {} is unreadable: {e}", root.display()))?;

    let joined = {
        let c = Path::new(candidate);
        if c.is_absolute() {
            c.to_path_buf()
        } else {
            root.join(c)
        }
    };

    // Existing paths: canonicalize fully. Missing paths (new file): canonicalize
    // the parent and re-attach the file name, so we still resolve `..` segments.
    let resolved = match joined.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            let parent = joined
                .parent()
                .ok_or_else(|| format!("invalid path: {candidate}"))?;
            let file = joined
                .file_name()
                .ok_or_else(|| format!("invalid path: {candidate}"))?;
            let parent = parent
                .canonicalize()
                .map_err(|e| format!("parent of {} does not exist: {e}", joined.display()))?;
            parent.join(file)
        }
    };

    if resolved.starts_with(&root) {
        Ok(resolved)
    } else {
        Err(format!(
            "access denied: {candidate} resolves outside the workspace root {}",
            root.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn allows_paths_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let got = resolve_in_root(dir.path(), "a.txt").unwrap();
        assert!(got.ends_with("a.txt"));
    }

    #[test]
    fn allows_nonexistent_file_for_create() {
        let dir = tempfile::tempdir().unwrap();
        let got = resolve_in_root(dir.path(), "new.txt").unwrap();
        assert!(got.ends_with("new.txt"));
    }

    #[test]
    fn rejects_parent_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_in_root(dir.path(), "../escape.txt").unwrap_err();
        assert!(err.contains("access denied"), "{err}");
    }

    #[test]
    fn rejects_absolute_outside_root() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_in_root(dir.path(), "/etc/hosts").unwrap_err();
        assert!(err.contains("access denied"), "{err}");
    }
}
