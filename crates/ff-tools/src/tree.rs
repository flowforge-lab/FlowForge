//! Directory tree discovery, jailed to the workspace root and respecting
//! `.gitignore`. Gives the model a cheap way to orient itself before choosing
//! between `glob`, `grep`, or `view`.

use std::path::Path;

use async_trait::async_trait;
use ignore::WalkBuilder;
use serde_json::Value;

use crate::jail::resolve_in_root;
use crate::registry::{Safety, Tool, ToolOutcome};

/// Hard ceiling on returned entries, so broad trees stay bounded for the model.
const MAX_ENTRIES: usize = 1000;

pub struct TreeTool;

#[async_trait]
impl Tool for TreeTool {
    fn reaches_network(&self) -> bool {
        false
    }
    fn name(&self) -> &str {
        "tree"
    }

    fn description(&self) -> &str {
        "List a directory tree in the workspace, respecting .gitignore. Returns \
         root-relative paths with indentation showing nesting. Optionally restrict \
         to a subdirectory (path), cap recursion depth (max_depth), or show only \
         directories (dirs_only)."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Subdirectory to list, relative to the workspace root. Defaults to the root." },
                "max_depth": { "type": "integer", "description": "Maximum depth below the selected directory to include. 1 shows direct children only." },
                "dirs_only": { "type": "boolean", "description": "Only include directories. Defaults to false." }
            }
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::ReadOnly
    }

    fn max_safety(&self) -> Safety {
        Safety::ReadOnly
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        let rel = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let max_depth = args
            .get("max_depth")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let dirs_only = args
            .get("dirs_only")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let search_dir = match resolve_in_root(root, rel) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::error(e),
        };
        if !search_dir.is_dir() {
            return ToolOutcome::error(format!("path is not a directory: {rel}"));
        }

        let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

        let mut walk = WalkBuilder::new(&search_dir);
        walk.require_git(false).sort_by_file_path(|a, b| a.cmp(b));
        if let Some(depth) = max_depth {
            // `ignore` counts the walk root as depth 0, so depth 1 is the
            // selected directory's direct children.
            walk.max_depth(Some(depth));
        }

        let mut entries: Vec<String> = Vec::new();
        let mut truncated = false;

        for entry in walk.build().flatten() {
            if entry.depth() == 0 {
                continue;
            }

            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            if dirs_only && !is_dir {
                continue;
            }

            if entries.len() >= MAX_ENTRIES {
                truncated = true;
                break;
            }

            let path = entry.path();
            let display = path
                .strip_prefix(&root_canon)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            let suffix = if is_dir { "/" } else { "" };
            let indent = "  ".repeat(entry.depth().saturating_sub(1));
            entries.push(format!("{indent}{display}{suffix}"));
        }

        if entries.is_empty() {
            return ToolOutcome::ok("(empty directory)");
        }
        if truncated {
            entries.push(format!("(truncated at {MAX_ENTRIES} entries)"));
        }
        ToolOutcome::ok(entries.join("\n"))
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
    async fn lists_basic_tree_deterministically() {
        // Anchor under `/tmp` so the workspace's gitignore / `.git` chain (which
        // would otherwise bleed in via `tempdir()` when TMPDIR points inside
        // the repo, e.g. FlowForge's `.ff-scratch/`) doesn't hide fixtures.
        let dir = tempfile::Builder::new()
            .prefix("ff-tree-")
            .tempdir_in("/tmp")
            .unwrap();
        write(dir.path(), "src/main.rs", "");
        write(dir.path(), "README.md", "");

        let out = TreeTool.run(serde_json::json!({}), dir.path()).await;

        assert!(out.success);
        assert_eq!(out.content, "README.md\nsrc/\n  src/main.rs");
    }

    #[tokio::test]
    async fn honors_max_depth_and_dirs_only() {
        // See `lists_basic_tree_deterministically` for why these fixtures
        // anchor under `/tmp`.
        let dir = tempfile::Builder::new()
            .prefix("ff-tree-")
            .tempdir_in("/tmp")
            .unwrap();
        write(dir.path(), "src/deep/main.rs", "");
        write(dir.path(), "src/lib.rs", "");
        write(dir.path(), "top.txt", "");

        let shallow = TreeTool
            .run(serde_json::json!({"max_depth": 1}), dir.path())
            .await;
        assert!(shallow.content.contains("src/"), "{}", shallow.content);
        assert!(shallow.content.contains("top.txt"), "{}", shallow.content);
        assert!(
            !shallow.content.contains("src/lib.rs"),
            "{}",
            shallow.content
        );
        assert!(
            !shallow.content.contains("src/deep/"),
            "{}",
            shallow.content
        );

        let dirs = TreeTool
            .run(serde_json::json!({"dirs_only": true}), dir.path())
            .await;
        assert_eq!(dirs.content, "src/\n  src/deep/");
    }

    #[tokio::test]
    async fn respects_gitignore() {
        // See `lists_basic_tree_deterministically` for why these fixtures
        // anchor under `/tmp`.
        let dir = tempfile::Builder::new()
            .prefix("ff-tree-")
            .tempdir_in("/tmp")
            .unwrap();
        write(dir.path(), ".gitignore", "target/\nignored.txt\n");
        write(dir.path(), "src/lib.rs", "");
        write(dir.path(), "target/debug.log", "");
        write(dir.path(), "ignored.txt", "");

        let out = TreeTool.run(serde_json::json!({}), dir.path()).await;

        assert!(out.content.contains("src/"), "{}", out.content);
        assert!(out.content.contains("src/lib.rs"), "{}", out.content);
        assert!(!out.content.contains("target/"), "{}", out.content);
        assert!(!out.content.contains("ignored.txt"), "{}", out.content);
    }

    #[tokio::test]
    async fn rejects_jail_escape() {
        let dir = tempfile::tempdir().unwrap();
        let out = TreeTool
            .run(serde_json::json!({"path": "../"}), dir.path())
            .await;

        assert!(!out.success);
        assert!(out.content.contains("access denied"), "{}", out.content);
    }

    #[tokio::test]
    async fn empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("empty")).unwrap();

        let out = TreeTool
            .run(serde_json::json!({"path": "empty"}), dir.path())
            .await;

        assert!(out.success);
        assert_eq!(out.content, "(empty directory)");
    }

    #[tokio::test]
    async fn truncates_large_output() {
        // See `lists_basic_tree_deterministically` for why these fixtures
        // anchor under `/tmp`.
        let dir = tempfile::Builder::new()
            .prefix("ff-tree-")
            .tempdir_in("/tmp")
            .unwrap();
        for i in 0..=MAX_ENTRIES {
            write(dir.path(), &format!("file-{i:04}.txt"), "");
        }

        let out = TreeTool.run(serde_json::json!({}), dir.path()).await;

        assert!(out.success);
        assert_eq!(out.content.lines().count(), MAX_ENTRIES + 1);
        assert!(
            out.content
                .ends_with(&format!("(truncated at {MAX_ENTRIES} entries)")),
            "{}",
            out.content
        );
    }
}
