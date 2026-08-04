//! Agent-callable wrappers around the skill installer (RFC 0001 §5). These bind the
//! generic `ff_tools::Tool` trait to the `ff_skills` engine plus the live skill
//! registry, so a model-driven install goes through the same M2 approval gate as
//! any other `Dangerous` tool and the in-memory registry refreshes on success.

use std::path::PathBuf;

use async_trait::async_trait;
use ff_skills::SharedRegistry;
use ff_tools::{Safety, Tool, ToolOutcome};
use serde_json::Value;

use crate::state;

/// Installs a skill from a local path, git URL, or raw-Markdown URL. Classified
/// `Dangerous` so the agent loop always routes it through the approval gate.
pub struct InstallSkillTool {
    skills_root: PathBuf,
    registry: SharedRegistry,
}

impl InstallSkillTool {
    pub fn new(skills_root: PathBuf, registry: SharedRegistry) -> Self {
        Self {
            skills_root,
            registry,
        }
    }
}

#[async_trait]
impl Tool for InstallSkillTool {
    fn name(&self) -> &str {
        "install_skill"
    }

    fn description(&self) -> &str {
        "Install a skill from a local path, a git URL, or a raw-Markdown URL. The \
         bundle is validated (no executables, valid SKILL.md) before installation."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "A local path, git URL (git@/ssh/.git/file://), or http(s) URL to a SKILL.md."
                }
            },
            "required": ["source"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::Dangerous
    }

    async fn run(&self, args: Value, _root: &std::path::Path) -> ToolOutcome {
        let Some(source) = args.get("source").and_then(Value::as_str) else {
            return ToolOutcome::error("install_skill requires a string `source`");
        };
        let source = source.to_string();
        let skills_root = self.skills_root.clone();

        let result =
            tokio::task::spawn_blocking(move || ff_skills::install(&source, &skills_root)).await;

        match result {
            Ok(Ok(path)) => {
                state::reload_registry(&self.skills_root, &self.registry);
                ToolOutcome::ok(format!("installed skill at {}", path.display()))
            }
            Ok(Err(e)) => ToolOutcome::error(format!("install failed: {e}")),
            Err(e) => ToolOutcome::error(format!("install task failed: {e}")),
        }
    }
}

/// Removes an installed skill by name. Local and reversible (reinstall), so it runs
/// without the approval gate (RFC 0001 §9 gates install + evolution).
pub struct UninstallSkillTool {
    skills_root: PathBuf,
    registry: SharedRegistry,
}

impl UninstallSkillTool {
    pub fn new(skills_root: PathBuf, registry: SharedRegistry) -> Self {
        Self {
            skills_root,
            registry,
        }
    }
}

#[async_trait]
impl Tool for UninstallSkillTool {
    fn reaches_network(&self) -> bool {
        false
    }

    fn name(&self) -> &str {
        "uninstall_skill"
    }

    fn description(&self) -> &str {
        "Uninstall an installed skill by its manifest name."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "The skill's manifest name." }
            },
            "required": ["name"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::Write
    }

    async fn run(&self, args: Value, _root: &std::path::Path) -> ToolOutcome {
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return ToolOutcome::error("uninstall_skill requires a string `name`");
        };
        match ff_skills::uninstall(name, &self.skills_root) {
            Ok(path) => {
                state::reload_registry(&self.skills_root, &self.registry);
                ToolOutcome::ok(format!("uninstalled skill at {}", path.display()))
            }
            Err(e) => ToolOutcome::error(format!("uninstall failed: {e}")),
        }
    }
}

/// Lists every installed skill with name, version and description. No arguments —
/// simply reads the live registry so the model can see what capabilities exist on this
/// machine without restarting.
pub struct SkillsTool {
    registry: SharedRegistry,
}

impl SkillsTool {
    pub fn new(registry: SharedRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for SkillsTool {
    fn reaches_network(&self) -> bool {
        false
    }

    fn name(&self) -> &str {
        "skills"
    }

    fn description(&self) -> &str {
        "List every installed skill (name, version, description). Read-only — no parameters."
    }

    fn parameters(&self) -> Value {
        // Explicit object schema even though there are no arguments (#1191): a bare
        // `{}` is what strict providers reject as `type: null`, 400ing the entire
        // request. `scoped_parameters` now coerces this as a safety net, but the
        // declaration is fixed at the source too -- the net exists for the *next*
        // no-arg tool, not as licence for this one to stay wrong.
        serde_json::json!({ "type": "object", "properties": {} })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::ReadOnly
    }

    async fn run(&self, _args: Value, _root: &std::path::Path) -> ToolOutcome {
        let reg = self.registry.read().unwrap();
        if reg.is_empty() {
            return ToolOutcome::ok("(no skills installed)".to_string());
        }
        let listing: Vec<_> = reg
            .list()
            .map(|s| {
                format!(
                    "- {} v{} — {}",
                    s.manifest.name, s.manifest.version, s.manifest.description
                )
            })
            .collect();
        ToolOutcome::ok(listing.join("\n"))
    }
}

/// Ranks installed skills for a query so the agent can discover capabilities to
/// activate (RFC 0001 §6). Read-only — never gated. Shares `ff_skills::search_skills`
/// with the palette-facing `search_skills` Tauri command, so ranking is identical.
pub struct SearchSkillsTool {
    registry: SharedRegistry,
}

impl SearchSkillsTool {
    pub fn new(registry: SharedRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for SearchSkillsTool {
    fn reaches_network(&self) -> bool {
        false
    }

    fn name(&self) -> &str {
        "search_skills"
    }

    fn description(&self) -> &str {
        "Search installed skills by keyword. Ranks by exact keyword, then name, then          description. An empty query lists every installed skill. Returns each match          as `name (version): description`."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search terms. Empty lists all installed skills."
                }
            }
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::ReadOnly
    }

    async fn run(&self, args: Value, _root: &std::path::Path) -> ToolOutcome {
        let query = args.get("query").and_then(Value::as_str).unwrap_or("");
        let reg = self.registry.read().unwrap();
        let hits = ff_skills::search_skills(&reg, query);
        if hits.is_empty() {
            return ToolOutcome::ok("no matching skills".to_string());
        }
        let listing = hits
            .iter()
            .map(|h| {
                format!(
                    "- {} (v{}): {}",
                    h.manifest.name, h.manifest.version, h.manifest.description
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        ToolOutcome::ok(listing)
    }
}

#[cfg(test)]
mod tests;
