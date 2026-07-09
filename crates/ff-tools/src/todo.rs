//! A per-turn planning checklist. The model passes the *complete* current list on
//! every call (full-replace, like a write-once snapshot); the latest call is the
//! authoritative state. It is stateless on the backend: the list is persisted as
//! the tool call's arguments on the assistant message (so it survives reload), and
//! the frontend renders it from the `tool:call` args. `Safety::ReadOnly` — it is
//! pure bookkeeping and never touches the filesystem.

use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

use crate::registry::{Safety, Tool, ToolOutcome};

const STATUSES: [&str; 3] = ["pending", "in_progress", "completed"];

fn marker(status: &str) -> &'static str {
    match status {
        "completed" => "[x]",
        "in_progress" => "[~]",
        _ => "[ ]",
    }
}

pub struct TodoTool;

#[async_trait]
impl Tool for TodoTool {
    fn reaches_network(&self) -> bool {
        false
    }
    fn name(&self) -> &str {
        "todo"
    }

    fn description(&self) -> &str {
        "Record or update the plan for the current task as a checklist. Pass the COMPLETE \
         list of items every call — it fully replaces the previous list. Each item has \
         `content` (the step) and `status` (one of: pending, in_progress, completed). Use \
         this to plan multi-step work and to mark progress as you go."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "description": "The complete checklist, in order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "The task step." },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                                "description": "Current status of this step."
                            }
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["items"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::ReadOnly
    }

    fn max_safety(&self) -> Safety {
        Safety::ReadOnly
    }

    async fn run(&self, args: Value, _root: &Path) -> ToolOutcome {
        let Some(items) = args.get("items").and_then(Value::as_array) else {
            return ToolOutcome::error("missing required argument: items (an array)");
        };
        if items.is_empty() {
            return ToolOutcome::ok("(empty checklist)");
        }

        let mut lines = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let Some(content) = item.get("content").and_then(Value::as_str) else {
                return ToolOutcome::error(format!("item {i}: missing `content`"));
            };
            let Some(status) = item.get("status").and_then(Value::as_str) else {
                return ToolOutcome::error(format!("item {i}: missing `status`"));
            };
            if !STATUSES.contains(&status) {
                return ToolOutcome::error(format!(
                    "item {i}: invalid status `{status}` (expected one of {})",
                    STATUSES.join(", ")
                ));
            }
            lines.push(format!("{} {content}", marker(status)));
        }
        ToolOutcome::ok(lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(items: Value) -> ToolOutcome {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(TodoTool.run(serde_json::json!({ "items": items }), Path::new(".")))
    }

    #[test]
    fn renders_all_statuses() {
        let out = run(serde_json::json!([
            { "content": "design", "status": "completed" },
            { "content": "build", "status": "in_progress" },
            { "content": "ship", "status": "pending" },
        ]));
        assert!(out.success);
        assert_eq!(out.content, "[x] design\n[~] build\n[ ] ship");
    }

    #[test]
    fn rejects_invalid_status() {
        let out = run(serde_json::json!([{ "content": "x", "status": "done" }]));
        assert!(!out.success);
        assert!(
            out.content.contains("invalid status `done`"),
            "{}",
            out.content
        );
    }

    #[test]
    fn missing_content_is_error() {
        let out = run(serde_json::json!([{ "status": "pending" }]));
        assert!(!out.success);
        assert!(out.content.contains("missing `content`"));
    }

    #[test]
    fn empty_list_is_ok() {
        let out = run(serde_json::json!([]));
        assert!(out.success);
        assert_eq!(out.content, "(empty checklist)");
    }

    #[tokio::test]
    async fn missing_items_arg_is_error() {
        let out = TodoTool.run(serde_json::json!({}), Path::new(".")).await;
        assert!(!out.success);
        assert!(out.content.contains("missing required argument: items"));
    }
}
