use super::*;
use std::ffi::OsString;
use std::path::PathBuf;

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
