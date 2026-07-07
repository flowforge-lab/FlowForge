use super::*;
use std::sync::{Arc, RwLock};
use tempfile::tempdir;

const MANIFEST: &str = "---\nname: demo\ndescription: d\nversion: 0.1.0\n---\n# Demo\nbody\n";

fn shared() -> SharedRegistry {
    Arc::new(RwLock::new(ff_skills::SkillRegistry::new()))
}

#[tokio::test]
async fn install_tool_installs_and_refreshes_registry() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("SKILL.md"), MANIFEST).unwrap();
    let skills = tmp.path().join("skills");
    let reg = shared();

    let tool = InstallSkillTool::new(skills.clone(), reg.clone());
    let out = tool
        .run(
            serde_json::json!({ "source": src.to_str().unwrap() }),
            tmp.path(),
        )
        .await;

    assert!(out.success, "{}", out.content);
    assert!(skills.join("demo").join("SKILL.md").is_file());
    assert!(reg.read().unwrap().get("demo").is_some());
    assert_eq!(tool.safety(&serde_json::json!({})), Safety::Dangerous);
}

#[tokio::test]
async fn install_tool_reports_bad_bundle() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("SKILL.md"), "garbage\n").unwrap();
    let skills = tmp.path().join("skills");

    let tool = InstallSkillTool::new(skills.clone(), shared());
    let out = tool
        .run(
            serde_json::json!({ "source": src.to_str().unwrap() }),
            tmp.path(),
        )
        .await;

    assert!(!out.success);
    assert!(out.content.contains("install failed"));
    assert!(!skills.exists());
}

#[tokio::test]
async fn search_tool_lists_all_then_ranks() {
    let tmp = tempdir().unwrap();
    let skills = tmp.path().join("skills");
    for (dir, name, desc, ver) in [
        ("rusty", "rusty", "rust debugging", "0.1.0"),
        ("other", "other", "misc helper", "0.2.0"),
    ] {
        let d = skills.join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {desc}\nversion: {ver}\n---\nbody\n"),
        )
        .unwrap();
    }
    let reg = shared();
    state::reload_registry(&skills, &reg);

    let tool = SearchSkillsTool::new(reg.clone());
    assert_eq!(tool.safety(&serde_json::json!({})), Safety::ReadOnly);

    let out = tool.run(serde_json::json!({}), tmp.path()).await;
    assert!(out.success, "{}", out.content);
    assert!(out.content.contains("rusty"));
    assert!(out.content.contains("other"));

    let out = tool
        .run(serde_json::json!({ "query": "rust" }), tmp.path())
        .await;
    assert!(out.success);
    assert!(out.content.contains("rusty (v0.1.0): rust debugging"));
    assert!(!out.content.contains("other"));
}

#[tokio::test]
async fn search_tool_reports_no_matches() {
    let tool = SearchSkillsTool::new(shared());
    let out = tool
        .run(
            serde_json::json!({ "query": "nothing" }),
            std::path::Path::new("."),
        )
        .await;
    assert!(out.success);
    assert_eq!(out.content, "no matching skills");
}

#[tokio::test]
async fn uninstall_tool_removes_and_refreshes() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("SKILL.md"), MANIFEST).unwrap();
    let skills = tmp.path().join("skills");
    let reg = shared();
    ff_skills::install(src.to_str().unwrap(), &skills).unwrap();
    state::reload_registry(&skills, &reg);
    assert!(reg.read().unwrap().get("demo").is_some());

    let tool = UninstallSkillTool::new(skills.clone(), reg.clone());
    let out = tool
        .run(serde_json::json!({ "name": "demo" }), tmp.path())
        .await;

    assert!(out.success, "{}", out.content);
    assert!(reg.read().unwrap().get("demo").is_none());
}

#[tokio::test]
async fn skills_tool_lists_empty_registry() {
    let tool = SkillsTool::new(shared());
    assert_eq!(tool.safety(&serde_json::json!({})), Safety::ReadOnly);
    let out = tool
        .run(serde_json::json!({}), std::path::Path::new("."))
        .await;
    assert!(out.success, "{}", out.content);
    assert_eq!(out.content, "(no skills installed)");
}

#[tokio::test]
async fn skills_tool_lists_populated_registry() {
    let tmp = tempdir().unwrap();
    let skills = tmp.path().join("skills");
    for (dir, name, desc, ver) in [
        ("alpha", "alpha", "alpha skill", "1.0.0"),
        ("zulu", "zulu", "zulu helper", "2.3.1"),
    ] {
        let d = skills.join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {desc}\nversion: {ver}\n---\nbody\n"),
        )
        .unwrap();
    }
    let reg = shared();
    state::reload_registry(&skills, &reg);

    let tool = SkillsTool::new(reg.clone());
    let out = tool.run(serde_json::json!({}), tmp.path()).await;
    assert!(out.success, "{}", out.content);
    assert!(out.content.contains("alpha"));
    assert!(out.content.contains("v1.0.0"));
    assert!(out.content.contains("alpha skill"));
    assert!(out.content.contains("zulu"));
    assert!(out.content.contains("v2.3.1"));
    assert!(out.content.contains("zulu helper"));
}
