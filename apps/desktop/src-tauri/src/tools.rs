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

#[cfg(test)]
mod tests {
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
}
