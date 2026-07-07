use super::*;
use std::path::PathBuf;

#[test]
fn manifest_round_trips() {
    let m = SkillManifest {
        name: "rust-debugging".into(),
        description: "Systematic Rust debugging.".into(),
        version: "0.1.0".into(),
        author: Some("tonytan4ever".into()),
        tools: vec!["bash".into(), "view".into(), "edit".into()],
        mcp: vec![],
        keywords: vec!["rust".into(), "debug".into()],
    };
    let json = serde_json::to_string(&m).unwrap();
    assert_eq!(m, serde_json::from_str(&json).unwrap());
}

#[test]
fn minimal_manifest_defaults_collections() {
    let json = r#"{"name":"x","description":"d","version":"0.1.0"}"#;
    let m: SkillManifest = serde_json::from_str(json).unwrap();
    assert!(m.author.is_none());
    assert!(m.tools.is_empty());
    assert!(m.mcp.is_empty());
    assert!(m.keywords.is_empty());
}

#[test]
fn manifest_uses_camel_case_keys() {
    // author omitted when None (skip_serializing_if).
    let m = SkillManifest {
        name: "x".into(),
        description: "d".into(),
        version: "0.1.0".into(),
        author: None,
        tools: vec![],
        mcp: vec![],
        keywords: vec![],
    };
    let json = serde_json::to_string(&m).unwrap();
    assert!(!json.contains("author"));
}

#[test]
fn skill_round_trips() {
    let s = Skill {
        manifest: SkillManifest {
            name: "x".into(),
            description: "d".into(),
            version: "0.1.0".into(),
            author: None,
            tools: vec!["bash".into()],
            mcp: vec![],
            keywords: vec![],
        },
        body: "# X\nDo the thing.".into(),
        path: PathBuf::from("/home/u/.flowforge/skills/x"),
    };
    let json = serde_json::to_string(&s).unwrap();
    assert_eq!(s, serde_json::from_str(&json).unwrap());
}

#[test]
fn phenotype_round_trips() {
    let p = Phenotype {
        name: "rust".into(),
        skills: vec!["rust-debugging".into()],
        model: Some("Qwen3-4B-Instruct-2507".into()),
        persona: None,
        max_iterations: None,
        provider: None,
        mcp_servers: Vec::new(),
    };
    let json = serde_json::to_string(&p).unwrap();
    assert_eq!(p, serde_json::from_str(&json).unwrap());
    assert!(!json.contains("persona"));
}

#[test]
fn minimal_phenotype_defaults_skills() {
    let json = r#"{"name":"default"}"#;
    let p: Phenotype = serde_json::from_str(json).unwrap();
    assert!(p.skills.is_empty());
    assert!(p.model.is_none());
    assert!(p.persona.is_none());
}
