//! Read a file from the jailed workspace with line numbers and optional range.

use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

use crate::jail::resolve_in_root;
use crate::registry::{Safety, Tool, ToolOutcome};

const MAX_BYTES: u64 = 1024 * 1024;
const DEFAULT_MAX_LINES: usize = 1000;

pub struct ViewTool;

#[async_trait]
impl Tool for ViewTool {
    fn name(&self) -> &str {
        "view"
    }

    fn description(&self) -> &str {
        "Read a text file in the workspace, returned with 1-based line numbers. \
         Optionally restrict to a line range with start_line/end_line."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to the workspace root." },
                "start_line": { "type": "integer", "description": "First line to read (1-based, inclusive)." },
                "end_line": { "type": "integer", "description": "Last line to read (1-based, inclusive)." }
            },
            "required": ["path"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::ReadOnly
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolOutcome::error("missing required argument: path");
        };

        let resolved = match resolve_in_root(root, path) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::error(e),
        };

        match tokio::fs::metadata(&resolved).await {
            Ok(m) if m.len() > MAX_BYTES => {
                return ToolOutcome::error(format!(
                    "file too large: {} bytes (max {MAX_BYTES})",
                    m.len()
                ));
            }
            Ok(_) => {}
            Err(e) => return ToolOutcome::error(format!("cannot read {path}: {e}")),
        }

        let text = match tokio::fs::read_to_string(&resolved).await {
            Ok(t) => t,
            Err(e) => return ToolOutcome::error(format!("cannot read {path}: {e}")),
        };

        let start = args
            .get("start_line")
            .and_then(Value::as_u64)
            .map(|n| n.max(1) as usize)
            .unwrap_or(1);
        let end = args
            .get("end_line")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(start + DEFAULT_MAX_LINES - 1);

        let mut out = String::new();
        let mut shown = 0;
        for (idx, line) in text.lines().enumerate() {
            let n = idx + 1;
            if n < start {
                continue;
            }
            if n > end {
                break;
            }
            out.push_str(&format!("{n:>6} | {line}\n"));
            shown += 1;
        }

        if shown == 0 {
            ToolOutcome::ok(format!("(no lines in range {start}..={end})"))
        } else {
            ToolOutcome::ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn reads_with_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("f.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let out = ViewTool
            .run(serde_json::json!({"path": "f.txt"}), dir.path())
            .await;
        assert!(out.success);
        assert!(out.content.contains("1 | alpha"));
        assert!(out.content.contains("3 | gamma"));
    }

    #[tokio::test]
    async fn honors_line_range() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("f.txt"), "a\nb\nc\nd\n").unwrap();
        let out = ViewTool
            .run(
                serde_json::json!({"path": "f.txt", "start_line": 2, "end_line": 3}),
                dir.path(),
            )
            .await;
        assert!(out.content.contains("2 | b"));
        assert!(out.content.contains("3 | c"));
        assert!(!out.content.contains("1 | a"));
        assert!(!out.content.contains("4 | d"));
    }

    #[tokio::test]
    async fn rejects_escape() {
        let dir = tempfile::tempdir().unwrap();
        let out = ViewTool
            .run(serde_json::json!({"path": "../secret"}), dir.path())
            .await;
        assert!(!out.success);
        assert!(out.content.contains("access denied"));
    }
}
