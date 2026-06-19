//! Headless assembly. Builds the same provider + tool registry + skill set the
//! desktop app wires up, but for a terminal. This is the single place runtime
//! dependencies are constructed, so later milestones (M4 MCP tools, M5 SQLite
//! sessions) wire in here rather than in each command.

use std::path::PathBuf;

use ff_core::{ProviderConfig, ProviderKind};
use ff_llm::{OllamaProvider, OpenAiProvider, Provider};
use ff_skills::SkillRegistry;

/// The provider + default model, honoring the same `~/.config/flowforge/provider.json`
/// the desktop app persists, so a provider chosen in the GUI is respected here.
/// Falls back to the default (local candle-vllm) when absent or unreadable.
pub fn load_provider() -> (Box<dyn Provider>, String) {
    let config = load_provider_config();
    let model = config.model.clone();
    (build_provider(&config), model)
}

fn load_provider_config() -> ProviderConfig {
    provider_config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn provider_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("flowforge").join("provider.json"))
}

fn build_provider(config: &ProviderConfig) -> Box<dyn Provider> {
    let base_url = config.resolved_base_url().to_string();
    match config.kind {
        ProviderKind::CandleVllm => Box::new(OpenAiProvider::new(base_url, None)),
        ProviderKind::Ollama => Box::new(OllamaProvider::new(base_url)),
    }
}

/// `~/.flowforge/skills` — the installed-skills directory shared with the desktop
/// app. Loaded read-only here; install/uninstall stay desktop-side for now.
pub fn skills_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".flowforge")
        .join("skills")
}

/// `~/.flowforge/phenotypes` — the phenotype definitions shared with the desktop
/// app (RFC 0001 §7). Loaded read-only here; editing stays desktop-side.
pub fn phenotypes_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".flowforge")
        .join("phenotypes")
}

/// Resolve a phenotype by name: the built-in `default`, otherwise a definition
/// from `~/.flowforge/phenotypes/<name>.toml`. Returns `None` for an unknown
/// name. Mirrors the desktop's `resolve_phenotype` so a headless turn sees the
/// same definition the GUI would apply.
pub fn resolve_phenotype(name: &str) -> Option<ff_core::Phenotype> {
    resolve_phenotype_in(name, &phenotypes_root())
}

fn resolve_phenotype_in(name: &str, root: &std::path::Path) -> Option<ff_core::Phenotype> {
    use ff_skills::{default_phenotype, load_phenotypes, DEFAULT_PHENOTYPE};

    if name == DEFAULT_PHENOTYPE {
        return Some(default_phenotype());
    }
    let (mut map, errors) = load_phenotypes(root);
    for e in &errors {
        eprintln!("warning: phenotype load: {e}");
    }
    map.remove(name)
}

/// Load the installed skill set for system-prompt injection. A parse error is
/// reported but never fatal — one bad skill must not block a turn.
pub fn load_skills() -> SkillRegistry {
    let (registry, errors) = SkillRegistry::load_dir(&skills_root());
    for e in &errors {
        eprintln!("warning: skill load: {e}");
    }
    registry
}

/// The directory the agent's file/shell tools are jailed to. For a CLI this is the
/// current working directory — you run `flowforge` where you want it to act.
pub fn workspace_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::resolve_phenotype_in;
    use ff_skills::DEFAULT_PHENOTYPE;
    use std::fs;

    #[test]
    fn default_phenotype_resolves_without_any_files() {
        // The built-in "default" short-circuits the filesystem: it resolves even
        // against an empty (or nonexistent) phenotypes root, like the desktop does.
        let tmp = tempfile::tempdir().unwrap();
        let p = resolve_phenotype_in(DEFAULT_PHENOTYPE, tmp.path()).unwrap();
        assert_eq!(p.name, "default");
        assert!(p.skills.is_empty());
        assert!(p.model.is_none());
        assert!(p.persona.is_none());
    }

    #[test]
    fn resolves_named_phenotype_from_toml_by_stem() {
        // A `--pheno rust` turn reads `<root>/rust.toml`; the name comes from the
        // file stem, not a field inside the TOML (mirrors the desktop).
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("rust.toml"),
            "skills = [\"cargo-check\", \"clippy\"]\n\
             model = \"qwen3-coder\"\n\
             persona = \"You are a Rust expert.\"\n",
        )
        .unwrap();

        let p = resolve_phenotype_in("rust", tmp.path()).unwrap();
        assert_eq!(p.name, "rust");
        assert_eq!(p.skills, vec!["cargo-check", "clippy"]);
        assert_eq!(p.model.as_deref(), Some("qwen3-coder"));
        assert_eq!(p.persona.as_deref(), Some("You are a Rust expert."));
    }

    #[test]
    fn unknown_name_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve_phenotype_in("nope", tmp.path()).is_none());
    }
}
