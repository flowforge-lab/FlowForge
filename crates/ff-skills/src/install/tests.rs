use super::*;
use std::fs;
use tempfile::tempdir;

const MANIFEST: &str =
    "---\nname: rust-debug\ndescription: Debug Rust.\nversion: 0.1.0\ntools:\n  - bash\n---\n# Rust Debug\n\nDo the thing.\n";

fn write_source_dir(root: &Path, body: &str) -> PathBuf {
    let dir = root.join("src-skill");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), body).unwrap();
    dir
}

#[test]
fn installs_from_local_dir() {
    let tmp = tempdir().unwrap();
    let src = write_source_dir(tmp.path(), MANIFEST);
    let skills = tmp.path().join("skills");

    let staged = prepare_install(src.to_str().unwrap()).unwrap();
    assert_eq!(staged.manifest().name, "rust-debug");
    assert_eq!(staged.manifest().tools, vec!["bash"]);

    let installed = commit_install(staged, &skills).unwrap();
    assert_eq!(installed, skills.join("rust-debug"));
    assert!(installed.join("SKILL.md").is_file());

    let (reg, errs) = crate::SkillRegistry::load_dir(&skills);
    assert!(errs.is_empty());
    assert!(reg.get("rust-debug").is_some());
}

#[test]
fn installs_from_single_markdown_file() {
    let tmp = tempdir().unwrap();
    let file = tmp.path().join("SKILL.md");
    fs::write(&file, MANIFEST).unwrap();
    let skills = tmp.path().join("skills");

    let installed = install(file.to_str().unwrap(), &skills).unwrap();
    assert_eq!(installed, skills.join("rust-debug"));
    assert!(installed.join("SKILL.md").is_file());
}

#[test]
fn rejects_bundle_with_executable() {
    let tmp = tempdir().unwrap();
    let src = write_source_dir(tmp.path(), MANIFEST);
    fs::write(src.join("run.sh"), "#!/bin/sh\necho hi\n").unwrap();
    let skills = tmp.path().join("skills");

    let err = prepare_install(src.to_str().unwrap()).unwrap_err();
    assert!(matches!(err, InstallError::ExecutablePresent(_)));
    assert!(
        !skills.exists(),
        "nothing must be placed when validation fails"
    );
}

#[test]
fn rejects_nested_executable() {
    let tmp = tempdir().unwrap();
    let src = write_source_dir(tmp.path(), MANIFEST);
    fs::create_dir_all(src.join("scripts")).unwrap();
    fs::write(src.join("scripts").join("evil.py"), "print('x')\n").unwrap();

    let err = prepare_install(src.to_str().unwrap()).unwrap_err();
    match err {
        InstallError::ExecutablePresent(f) => assert!(f.contains("evil.py")),
        other => panic!("expected ExecutablePresent, got {other:?}"),
    }
}

#[test]
fn rejects_invalid_manifest() {
    let tmp = tempdir().unwrap();
    let src = write_source_dir(tmp.path(), "no frontmatter here\n");
    let err = prepare_install(src.to_str().unwrap()).unwrap_err();
    assert!(matches!(err, InstallError::Invalid(_)));
}

#[test]
fn rejects_missing_skill_md() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path().join("empty");
    fs::create_dir_all(&dir).unwrap();
    let err = prepare_install(dir.to_str().unwrap()).unwrap_err();
    assert!(matches!(err, InstallError::Invalid(SkillError::Io { .. })));
}

#[test]
fn rejects_already_installed() {
    let tmp = tempdir().unwrap();
    let src = write_source_dir(tmp.path(), MANIFEST);
    let skills = tmp.path().join("skills");
    install(src.to_str().unwrap(), &skills).unwrap();

    let staged = prepare_install(src.to_str().unwrap()).unwrap();
    let err = commit_install(staged, &skills).unwrap_err();
    assert!(matches!(err, InstallError::AlreadyInstalled(n) if n == "rust-debug"));
}

#[test]
fn unknown_source_is_unsupported() {
    let err = prepare_install("not-a-real-thing").unwrap_err();
    assert!(matches!(err, InstallError::UnsupportedSource(_)));
}

#[test]
fn missing_path_is_not_found() {
    let err = prepare_install("./no/such/path").unwrap_err();
    assert!(matches!(err, InstallError::NotFound(_)));
}

#[test]
fn uninstall_removes_then_errors_when_absent() {
    let tmp = tempdir().unwrap();
    let src = write_source_dir(tmp.path(), MANIFEST);
    let skills = tmp.path().join("skills");
    install(src.to_str().unwrap(), &skills).unwrap();

    let removed = uninstall("rust-debug", &skills).unwrap();
    assert!(!removed.exists());
    assert!(matches!(
        uninstall("rust-debug", &skills).unwrap_err(),
        InstallError::NotInstalled(_)
    ));
}

#[test]
fn rejects_traversal_manifest_name() {
    for bad in ["../escape", "/abs", "a/b", "..", "."] {
        let tmp = tempdir().unwrap();
        let manifest =
            format!("---\nname: {bad}\ndescription: d\nversion: 0.1.0\n---\n# X\nbody\n");
        let src = write_source_dir(tmp.path(), &manifest);
        let skills = tmp.path().join("skills");

        let staged = prepare_install(src.to_str().unwrap()).unwrap();
        let err = commit_install(staged, &skills).unwrap_err();
        assert!(
            matches!(err, InstallError::InvalidName(n) if n == bad),
            "name `{bad}` should be rejected"
        );
        // Nothing escaped the skills root (which itself was never created).
        assert!(!skills.join("escape").exists());
    }
}

#[test]
fn rejects_traversal_uninstall_name() {
    let tmp = tempdir().unwrap();
    let skills = tmp.path().join("skills");
    // A sibling dir that a traversal name would target — must survive.
    let victim = tmp.path().join("victim");
    fs::create_dir_all(&victim).unwrap();

    for bad in ["../victim", "/abs", "a/b", ".."] {
        let err = uninstall(bad, &skills).unwrap_err();
        assert!(
            matches!(err, InstallError::InvalidName(n) if n == bad),
            "uninstall name `{bad}` should be rejected"
        );
    }
    assert!(
        victim.is_dir(),
        "traversal uninstall must not delete siblings"
    );
}

#[test]
fn installs_from_local_git_repo() {
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("git not available; skipping git install test");
        return;
    }
    let tmp = tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap()
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "t@e.com"]);
    run(&["config", "user.name", "t"]);
    fs::write(repo.join("SKILL.md"), MANIFEST).unwrap();
    run(&["add", "."]);
    run(&["commit", "-qm", "init"]);

    let url = format!("file://{}", repo.display());
    let skills = tmp.path().join("skills");
    let installed = install(&url, &skills).unwrap();
    assert!(installed.join("SKILL.md").is_file());
    assert!(
        !installed.join(".git").exists(),
        ".git is stripped on install"
    );
}
