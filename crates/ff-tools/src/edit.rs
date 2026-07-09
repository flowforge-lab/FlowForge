//! Exact search/replace editing within the jailed workspace.

use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

use crate::jail::resolve_in_root;
use crate::registry::{Safety, Tool, ToolOutcome};

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn reaches_network(&self) -> bool {
        false
    }
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace an exact string in a workspace file. `old_str` must match exactly \
         and, unless `replace_all` is true, must be unique in the file."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to the workspace root." },
                "old_str": { "type": "string", "description": "Exact text to find." },
                "new_str": { "type": "string", "description": "Replacement text." },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace every occurrence instead of requiring a unique match. Default false."
                }
            },
            "required": ["path", "old_str", "new_str"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::Write
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        let (Some(path), Some(old_str), Some(new_str)) = (
            args.get("path").and_then(Value::as_str),
            args.get("old_str").and_then(Value::as_str),
            args.get("new_str").and_then(Value::as_str),
        ) else {
            return ToolOutcome::error("missing required argument: path, old_str, new_str");
        };
        if old_str.is_empty() {
            // An empty needle matches between every character; `replace_all` would
            // splice `new_str` throughout the file and corrupt it.
            return ToolOutcome::error("old_str must not be empty");
        }
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let resolved = match resolve_in_root(root, path) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::error(e),
        };

        let content = match tokio::fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) => return ToolOutcome::error(format!("cannot read {path}: {e}")),
        };

        let count = content.matches(old_str).count();
        if count == 0 {
            return ToolOutcome::error(format!("old_str not found in {path}"));
        }
        if count > 1 && !replace_all {
            return ToolOutcome::error(format!(
                "old_str is not unique in {path} ({count} matches); pass replace_all or add context"
            ));
        }

        let updated = if replace_all {
            content.replace(old_str, new_str)
        } else {
            content.replacen(old_str, new_str, 1)
        };

        match tokio::fs::write(&resolved, updated).await {
            Ok(()) => ToolOutcome::ok(format!("edited {path} ({count} replacement(s))")),
            Err(e) => ToolOutcome::error(format!("cannot write {path}: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn replaces_unique_match() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        fs::write(&f, "let x = 1;\n").unwrap();
        let out = EditTool
            .run(
                serde_json::json!({"path": "f.txt", "old_str": "x = 1", "new_str": "x = 2"}),
                dir.path(),
            )
            .await;
        assert!(out.success, "{}", out.content);
        assert_eq!(fs::read_to_string(&f).unwrap(), "let x = 2;\n");
    }

    #[tokio::test]
    async fn rejects_ambiguous_without_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("f.txt"), "a\na\n").unwrap();
        let out = EditTool
            .run(
                serde_json::json!({"path": "f.txt", "old_str": "a", "new_str": "b"}),
                dir.path(),
            )
            .await;
        assert!(!out.success);
        assert!(out.content.contains("not unique"));
    }

    #[tokio::test]
    async fn replace_all_rewrites_every_occurrence() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        fs::write(&f, "a\na\n").unwrap();
        let out = EditTool
            .run(
                serde_json::json!({"path": "f.txt", "old_str": "a", "new_str": "b", "replace_all": true}),
                dir.path(),
            )
            .await;
        assert!(out.success);
        assert_eq!(fs::read_to_string(&f).unwrap(), "b\nb\n");
    }

    #[tokio::test]
    async fn empty_old_str_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        fs::write(&f, "abc\n").unwrap();
        let out = EditTool
            .run(
                serde_json::json!({"path": "f.txt", "old_str": "", "new_str": "X", "replace_all": true}),
                dir.path(),
            )
            .await;
        assert!(!out.success);
        assert!(out.content.contains("must not be empty"));
        assert_eq!(fs::read_to_string(&f).unwrap(), "abc\n");
    }

    #[tokio::test]
    async fn missing_old_str_is_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("f.txt"), "hello\n").unwrap();
        let out = EditTool
            .run(
                serde_json::json!({"path": "f.txt", "old_str": "nope", "new_str": "x"}),
                dir.path(),
            )
            .await;
        assert!(!out.success);
        assert!(out.content.contains("not found"));
    }
}
