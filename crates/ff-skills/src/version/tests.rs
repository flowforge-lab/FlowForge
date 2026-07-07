use super::*;

const BODY_V1: &str = "Original instructions.";

fn write_skill(skills_root: &Path, name: &str, version: &str, body: &str) {
    let dir = skills_root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let md =
        format!("---\nname: {name}\ndescription: A test skill.\nversion: {version}\n---\n{body}\n");
    std::fs::write(dir.join("SKILL.md"), md).unwrap();
}

fn read_live(skills_root: &Path, name: &str) -> ff_core::Skill {
    let p = live_md(skills_root, name);
    let c = std::fs::read_to_string(&p).unwrap();
    parse_skill(&c, p.parent().unwrap().to_path_buf(), p).unwrap()
}

#[test]
fn bump_patch_cases() {
    assert_eq!(bump_patch("0.1.0"), "0.1.1");
    assert_eq!(bump_patch("1.2.9"), "1.2.10");
    assert_eq!(bump_patch("2"), "3");
    assert_eq!(bump_patch("1.0.x"), "1.0.x.1");
}

#[test]
fn bump_retains_old_version_and_writes_new_body() {
    let skills = tempfile::tempdir().unwrap();
    let history = tempfile::tempdir().unwrap();
    write_skill(skills.path(), "alpha", "0.1.0", BODY_V1);

    let new_version = bump_skill(
        skills.path(),
        history.path(),
        "alpha",
        "Streamlined instructions.",
    )
    .unwrap();
    assert_eq!(new_version, "0.1.1");

    // Live skill carries the new body + bumped version.
    let live = read_live(skills.path(), "alpha");
    assert_eq!(live.manifest.version, "0.1.1");
    assert_eq!(live.body, "Streamlined instructions.");

    // The previous version is retained verbatim — never silently overwritten.
    let archived = version_dir(history.path(), "alpha", "0.1.0").join("SKILL.md");
    let archived_skill = parse_skill(
        &std::fs::read_to_string(&archived).unwrap(),
        archived.parent().unwrap().to_path_buf(),
        archived.clone(),
    )
    .unwrap();
    assert_eq!(archived_skill.manifest.version, "0.1.0");
    assert_eq!(archived_skill.body, BODY_V1);

    assert_eq!(
        list_skill_versions(history.path(), "alpha").unwrap(),
        vec!["0.1.0".to_string()]
    );
}

#[test]
fn rollback_restores_old_body_and_archives_current() {
    let skills = tempfile::tempdir().unwrap();
    let history = tempfile::tempdir().unwrap();
    write_skill(skills.path(), "alpha", "0.1.0", BODY_V1);
    bump_skill(skills.path(), history.path(), "alpha", "V2 body").unwrap();

    // Roll back to 0.1.0.
    rollback_skill(skills.path(), history.path(), "alpha", "0.1.0").unwrap();
    let live = read_live(skills.path(), "alpha");
    assert_eq!(live.manifest.version, "0.1.0");
    assert_eq!(live.body, BODY_V1);

    // The 0.1.1 we rolled away from is now retained too (rollback is reversible).
    assert!(list_skill_versions(history.path(), "alpha")
        .unwrap()
        .contains(&"0.1.1".to_string()));
}

#[test]
fn bump_unknown_skill_errors() {
    let skills = tempfile::tempdir().unwrap();
    let history = tempfile::tempdir().unwrap();
    let err = bump_skill(skills.path(), history.path(), "ghost", "x").unwrap_err();
    assert!(matches!(err, VersionError::NotInstalled(n) if n == "ghost"));
}

#[test]
fn rollback_missing_version_errors() {
    let skills = tempfile::tempdir().unwrap();
    let history = tempfile::tempdir().unwrap();
    write_skill(skills.path(), "alpha", "0.1.0", BODY_V1);
    let err = rollback_skill(skills.path(), history.path(), "alpha", "9.9.9").unwrap_err();
    assert!(matches!(err, VersionError::VersionNotFound { .. }));
}

#[test]
fn rejects_traversal_names() {
    let skills = tempfile::tempdir().unwrap();
    let history = tempfile::tempdir().unwrap();
    assert!(matches!(
        bump_skill(skills.path(), history.path(), "../escape", "x"),
        Err(VersionError::InvalidName(_))
    ));
    assert!(matches!(
        list_skill_versions(history.path(), "a/b"),
        Err(VersionError::InvalidName(_))
    ));
}

#[test]
fn version_key_orders_numerically() {
    // The lexicographic trap: under plain string sort "0.1.10" < "0.1.9".
    assert!(version_key("0.1.9") < version_key("0.1.10"));
    assert!(version_key("0.1.2") < version_key("0.1.9"));
    assert!(version_key("1.0.0") < version_key("1.2.0"));
    // Non-numeric segments fall back to lexical order.
    assert!(version_key("1.0.x") < version_key("1.1.0"));
}

#[test]
fn list_versions_sorts_numerically_past_ten() {
    let history = tempfile::tempdir().unwrap();
    let root = history.path().join("alpha");
    for v in ["0.1.2", "0.1.9", "0.1.10"] {
        std::fs::create_dir_all(root.join(v)).unwrap();
    }
    // Lexically "0.1.10" < "0.1.9", so the old sort()+reverse() returned
    // 0.1.9 first; numeric per-segment comparison puts 0.1.10 newest.
    assert_eq!(
        list_skill_versions(history.path(), "alpha").unwrap(),
        vec![
            "0.1.10".to_string(),
            "0.1.9".to_string(),
            "0.1.2".to_string()
        ]
    );
}

#[test]
fn list_versions_empty_when_no_history() {
    let history = tempfile::tempdir().unwrap();
    assert!(list_skill_versions(history.path(), "alpha")
        .unwrap()
        .is_empty());
}
