//! Multi-file patch application within the jailed workspace.
//!
//! Accepts a Codex/OpenAI-style patch envelope (V4A) describing add/update/delete
//! operations across one or more files and applies them in two phases:
//!
//! - **Validation** is all-or-nothing. Every operation is checked against the
//!   current on-disk contents (or an earlier op's planned result) *before* a
//!   single byte is written, and a single mismatched hunk aborts the whole patch.
//!   The model can never land an edit whose precondition did not hold.
//! - **Commit** writes the validated final state file by file in order. This
//!   phase is not yet atomic across files: if write #1 succeeds and write #2
//!   fails on an I/O error (permissions, disk full), #1 is already on disk and
//!   the error is surfaced verbatim. True multi-file atomicity would require a
//!   temp-write + rename/swap commit; tracked as a follow-up until then.
//!
//! The envelope is context-anchored rather than line-numbered, which is robust to
//! a model miscounting line offsets:
//!
//! ```text
//! *** Begin Patch
//! *** Update File: src/foo.rs
//! @@
//!  unchanged context
//! -removed line
//! +added line
//! *** Add File: src/new.rs
//! +full contents
//! *** Delete File: src/old.rs
//! *** End Patch
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

use crate::jail::{resolve_for_create, resolve_in_root};
use crate::registry::{Safety, Tool, ToolOutcome};

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const ADD: &str = "*** Add File: ";
const UPDATE: &str = "*** Update File: ";
const DELETE: &str = "*** Delete File: ";

/// One reconstructed hunk: the exact `old` block to find and the `new` block to
/// substitute. Both are derived from the unified-diff prefixes (` `/`-` build
/// `old`; ` `/`+` build `new`), so a pure context block with no `-`/`+` is a no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hunk {
    old: String,
    new: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileOp {
    Add { path: String, contents: String },
    Update { path: String, hunks: Vec<Hunk> },
    Delete { path: String },
}

pub struct ApplyPatchTool;

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply a multi-file patch within the workspace; every op is validated \
         all-or-nothing (a single bad hunk aborts) before any file is written. \
         The `patch` is a Codex-style envelope delimited by `*** Begin Patch` / \
         `*** End Patch`, with `*** Add File:`, `*** Update File:`, and \
         `*** Delete File:` sections. Update sections use unified-diff hunks \
         (lines prefixed with a space for context, `-` to remove, `+` to add), \
         optionally separated by `@@` markers. Prefer this over multiple `edit` \
         calls when a change spans several files or hunks."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "The full patch envelope, from `*** Begin Patch` to `*** End Patch`."
                }
            },
            "required": ["patch"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::Write
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        let Some(patch) = args.get("patch").and_then(Value::as_str) else {
            return ToolOutcome::error("missing required argument: patch");
        };

        let ops = match parse_patch(patch) {
            Ok(ops) => ops,
            Err(e) => return ToolOutcome::error(format!("invalid patch: {e}")),
        };
        if ops.is_empty() {
            return ToolOutcome::error("invalid patch: no file operations");
        }

        // Phase 1: validate every op and compute the planned final state in memory.
        // A `Some(content)` entry is a write; `None` is a delete. Later ops see the
        // results of earlier ones so a patch can update then delete the same file.
        let mut planned: BTreeMap<String, Option<String>> = BTreeMap::new();
        for op in &ops {
            if let Err(e) = plan_op(root, op, &mut planned).await {
                return ToolOutcome::error(format!("patch not applied: {e}"));
            }
        }

        // Phase 2: commit. Validation already passed, so these are expected to
        // succeed; any I/O error here is surfaced verbatim.
        let mut written = 0usize;
        let mut deleted = 0usize;
        for (path, state) in &planned {
            match state {
                Some(contents) => {
                    let resolved = match resolve_for_create(root, path) {
                        Ok(p) => p,
                        Err(e) => return ToolOutcome::error(e),
                    };
                    if let Some(parent) = resolved.parent() {
                        if let Err(e) = tokio::fs::create_dir_all(parent).await {
                            return ToolOutcome::error(format!(
                                "cannot create parent of {path}: {e}"
                            ));
                        }
                    }
                    if let Err(e) = tokio::fs::write(&resolved, contents).await {
                        return ToolOutcome::error(format!("cannot write {path}: {e}"));
                    }
                    written += 1;
                }
                None => {
                    let resolved = match resolve_in_root(root, path) {
                        Ok(p) => p,
                        Err(e) => return ToolOutcome::error(e),
                    };
                    if let Err(e) = tokio::fs::remove_file(&resolved).await {
                        return ToolOutcome::error(format!("cannot delete {path}: {e}"));
                    }
                    deleted += 1;
                }
            }
        }

        ToolOutcome::ok(format!(
            "applied patch: {written} file(s) written, {deleted} file(s) deleted"
        ))
    }
}

