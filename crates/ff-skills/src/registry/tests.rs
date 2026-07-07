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
#[cfg(unix)]
fn executable_bit_on_skill_md_is_not_flagged() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempdir().unwrap();
    write_skill(tmp.path(), "ok", "ok");
    let md = tmp.path().join("ok").join("SKILL.md");
    let mut perms = fs::metadata(&md).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&md, perms).unwrap();

    let (reg, errs) = SkillRegistry::load_dir(tmp.path());
    assert!(errs.is_empty());
    assert!(reg.get("ok").is_some());
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
