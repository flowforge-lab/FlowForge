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

/// Like [`resolve_in_root`] but for paths whose intermediate directories may not
/// exist yet (e.g. creating `src/main.rs` in a fresh workspace). Containment is
/// anchored on the *deepest existing ancestor*, which is canonicalized so a
/// symlinked ancestor cannot escape `root`; the trailing not-yet-created segments
/// must be plain names (no `.` / `..`).
pub fn resolve_for_create(root: &Path, candidate: &str) -> Result<PathBuf, String> {
    use std::path::Component;

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

    // Find the deepest existing ancestor and canonicalize it, then re-attach the
    // trailing components that do not exist yet.
    let mut existing = joined.as_path();
    let mut trailing: Vec<&std::ffi::OsStr> = Vec::new();
    let anchor = loop {
        match existing.canonicalize() {
            Ok(p) => break p,
            Err(_) => {
                // A missing component without a plain file name (a `..`/`.`
                // segment) is a traversal attempt, not a real path to create.
                let name = existing.file_name().ok_or_else(|| {
                    format!("access denied: {candidate} contains a non-literal path segment")
                })?;
                trailing.push(name);
                existing = existing
                    .parent()
                    .ok_or_else(|| format!("invalid path: {candidate}"))?;
            }
        }
    };

    let mut resolved = anchor;
    for name in trailing.into_iter().rev() {
        // The not-yet-created tail must be ordinary names; `.`/`..` here could
        // walk back out of the anchored, already-validated prefix.
        match Path::new(name).components().next() {
            Some(Component::Normal(_)) => resolved.push(name),
            _ => {
                return Err(format!(
                    "access denied: {candidate} contains a non-literal path segment"
                ))
            }
        }
    }

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

    #[test]
    fn create_allows_nested_missing_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let got = resolve_for_create(dir.path(), "a/b/c.txt").unwrap();
        assert!(got.ends_with("a/b/c.txt"));
        assert!(got.starts_with(dir.path().canonicalize().unwrap()));
    }

    #[test]
    fn create_rejects_parent_traversal_in_missing_tail() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_for_create(dir.path(), "a/../../escape.txt").unwrap_err();
        assert!(err.contains("access denied"), "{err}");
    }

    // This test exercises a unix-only primitive (symlinks); gating the whole
    // fn avoids unused-variable warnings on Windows, where the body would
    // otherwise allocate `outside`/`root` and never read them.
    #[cfg(unix)]
    #[test]
    fn create_rejects_symlinked_ancestor_escape() {
        let outside = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        // `root/link` -> outside; creating `root/link/x.txt` must be rejected.
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        let err = resolve_for_create(root.path(), "link/x.txt").unwrap_err();
        assert!(err.contains("access denied"), "{err}");
    }

    // Windows: std::fs::canonicalize() yields a `\\?\` verbatim/UNC prefix on
    // both sides, so starts_with() compares verbatim-to-verbatim. The
    // #[cfg(unix)] symlink test above cannot run here, so these give Windows
    // containment its only coverage: drive-absolute escape, `..` traversal,
    // and case-insensitive match.
    #[cfg(windows)]
    #[test]
    fn windows_rejects_drive_absolute_escape() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("secret.txt");
        fs::write(&outside_file, "x").unwrap();
        // A plain `C:\...\secret.txt` canonicalizes to `\\?\C:\...\secret.txt`
        // inside resolve_in_root; starts_with(&root) must then reject it.
        let err = resolve_in_root(root.path(), outside_file.to_str().unwrap())
            .expect_err("drive-absolute path outside root must be denied");
        assert!(err.contains("access denied"), "{err}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_rejects_backslash_parent_traversal() {
        let root = tempfile::tempdir().unwrap();
        // `..\escape.txt` (backslash separators) canonicalizes to root's parent
        // — outside the workspace — and must be rejected.
        let err = resolve_in_root(root.path(), r"..\escape.txt")
            .expect_err("parent traversal must be denied");
        assert!(err.contains("access denied"), "{err}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_case_insensitive_match_inside_root() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("MixedCase.txt"), "hi").unwrap();
        // Windows is case-insensitive; canonicalize() must normalize both sides
        // so the case-sensitive starts_with() still admits the file.
        let got = resolve_in_root(root.path(), r"MIXEDCASE.TXT")
            .expect("differently-cased inside-root file must be admitted");
        assert!(got.ends_with("MixedCase.txt"), "{got:?}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_create_rejects_drive_absolute_escape() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        // A not-yet-existing file under an outside dir: the missing-tail branch
        // anchors on the outside dir, then starts_with(&root) must reject.
        let candidate = outside.path().join("escaped.txt");
        let err = resolve_for_create(root.path(), candidate.to_str().unwrap())
            .expect_err("drive-absolute create outside root must be denied");
        assert!(err.contains("access denied"), "{err}");
    }
}