/// Validate a single op against current state (disk or an earlier op's result) and
/// record its planned outcome into `planned`. Reads nothing on failure paths beyond
/// what is needed to validate, and never writes.
async fn plan_op(
    root: &Path,
    op: &FileOp,
    planned: &mut BTreeMap<String, Option<String>>,
) -> Result<(), String> {
    match op {
        FileOp::Add { path, contents } => {
            // Reject clobbering an existing file (or one an earlier op created):
            // the model should use Update to change a file that already exists.
            let exists = match planned.get(path) {
                Some(state) => state.is_some(),
                None => file_exists(root, path).await?,
            };
            if exists {
                return Err(format!(
                    "{path}: Add File target already exists; use Update"
                ));
            }
            planned.insert(path.clone(), Some(contents.clone()));
            Ok(())
        }
        FileOp::Delete { path } => {
            let exists = match planned.get(path) {
                Some(state) => state.is_some(),
                None => file_exists(root, path).await?,
            };
            if !exists {
                return Err(format!("{path}: Delete File target does not exist"));
            }
            planned.insert(path.clone(), None);
            Ok(())
        }
        FileOp::Update { path, hunks } => {
            let current = match planned.get(path) {
                Some(Some(content)) => content.clone(),
                Some(None) => return Err(format!("{path}: cannot Update a deleted file")),
                None => read_existing(root, path).await?,
            };
            let updated = apply_hunks(path, &current, hunks)?;
            planned.insert(path.clone(), Some(updated));
            Ok(())
        }
    }
}

async fn file_exists(root: &Path, path: &str) -> Result<bool, String> {
    // Use the create-aware resolver so a not-yet-existing nested target (e.g.
    // `sub/new.txt` in a fresh dir) is treated as absent, not a hard error.
    match resolve_for_create(root, path) {
        Ok(p) => Ok(tokio::fs::try_exists(&p).await.unwrap_or(false)),
        // A path that cannot resolve inside the root is a hard error, not "absent".
        Err(e) => Err(e),
    }
}

async fn read_existing(root: &Path, path: &str) -> Result<String, String> {
    let resolved = resolve_in_root(root, path)?;
    tokio::fs::read_to_string(&resolved)
        .await
        .map_err(|e| format!("{path}: cannot read for Update: {e}"))
}

