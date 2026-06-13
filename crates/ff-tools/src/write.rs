//! Create or overwrite a file within the jailed workspace.

use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

use crate::jail::resolve_for_create;
use crate::registry::{Safety, Tool, ToolOutcome};

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Create or overwrite a file with the given content, creating parent \
         directories as needed. Use this to create new files; use `edit` to \
         modify an existing file in place."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to the workspace root." },
                "content": { "type": "string", "description": "Full file contents to write." }
            },
            "required": ["path", "content"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::Write
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        let (Some(path), Some(content)) = (
            args.get("path").and_then(Value::as_str),
            args.get("content").and_then(Value::as_str),
        ) else {
            return ToolOutcome::error("missing required argument: path, content");
        };

        let resolved = match resolve_for_create(root, path) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::error(e),
        };

        if let Some(parent) = resolved.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolOutcome::error(format!("cannot create parent of {path}: {e}"));
            }
        }

        match tokio::fs::write(&resolved, content).await {
            Ok(()) => ToolOutcome::ok(format!("wrote {path} ({} bytes)", content.len())),
            Err(e) => ToolOutcome::error(format!("cannot write {path}: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = WriteTool
            .run(
                serde_json::json!({"path": "hello.txt", "content": "hi\n"}),
                dir.path(),
            )
            .await;
        assert!(out.success, "{}", out.content);
        assert_eq!(
            fs::read_to_string(dir.path().join("hello.txt")).unwrap(),
            "hi\n"
        );
    }

    #[tokio::test]
    async fn overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        fs::write(&f, "old").unwrap();
        let out = WriteTool
            .run(
                serde_json::json!({"path": "f.txt", "content": "new"}),
                dir.path(),
            )
            .await;
        assert!(out.success, "{}", out.content);
        assert_eq!(fs::read_to_string(&f).unwrap(), "new");
    }

    #[tokio::test]
    async fn creates_nested_directories() {
        let dir = tempfile::tempdir().unwrap();
        let out = WriteTool
            .run(
                serde_json::json!({"path": "hello_world/src/main.rs", "content": "fn main() {}\n"}),
                dir.path(),
            )
            .await;
        assert!(out.success, "{}", out.content);
        assert_eq!(
            fs::read_to_string(dir.path().join("hello_world/src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
    }

    #[tokio::test]
    async fn rejects_escape_outside_root() {
        let dir = tempfile::tempdir().unwrap();
        let out = WriteTool
            .run(
                serde_json::json!({"path": "../escape.txt", "content": "x"}),
                dir.path(),
            )
            .await;
        assert!(!out.success);
        assert!(out.content.contains("access denied"), "{}", out.content);
    }

    #[tokio::test]
    async fn missing_argument_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let out = WriteTool
            .run(serde_json::json!({"path": "f.txt"}), dir.path())
            .await;
        assert!(!out.success);
        assert!(
            out.content.contains("missing required argument"),
            "{}",
            out.content
        );
    }
}
