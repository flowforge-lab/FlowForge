//! Skill and phenotype domain types (RFC 0001).
//!
//! A [`Skill`] is Markdown instructions plus declared tool/MCP references, loaded
//! from `~/.flowforge/skills/<name>/SKILL.md` at runtime. A [`Phenotype`] selects the
//! working set of skills (plus an optional model and persona) for a session.
//!
//! [`SkillManifest`] and [`Phenotype`] cross the IPC boundary (skill list, search,
//! phenotype UI) and are exported to TypeScript. [`Skill`] carries the full body and
//! an on-disk path — backend-only, deliberately not part of the FE contract.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::McpServerConfig;

/// The `SKILL.md` frontmatter. Collection fields default to empty so a minimal
/// manifest (just `name` + `description` + `version`) deserializes cleanly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct SkillManifest {
    pub name: String,
    pub description: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub author: Option<String>,
    /// Tool names that must resolve in the `ToolRegistry` when the skill loads.
    #[serde(default)]
    pub tools: Vec<String>,
    /// MCP server ids — declared in M3, enforced in M4.
    #[serde(default)]
    pub mcp: Vec<String>,
    /// Search/discovery keywords.
    #[serde(default)]
    pub keywords: Vec<String>,
}

/// A skill as presented to the frontend: discovery metadata plus whether it is
/// currently active. One DTO backs both `list_skills` (unranked, `score` = 0) and
/// `search_skills` (ranked). Distinct from [`SkillManifest`] (frontmatter only) —
/// it folds in runtime `active` state and a search `score`, and omits the
/// FE-irrelevant `tools`/`mcp`/`author` fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub version: String,
    pub keywords: Vec<String>,
    /// Whether the skill is in the global active set (its body is injected into
    /// the system prompt for the next turn).
    pub active: bool,
    /// Lexical relevance from `search_skills`; `0` for the unranked `list_skills`.
    pub score: u32,
}

/// A loaded skill: its manifest, the instruction body, and where it lives on disk.
/// Backend-only — not exported to TypeScript (see module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    pub manifest: SkillManifest,
    /// Instruction body (everything after the `SKILL.md` frontmatter).
    pub body: String,
    /// Directory the skill was loaded from.
    pub path: PathBuf,
}

/// A named, switchable working set: which skills are active, an optional model
/// override, and an optional persona preamble prepended to the system prompt.
///
/// Named for the genotype/phenotype pairing: installed skills are the latent
/// "genes"; a `Phenotype` is the *expressed* set active in a given context (and
/// what Skill Evolution improves). User-facing surfaces use the short form `pheno`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct Phenotype {
    pub name: String,
    /// Active skill names (resolved against the `SkillRegistry`).
    #[serde(default)]
    pub skills: Vec<String>,
    /// Overrides the default model when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// Binds this phenotype to a specific provider connection (RFC 0005 Phase C).
    /// `None` inherits the globally active connection. Pairs with `model` to form
    /// the phenotype tier of three-tier model resolution (RFC 0005 §11.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider: Option<crate::provider::ConnectionId>,
    /// Extra system-prompt preamble for this phenotype.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub persona: Option<String>,
    /// Overrides the agent loop's tool-call iteration cap when set (#244 R3).
    /// A coding phenotype that runs long edit/build/test/fix cycles raises this
    /// above the default; unset falls back to [`ff_agent`'s default cap].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_iterations: Option<usize>,
    /// Phenotype-tier MCP server definitions (RFC 0018 section 3.1). These overlay
    /// the global `mcp.json` by id (whole-record, RFC section 11.5) when this
    /// phenotype is active; a `scope: workspace` entry (e.g. codegraph) is keyed
    /// per workspace root. Empty contributes nothing, identical to today.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