/// Apply each hunk's `old -> new` substitution in order against the evolving
/// content. `old` must occur exactly once (unique), mirroring `edit`'s contract,
/// so an ambiguous anchor aborts rather than guessing.
fn apply_hunks(path: &str, content: &str, hunks: &[Hunk]) -> Result<String, String> {
    let mut out = content.to_string();
    for (i, hunk) in hunks.iter().enumerate() {
        // A pure-context block with no `-`/`+` reconstructs identical `old` and
        // `new`; skip it as a no-op.
        if hunk.old == hunk.new {
            continue;
        }
        // A hunk with only `+` lines has an empty `old`, so there is nothing to
        // anchor where the additions should land. An empty needle would instead
        // match at every position ("ambiguous (N matches)"), so reject it up
        // front with a targeted message. A fuzzy pass may later anchor such a
        // hunk to the file head/tail; see docs/plans/apply-patch-followups.md.
        if hunk.old.is_empty() {
            return Err(format!(
                "{path}: hunk {} has no context line to anchor the addition; \
                 prepend a context line (` `) before the `+` line(s)",
                i + 1
            ));
        }
        let count = out.matches(&hunk.old).count();
        if count == 0 {
            // Reconstructed `old` lines each carry a trailing `\n`, so a hunk
            // touching the final line of a no-trailing-newline file misses here
            // and fails safe. A trailing-newline-normalizing match pass would
            // relax this; see docs/plans/apply-patch-followups.md.
            return Err(format!(
                "{path}: hunk {} context not found in current file",
                i + 1
            ));
        }
        if count > 1 {
            return Err(format!(
                "{path}: hunk {} context is ambiguous ({count} matches); add more context lines",
                i + 1
            ));
        }
        out = out.replacen(&hunk.old, &hunk.new, 1);
    }
    Ok(out)
}

/// Parse a V4A patch envelope into ordered file operations. Strict: requires the
/// begin/end sentinels and a recognized section header before any body lines.
fn parse_patch(patch: &str) -> Result<Vec<FileOp>, String> {
    let mut lines = patch.lines().peekable();

    // Skip blank leading lines, then require the begin sentinel.
    loop {
        match lines.peek() {
            Some(l) if l.trim().is_empty() => {
                lines.next();
            }
            Some(l) if l.trim() == BEGIN => {
                lines.next();
                break;
            }
            _ => return Err(format!("missing `{BEGIN}` header")),
        }
    }

    let mut ops = Vec::new();
    let mut saw_end = false;

    while let Some(line) = lines.next() {
        let trimmed = line.trim_end();
        if trimmed.trim() == END {
            saw_end = true;
            break;
        }
        if let Some(path) = trimmed.strip_prefix(ADD) {
            let path = clean_path(path)?;
            let mut contents = String::new();
            while let Some(peek) = lines.peek() {
                if peek.starts_with("*** ") {
                    break;
                }
                let body = lines.next().unwrap();
                let l = body
                    .strip_prefix('+')
                    .ok_or_else(|| format!("{path}: Add File body line must start with `+`"))?;
                contents.push_str(l);
                contents.push('\n');
            }
            ops.push(FileOp::Add { path, contents });
        } else if let Some(path) = trimmed.strip_prefix(DELETE) {
            let path = clean_path(path)?;
            ops.push(FileOp::Delete { path });
        } else if let Some(path) = trimmed.strip_prefix(UPDATE) {
            let path = clean_path(path)?;
            let hunks = parse_update_body(&path, &mut lines)?;
            if hunks.is_empty() {
                return Err(format!("{path}: Update File has no hunks"));
            }
            ops.push(FileOp::Update { path, hunks });
        } else if trimmed.trim().is_empty() {
            // Tolerate blank lines between sections.
        } else {
            return Err(format!("unexpected line outside a file section: {trimmed}"));
        }
    }

    if !saw_end {
        return Err(format!("missing `{END}` trailer"));
    }
    Ok(ops)
}

