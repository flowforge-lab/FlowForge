use super::*;

#[test]
fn augment_path_appends_extra_dirs_in_order() {
    let extra = vec![PathBuf::from("/x/bin"), PathBuf::from("/y/bin")];
    let out = ff_core::augment_path(Some(OsString::from("/usr/bin:/bin")), &extra);
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
#[tokio::test]
async fn augmented_path_resolves_bare_command_under_env_clear() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("ff-fake-tool");
    std::fs::write(&bin, "#!/bin/sh\necho ok\n").unwrap();
    let mut perms = std::fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&bin, perms).unwrap();

    let path = ff_core::augment_path(
        Some(OsString::from("/usr/bin:/bin")),
        &[tmp.path().to_path_buf()],
    );
    let mut cmd = tokio::process::Command::new("ff-fake-tool");
    cmd.env_clear();
    cmd.env("PATH", &path);
    let out = cmd.output().await.unwrap();
    assert!(
        out.status.success(),
        "bare command should resolve via the augmented PATH"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
}

fn join(dirs: &[&std::path::Path]) -> String {
    std::env::join_paths(dirs.iter().map(|d| d.as_os_str()))
        .unwrap()
        .into_string()
        .unwrap()
}

#[test]
fn resolve_via_pathext_finds_a_cmd_shim() {
    let dir = tempfile::tempdir().unwrap();
    let shim = dir.path().join("npx.cmd");
    std::fs::write(&shim, b"").unwrap();
    let path = join(&["/no/such/dir".as_ref(), dir.path()]);
    assert_eq!(resolve_via_pathext("npx", &path, ".exe;.cmd"), Some(shim),);
}

#[test]
fn resolve_via_pathext_respects_pathext_then_path_order() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("npx.exe");
    std::fs::write(&exe, b"").unwrap();
    std::fs::write(dir.path().join("npx.cmd"), b"").unwrap();
    let path = join(&[dir.path()]);
    assert_eq!(resolve_via_pathext("npx", &path, ".exe;.cmd"), Some(exe),);
}

#[test]
fn resolve_via_pathext_none_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    let path = join(&[dir.path()]);
    assert!(resolve_via_pathext("npx", &path, ".EXE;.CMD").is_none());
}

#[test]
fn resolve_via_pathext_passes_through_qualified_commands() {
    let path = "/nope";
    assert!(resolve_via_pathext("/usr/bin/node", path, ".EXE;.CMD").is_none());
    assert!(resolve_via_pathext("./local/tool", path, ".EXE;.CMD").is_none());
    assert!(resolve_via_pathext("dir\\tool", path, ".EXE;.CMD").is_none());
    assert!(resolve_via_pathext("server.js", path, ".EXE;.CMD").is_none());
}

#[test]
fn resolve_via_pathext_handles_empty_path_and_pathext() {
    assert!(resolve_via_pathext("npx", "", ".EXE;.CMD").is_none());
    assert!(resolve_via_pathext("npx", "/some/dir", "").is_none());
}

#[test]
fn root_uris_maps_paths_to_file_uris() {
    let roots = root_uris(&[Path::new("/work/A"), Path::new("/work/B")]);
    assert_eq!(
        roots.iter().map(|r| r.uri.clone()).collect::<Vec<_>>(),
        vec!["file:///work/A".to_string(), "file:///work/B".to_string()],
    );
}

#[test]
fn root_uris_empty_for_global() {
    assert!(root_uris(&[]).is_empty());
}

#[tokio::test]
async fn handler_advertises_roots_and_lists_them() {
    let handler = FfClientHandler {
        tools_changed: Arc::new(AtomicBool::new(false)),
        roots: Arc::new(root_uris(&[Path::new("/work/A")])),
    };
    assert!(handler.get_info().capabilities.roots.is_some());
    assert_eq!(
        handler
            .roots
            .iter()
            .map(|r| r.uri.clone())
            .collect::<Vec<_>>(),
        vec!["file:///work/A".to_string()],
    );
}
