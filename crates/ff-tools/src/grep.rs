//! ripgrep-backed content search, jailed to the workspace root and respecting
//! `.gitignore`. Removes the small model's need to hand-craft `rg`/`grep` shell
//! invocations (the failure class behind #37).

use std::path::Path;

use async_trait::async_trait;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::SearcherBuilder;
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use serde_json::Value;

use crate::jail::resolve_in_root;
use crate::registry::{Safety, Tool, ToolOutcome};

/// Hard ceiling on returned match lines, so a broad pattern can't flood the model
/// context. The per-call `max_count` further caps matches *per file*.
const MAX_MATCHES: usize = 1000;

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents in the workspace with a regular expression (ripgrep), \
         respecting .gitignore. Returns matching lines as `path:line:text`. Optionally \
         restrict to a subdirectory (path), filter files by a glob, ignore case, or cap \
         matches per file (max_count)."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regular expression to search for." },
                "path": { "type": "string", "description": "Subdirectory to search, relative to the workspace root. Defaults to the root." },
                "glob": { "type": "string", "description": "Only search files whose path matches this glob (e.g. `*.rs`)." },
                "case_insensitive": { "type": "boolean", "description": "Case-insensitive match. Defaults to false." },
                "max_count": { "type": "integer", "description": "Maximum matches to return per file." }
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
        let case_insensitive = args
            .get("case_insensitive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let per_file_cap = args
            .get("max_count")
            .and_then(Value::as_u64)
            .map(|n| n as usize);

        let search_dir = match resolve_in_root(root, rel) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::error(e),
        };
        let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

        // Optional file glob. Built as an `ignore` override so it uses ripgrep's
        // gitignore-style semantics — a pattern without a `/` (e.g. `*.rs`) matches
        // at any depth, not just the top level — and prunes during the walk.
        let overrides = match args.get("glob").and_then(Value::as_str) {
            Some(g) => {
                let mut ob = OverrideBuilder::new(&search_dir);
                if let Err(e) = ob.add(g) {
                    return ToolOutcome::error(format!("invalid glob `{g}`: {e}"));
                }
                match ob.build() {
                    Ok(o) => Some(o),
                    Err(e) => return ToolOutcome::error(format!("invalid glob `{g}`: {e}")),
                }
            }
            None => None,
        };

        let matcher = match RegexMatcherBuilder::new()
            .case_insensitive(case_insensitive)
            .build(pattern)
        {
            Ok(m) => m,
            Err(e) => return ToolOutcome::error(format!("invalid pattern `{pattern}`: {e}")),
        };

        let mut results: Vec<String> = Vec::new();
        let mut truncated = false;

        let mut walk = WalkBuilder::new(&search_dir);
        walk.require_git(false).sort_by_file_path(|a, b| a.cmp(b));
        if let Some(o) = overrides {
            walk.overrides(o);
        }
        for entry in walk.build().flatten() {
            if results.len() >= MAX_MATCHES {
                truncated = true;
                break;
            }
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = entry.path();
            let display = path
                .strip_prefix(&root_canon)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();

            let mut file_matches = 0usize;
            let mut searcher = SearcherBuilder::new().build();
            let _ = searcher.search_path(
                &matcher,
                path,
                UTF8(|lnum, line| {
                    results.push(format!("{display}:{lnum}:{}", line.trim_end_matches('\n')));
                    file_matches += 1;
                    let hit_global = results.len() >= MAX_MATCHES;
                    let hit_file = per_file_cap.is_some_and(|c| file_matches >= c);
                    Ok(!(hit_global || hit_file))
                }),
            );
        }

        if results.is_empty() {
            return ToolOutcome::ok("(no matches)");
        }
        if truncated {
            results.push(format!("(truncated at {MAX_MATCHES} matches)"));
        }
        ToolOutcome::ok(results.join("\n"))
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
    async fn finds_matches_with_path_and_line() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "fn main() {}\nlet x = todo!();\n");
        let out = GrepTool
            .run(serde_json::json!({"pattern": "todo"}), dir.path())
            .await;
        assert!(out.success);
        assert!(
            out.content.contains("a.rs:2:let x = todo!();"),
            "{}",
            out.content
        );
        assert!(!out.content.contains("a.rs:1"));
    }

    #[tokio::test]
    async fn respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".gitignore", "ignored.txt\n");
        write(dir.path(), "kept.txt", "needle here\n");
        write(dir.path(), "ignored.txt", "needle here\n");
        let out = GrepTool
            .run(serde_json::json!({"pattern": "needle"}), dir.path())
            .await;
        assert!(out.content.contains("kept.txt"), "{}", out.content);
        assert!(!out.content.contains("ignored.txt"), "{}", out.content);
    }

    #[tokio::test]
    async fn case_insensitive_flag() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "Hello World\n");
        let sensitive = GrepTool
            .run(serde_json::json!({"pattern": "hello"}), dir.path())
            .await;
        assert_eq!(sensitive.content, "(no matches)");
        let insensitive = GrepTool
            .run(
                serde_json::json!({"pattern": "hello", "case_insensitive": true}),
                dir.path(),
            )
            .await;
        assert!(insensitive.content.contains("a.txt:1:Hello World"));
    }

    #[tokio::test]
    async fn glob_filters_files() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "target\n");
        write(dir.path(), "b.txt", "target\n");
        let out = GrepTool
            .run(
                serde_json::json!({"pattern": "target", "glob": "*.rs"}),
                dir.path(),
            )
            .await;
        assert!(out.content.contains("a.rs"));
        assert!(!out.content.contains("b.txt"));
    }

    #[tokio::test]
    async fn glob_matches_nested_files() {
        // `*.rs` (no slash) must match at any depth, ripgrep-style — not just the
        // top level. This is the footgun the top-level-only `glob_filters_files`
        // test masked.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/deep/foo.rs", "target\n");
        write(dir.path(), "src/bar.txt", "target\n");
        let out = GrepTool
            .run(
                serde_json::json!({"pattern": "target", "glob": "*.rs"}),
                dir.path(),
            )
            .await;
        assert!(out.content.contains("foo.rs"), "{}", out.content);
        assert!(!out.content.contains("bar.txt"), "{}", out.content);
    }

    #[tokio::test]
    async fn empty_results() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "nothing here\n");
        let out = GrepTool
            .run(serde_json::json!({"pattern": "absent"}), dir.path())
            .await;
        assert!(out.success);
        assert_eq!(out.content, "(no matches)");
    }

    #[tokio::test]
    async fn rejects_jail_escape() {
        let dir = tempfile::tempdir().unwrap();
        let out = GrepTool
            .run(
                serde_json::json!({"pattern": "x", "path": "../"}),
                dir.path(),
            )
            .await;
        assert!(!out.success);
        assert!(out.content.contains("access denied"), "{}", out.content);
    }

    #[tokio::test]
    async fn invalid_pattern_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let out = GrepTool
            .run(serde_json::json!({"pattern": "("}), dir.path())
            .await;
        assert!(!out.success);
        assert!(out.content.contains("invalid pattern"));
    }
}