/// Collect the hunks of an `*** Update File:` section. A `@@` line starts a new
/// hunk; otherwise each line's first character selects context (` `), removal
/// (`-`), or addition (`+`).
fn parse_update_body(
    path: &str,
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
) -> Result<Vec<Hunk>, String> {
    let mut hunks = Vec::new();
    let mut cur_old = String::new();
    let mut cur_new = String::new();
    let mut cur_started = false;

    let flush = |hunks: &mut Vec<Hunk>, old: &mut String, new: &mut String, started: &mut bool| {
        if *started {
            hunks.push(Hunk {
                old: std::mem::take(old),
                new: std::mem::take(new),
            });
            *started = false;
        }
    };

    while let Some(peek) = lines.peek() {
        if peek.starts_with("*** ") {
            break;
        }
        let line = lines.next().unwrap();
        if let Some(_header) = line.strip_prefix("@@") {
            // A `@@` marker delimits hunks; its trailing text is a human-oriented
            // locator we do not need because each hunk carries its own context.
            flush(&mut hunks, &mut cur_old, &mut cur_new, &mut cur_started);
            continue;
        }
        let (tag, rest) = match line.chars().next() {
            Some(c @ (' ' | '-' | '+')) => (c, &line[1..]),
            // A fully empty line in a hunk body is treated as a blank context line.
            None => (' ', ""),
            Some(_) => {
                return Err(format!(
                    "{path}: hunk line must start with ' ', '-', or '+': {line}"
                ))
            }
        };
        cur_started = true;
        match tag {
            ' ' => {
                cur_old.push_str(rest);
                cur_old.push('\n');
                cur_new.push_str(rest);
                cur_new.push('\n');
            }
            '-' => {
                cur_old.push_str(rest);
                cur_old.push('\n');
            }
            '+' => {
                cur_new.push_str(rest);
                cur_new.push('\n');
            }
            _ => unreachable!(),
        }
    }
    flush(&mut hunks, &mut cur_old, &mut cur_new, &mut cur_started);
    Ok(hunks)
}

