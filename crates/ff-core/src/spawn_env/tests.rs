use super::*;
#[cfg(unix)]
use std::ffi::OsString;
use std::path::PathBuf;

// Unix-only: the inputs use `:`-separated absolute Unix paths and the
// `assert_eq!` re-splits the joined result with `std::env::split_paths`,
// which is platform-dependent (`:` on unix, `;` on Windows). The function
// itself is cross-platform; the test asserts the unix shape. Mirrored
// exactly in `crates/ff-mcp/src/client/tests.rs` (kept in sync).
#[cfg(unix)]
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

#[cfg(unix)]
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
