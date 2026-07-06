//! Child-process environment helpers shared across the crates that spawn
//! external binaries (#755).
//!
//! A macOS app launched from Finder / Dock / launchd — or any process that did
//! not inherit a login shell — gets a minimal `PATH` that omits the user-level
//! bin dirs a terminal would have (`~/.local/bin`, `~/.cargo/bin`,
//! `/opt/homebrew/{bin,sbin}`, `/usr/local/bin`). A bare `Command::new("cargo")`
//! / `"gh"` / `"git"` then fails with "No such file or directory". `ff-mcp`
//! first hit this for MCP servers (#573); this module hoists that fix so every
//! spawner — the `diagnostics` / `github` tools and the MCP client — shares one
//! implementation instead of hardcoding paths.

use std::ffi::OsString;
use std::path::PathBuf;

/// Common user-level bin directories a login shell puts on `PATH` but a
/// GUI-launched process does not inherit. Unix-only; other platforms add nothing
/// (Windows resolves via its own search rules / `PATHEXT`).
#[cfg(unix)]
pub fn extra_path_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/opt/homebrew/sbin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

#[cfg(not(unix))]
pub fn extra_path_dirs() -> Vec<PathBuf> {
    Vec::new()
}

/// Append `extra` directories to an inherited `PATH`, preserving order and
/// dropping duplicates so the inherited entries keep priority and a dir already
/// present is not repeated. Falls back to the inherited value unchanged if
/// joining fails (e.g. a dir contains the platform path separator).
pub fn augment_path(inherited: Option<OsString>, extra: &[PathBuf]) -> OsString {
    use std::collections::HashSet;
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut parts: Vec<PathBuf> = Vec::new();
    if let Some(p) = &inherited {
        for dir in std::env::split_paths(p) {
            if seen.insert(dir.clone()) {
                parts.push(dir);
            }
        }
    }
    for dir in extra {
        if seen.insert(dir.clone()) {
            parts.push(dir.clone());
        }
    }
    std::env::join_paths(&parts).unwrap_or_else(|_| inherited.unwrap_or_default())
}

/// The process `PATH` augmented with [`extra_path_dirs`] — ready to hand to
/// `Command::env("PATH", …)` before spawning an external binary so a bare
/// command name resolves in a packaged / GUI-launched build. Inherited entries
/// keep priority; the extra dirs are appended (so a user override still wins).
pub fn augmented_path() -> OsString {
    augment_path(std::env::var_os("PATH"), &extra_path_dirs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn augment_path_appends_extra_dirs_in_order() {
        let extra = vec![PathBuf::from("/x/bin"), PathBuf::from("/y/bin")];
        let out = augment_path(Some(OsString::from("/usr/bin:/bin")), &extra);
        let dirs: Vec<PathBuf> = std::env::split_paths(&out).collect();
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
                PathBuf::from("/x/bin"),
                PathBuf::from("/y/bin"),
            ]
        );
    }

    #[test]
    fn augment_path_dedups_dirs_already_inherited() {
        let extra = vec![PathBuf::from("/usr/bin"), PathBuf::from("/x/bin")];
        let out = augment_path(Some(OsString::from("/usr/bin:/bin")), &extra);
        let dirs: Vec<PathBuf> = std::env::split_paths(&out).collect();
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
                PathBuf::from("/x/bin"),
            ]
        );
    }

    #[test]
    fn augment_path_handles_no_inherited_path() {
        let out = augment_path(None, &[PathBuf::from("/x/bin")]);
        let dirs: Vec<PathBuf> = std::env::split_paths(&out).collect();
        assert_eq!(dirs, vec![PathBuf::from("/x/bin")]);
    }

    #[cfg(unix)]
    #[test]
    fn extra_dirs_include_homebrew_and_cargo() {
        let dirs = extra_path_dirs();
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        // ~/.cargo/bin present when a home dir is resolvable.
        if let Some(home) = dirs::home_dir() {
            assert!(dirs.contains(&home.join(".cargo/bin")));
        }
    }
}