fn clean_path(raw: &str) -> Result<String, String> {
    let p = raw.trim();
    if p.is_empty() {
        return Err("file section has an empty path".to_string());
    }
    Ok(p.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn env(begin_body: &str) -> String {
        format!("{BEGIN}\n{begin_body}\n{END}\n")
    }

    #[test]
    fn parses_add_update_delete() {
        let patch = env(
            "*** Add File: a.txt\n+hello\n*** Delete File: gone.txt\n*** Update File: b.txt\n@@\n ctx\n-old\n+new",
        );
        let ops = parse_patch(&patch).unwrap();
        assert_eq!(ops.len(), 3);
        assert!(matches!(&ops[0], FileOp::Add { path, contents }
            if path == "a.txt" && contents == "hello\n"));
        assert!(matches!(&ops[1], FileOp::Delete { path } if path == "gone.txt"));
        match &ops[2] {
            FileOp::Update { path, hunks } => {
                assert_eq!(path, "b.txt");
                assert_eq!(hunks.len(), 1);
                assert_eq!(hunks[0].old, "ctx\nold\n");
                assert_eq!(hunks[0].new, "ctx\nnew\n");
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn rejects_missing_begin() {
        let err = parse_patch("*** Update File: x\n-a\n+b\n*** End Patch\n").unwrap_err();
        assert!(err.contains("Begin Patch"), "{err}");
    }

    #[test]
    fn rejects_missing_end() {
        let err = parse_patch("*** Begin Patch\n*** Delete File: x\n").unwrap_err();
        assert!(err.contains("End Patch"), "{err}");
    }

    #[test]
    fn rejects_garbage_body_line() {
        let err = parse_patch(&env("garbage line")).unwrap_err();
        assert!(err.contains("unexpected line"), "{err}");
    }

    #[tokio::test]
    async fn applies_multi_file_patch_atomically() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("b.txt"), "ctx\nold\ntail\n").unwrap();
        fs::write(dir.path().join("gone.txt"), "bye\n").unwrap();
        let patch = env(
            "*** Add File: sub/a.txt\n+new file\n*** Update File: b.txt\n@@\n ctx\n-old\n+new\n*** Delete File: gone.txt",
        );
        let out = ApplyPatchTool
            .run(serde_json::json!({ "patch": patch }), dir.path())
            .await;
        assert!(out.success, "{}", out.content);
        assert_eq!(
            fs::read_to_string(dir.path().join("sub/a.txt")).unwrap(),
            "new file\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "ctx\nnew\ntail\n"
        );
        assert!(!dir.path().join("gone.txt").exists());
    }

    #[tokio::test]
    async fn context_mismatch_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("b.txt"), "real content\n").unwrap();
        // Two ops: a valid add and an update whose context does not match. The
        // failed update must roll back the whole patch -> a.txt must NOT appear.
        let patch = env(
            "*** Add File: a.txt\n+should not survive\n*** Update File: b.txt\n@@\n-does not exist\n+x",
        );
        let out = ApplyPatchTool
            .run(serde_json::json!({ "patch": patch }), dir.path())
            .await;
        assert!(!out.success);
        assert!(out.content.contains("not found"), "{}", out.content);
        assert!(
            !dir.path().join("a.txt").exists(),
            "add leaked despite abort"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "real content\n"
        );
    }

    #[tokio::test]
    async fn ambiguous_hunk_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("b.txt"), "dup\ndup\n").unwrap();
        let patch = env("*** Update File: b.txt\n@@\n-dup\n+x");
        let out = ApplyPatchTool
            .run(serde_json::json!({ "patch": patch }), dir.path())
            .await;
        assert!(!out.success);
        assert!(out.content.contains("ambiguous"), "{}", out.content);
    }

    #[tokio::test]
    async fn pure_addition_hunk_gives_targeted_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("b.txt"), "keep\n").unwrap();
        // A hunk with only `+` lines has no context/removal to anchor where the
        // addition lands; it must be rejected with a clear message rather than
        // the misleading "N matches" an empty needle would produce.
        let patch = env("*** Update File: b.txt\n@@\n+injected");
        let out = ApplyPatchTool
            .run(serde_json::json!({ "patch": patch }), dir.path())
            .await;
        assert!(!out.success);
        assert!(
            out.content.contains("no context line to anchor"),
            "{}",
            out.content
        );
        assert!(
            !out.content.contains("ambiguous"),
            "pure-addition should not be reported as ambiguity: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn add_rejects_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "already\n").unwrap();
        let patch = env("*** Add File: a.txt\n+new");
        let out = ApplyPatchTool
            .run(serde_json::json!({ "patch": patch }), dir.path())
            .await;
        assert!(!out.success);
        assert!(out.content.contains("already exists"), "{}", out.content);
    }

    #[tokio::test]
    async fn delete_missing_file_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let patch = env("*** Delete File: nope.txt");
        let out = ApplyPatchTool
            .run(serde_json::json!({ "patch": patch }), dir.path())
            .await;
        assert!(!out.success);
        assert!(out.content.contains("does not exist"), "{}", out.content);
    }

    #[tokio::test]
    async fn jail_escape_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let patch = env("*** Add File: ../escape.txt\n+pwned");
        let out = ApplyPatchTool
            .run(serde_json::json!({ "patch": patch }), dir.path())
            .await;
        assert!(!out.success);
        assert!(out.content.contains("access denied"), "{}", out.content);
        assert!(!dir.path().parent().unwrap().join("escape.txt").exists());
    }

    #[tokio::test]
    async fn multiple_hunks_apply_in_sequence() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("f.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        let patch = env("*** Update File: f.txt\n@@\n one\n-two\n+TWO\n@@\n three\n-four\n+FOUR");
        let out = ApplyPatchTool
            .run(serde_json::json!({ "patch": patch }), dir.path())
            .await;
        assert!(out.success, "{}", out.content);
        assert_eq!(
            fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "one\nTWO\nthree\nFOUR\n"
        );
    }

    #[tokio::test]
    async fn missing_patch_arg_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let out = ApplyPatchTool.run(serde_json::json!({}), dir.path()).await;
        assert!(!out.success);
        assert!(out.content.contains("missing required argument"));
    }
}
