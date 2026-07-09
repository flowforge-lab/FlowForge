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
        egress: crate::Egress::Open,
    };
    let json = serde_json::to_string(&p).unwrap();
    assert_eq!(p, serde_json::from_str(&json).unwrap());
    assert!(!json.contains("persona"));
}

#[test]
fn phenotype_defaults_egress_to_open_when_absent() {
    // TOML/JSON that omits `egress` must deserialize to Open (backward compat).
    let p: Phenotype = serde_json::from_str(r#"{"name":"legacy"}"#).unwrap();
    assert_eq!(p.egress, crate::Egress::Open);
}

#[test]
fn phenotype_accepts_kebab_and_camel_egress() {
    // RFC 0013 TOML literal `local-only` (alias) and the camelCase wire form both parse.
    let a: Phenotype = serde_json::from_str(r#"{"name":"a","egress":"local-only"}"#).unwrap();
    let b: Phenotype = serde_json::from_str(r#"{"name":"b","egress":"localOnly"}"#).unwrap();
    assert_eq!(a.egress, crate::Egress::LocalOnly);
    assert_eq!(b.egress, crate::Egress::LocalOnly);
    // Serializes to the camelCase form for a consistent TS binding.
    let json = serde_json::to_string(&b).unwrap();
    assert!(json.contains("\"localOnly\""), "got: {json}");
}

#[test]
fn minimal_phenotype_defaults_skills() {
    let json = r#"{"name":"default"}"#;
    let p: Phenotype = serde_json::from_str(json).unwrap();
    assert!(p.skills.is_empty());
    assert!(p.model.is_none());
    assert!(p.persona.is_none());
}
