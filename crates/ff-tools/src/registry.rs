//! The tool abstraction and the registry the agent loop dispatches through.

use std::collections::HashMap;
use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

/// How much trust a given invocation needs. The agent loop auto-runs
/// [`Safety::ReadOnly`] and defers [`Safety::Write`] / [`Safety::Dangerous`] to an
/// approval policy supplied by the host (UI confirm in the desktop shell).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Safety {
    ReadOnly,
    Write,
    Dangerous,
}

/// The result of running a tool. `content` is fed back to the model verbatim as the
/// tool message; `success` lets the host render pass/fail without parsing `content`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub success: bool,
    pub content: String,
}

impl ToolOutcome {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            success: true,
            content: content.into(),
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            success: false,
            content: content.into(),
        }
    }
}

/// A callable the model can invoke. Implementors describe themselves as an
/// OpenAI-style function schema and execute against a jailed workspace `root`.
///
/// `run` must never panic or propagate transport errors to the caller — failures
/// are returned as [`ToolOutcome::error`] so the model can read and react to them.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema for the arguments object (the `parameters` field of the function).
    fn parameters(&self) -> Value;
    /// Classify a concrete invocation. Defaults to [`Safety::Write`] — implementors
    /// override when they can prove read-only or flag a destructive call.
    fn safety(&self, _args: &Value) -> Safety {
        Safety::Write
    }
    async fn run(&self, args: Value, root: &Path) -> ToolOutcome;
}

/// Name -> tool. Built with the M2 defaults (bash, view, edit) and queried by the
/// agent loop to (a) advertise schemas to the model and (b) dispatch calls.
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The built-in M2 toolset.
    pub fn with_defaults() -> Self {
        let mut r = Self::new();
        r.register(Box::new(crate::bash::BashTool));
        r.register(Box::new(crate::view::ViewTool));
        r.register(Box::new(crate::edit::EditTool));
        r.register(Box::new(crate::write::WriteTool));
        r.register(Box::new(crate::grep::GrepTool));
        r.register(Box::new(crate::glob::GlobTool));
        r.register(Box::new(crate::todo::TodoTool));
        r
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|b| b.as_ref())
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// All tools as OpenAI `tools` request entries.
    pub fn openai_tools(&self) -> Vec<Value> {
        self.tools
            .values()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters(),
                    }
                })
            })
            .collect()
    }

    /// Dispatch a call by name. Unknown tools and malformed arguments return an
    /// error outcome rather than failing the turn.
    pub async fn run(&self, name: &str, args: Value, root: &Path) -> ToolOutcome {
        match self.get(name) {
            Some(tool) => tool.run(args, root).await,
            None => ToolOutcome::error(format!("unknown tool: {name}")),
        }
    }

    /// Safety of a concrete call (defaults to [`Safety::Dangerous`] for unknown
    /// tools so an unrecognized name can never be auto-approved).
    pub fn safety(&self, name: &str, args: &Value) -> Safety {
        match self.get(name) {
            Some(tool) => tool.safety(args),
            None => Safety::Dangerous,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unknown_tool_is_error_not_panic() {
        let reg = ToolRegistry::with_defaults();
        let out = reg.run("nope", serde_json::json!({}), Path::new(".")).await;
        assert!(!out.success);
        assert!(out.content.contains("unknown tool"));
    }

    #[test]
    fn advertises_default_schemas() {
        let reg = ToolRegistry::with_defaults();
        let tools = reg.openai_tools();
        assert_eq!(tools.len(), 7);
        let names: Vec<_> = tools
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        for expected in ["bash", "view", "edit", "write", "grep", "glob", "todo"] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
    }

    #[test]
    fn search_and_plan_tools_are_read_only() {
        let reg = ToolRegistry::with_defaults();
        for name in ["grep", "glob", "todo"] {
            assert_eq!(
                reg.safety(name, &serde_json::json!({})),
                Safety::ReadOnly,
                "{name} should be read-only"
            );
        }
    }

    #[test]
    fn unknown_tool_safety_is_dangerous() {
        let reg = ToolRegistry::with_defaults();
        assert_eq!(
            reg.safety("nope", &serde_json::json!({})),
            Safety::Dangerous
        );
    }
}
