use super::*;

#[test]
fn find_in_dirs_locates_a_file_on_path() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("pwsh.exe");
    std::fs::write(&bin, b"").unwrap();
    let path = std::env::join_paths(["/no/such/dir".as_ref(), dir.path().as_os_str()])
        .unwrap()
        .into_string()
        .unwrap();
    assert_eq!(find_in_dirs("pwsh.exe", &path).as_deref(), bin.to_str());
}

#[test]
fn find_in_dirs_returns_none_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().into_owned();
    assert!(find_in_dirs("pwsh.exe", &path).is_none());
}

#[test]
fn find_in_dirs_handles_empty_path() {
    assert!(find_in_dirs("pwsh.exe", "").is_none());
}

#[test]
fn shell_invocation_uses_a_command_flag() {
    let (program, flag) = shell_invocation();
    assert!(!program.is_empty());
    assert!(["-c", "/C", "-Command"].contains(&flag));
    #[cfg(not(windows))]
    assert_eq!(flag, "-c");
}
