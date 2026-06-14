//! File discovery by glob, jailed to the workspace root and respecting
//! `.gitignore`. Covers directory listing (pattern `*`) so there is no separate
//! `ls` tool.

use std::path::Path;

use async_trait::async_trait;
use ignore::WalkBuilder;
use serde_json::Value;

use crate::jail::resolve_in_root;
use crate::registry::{Safety, Tool, ToolOutcome};

/// Hard ceiling on returned paths, to keep results bounded for the model.
const MAX_PATHS: usize = 1000;

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files in the workspace whose path matches a glob (e.g. `**/*.rs`, `src/*`), \
         respecting .gitignore. Returns matching paths relative to the workspace root, one \
         per line. Optionally restrict the search to a subdirectory (path)."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob to match against paths, e.g. `**/*.rs`." },
                "path": { "type": "string", "description": "Subdirectory to search, relative to the workspace root. Defaults to the root." }
            },
            "required": ["pattern"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::ReadOnly
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
            return ToolOutcome::error("missing required argument: pattern");
        };
        let rel = args.get("path").and_then(Value::as_str).unwrap_or(".");

        let search_dir = match resolve_in_root(root, rel) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::error(e),
        };
        let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

        let matcher = match globset::Glob::new(pattern) {
            Ok(g) => g.compile_matcher(),
            Err(e) => return ToolOutcome::error(format!("invalid glob `{pattern}`: {e}")),
        };

        let mut paths: Vec<String> = Vec::new();
        let mut truncated = false;
        for entry in WalkBuilder::new(&search_dir)
            .require_git(false)
            .build()
            .flatten()
        {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = entry.path();
            let display = path
                .strip_prefix(&root_canon)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            if matcher.is_match(&display) {
                if paths.len() >= MAX_PATHS {
                    truncated = true;
                    break;
                }
                paths.push(display);
            }
        }

        if paths.is_empty() {
            return ToolOutcome::ok("(no matches)");
        }
        paths.sort();
        if truncated {
            paths.push(format!("(truncated at {MAX_PATHS} paths)"));
        }
        ToolOutcome::ok(paths.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }

    #[tokio::test]
    async fn finds_by_pattern() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/main.rs", "");
        write(dir.path(), "src/lib.rs", "");
        write(dir.path(), "README.md", "");
        let out = GlobTool
            .run(serde_json::json!({"pattern": "**/*.rs"}), dir.path())
            .await;
        assert!(out.success);
        assert!(out.content.contains("src/main.rs"));
        assert!(out.content.contains("src/lib.rs"));
        assert!(!out.content.contains("README.md"));
    }

    #[tokio::test]
    async fn respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".gitignore", "target/\n");
        write(dir.path(), "src/a.rs", "");
        write(dir.path(), "target/b.rs", "");
        let out = GlobTool
            .run(serde_json::json!({"pattern": "**/*.rs"}), dir.path())
            .await;
        assert!(out.content.contains("src/a.rs"));
        assert!(!out.content.contains("target/b.rs"), "{}", out.content);
    }

    #[tokio::test]
    async fn empty_results() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "");
        let out = GlobTool
            .run(serde_json::json!({"pattern": "*.rs"}), dir.path())
            .await;
        assert_eq!(out.content, "(no matches)");
    }

    #[tokio::test]
    async fn rejects_jail_escape() {
        let dir = tempfile::tempdir().unwrap();
        let out = GlobTool
            .run(
                serde_json::json!({"pattern": "*", "path": "../"}),
                dir.path(),
            )
            .await;
        assert!(!out.success);
        assert!(out.content.contains("access denied"));
    }
}
