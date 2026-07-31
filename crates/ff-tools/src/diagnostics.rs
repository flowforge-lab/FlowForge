//! Structured `cargo check` diagnostics (#732). Runs `cargo check
//! --message-format=json --all-targets` and returns deduplicated
//! errors/warnings with file:line:col locations — far cleaner than parsing raw
//! stderr, and it skips codegen.
//!
//! `--all-targets` is on by default deliberately. Plain `cargo check` builds only
//! lib/bin targets, and cargo's own dep-info proves the test sources are never
//! even opened: for `ff-transport-slack`, the lib-only fingerprint mentions
//! `tests_t4.rs` zero times against five times under `--all-targets`. Since 13 of
//! this workspace's 14 crates keep their tests in `#[cfg(test)]` modules inside
//! `src/` (~1750 `#[test]` fns), the lib-only default answers "No errors or
//! warnings." for code that `cargo test` cannot compile — a silent false green
//! with no failure signal to correct it.

use std::collections::HashSet;
use std::path::Path;
use std::process::Stdio;

use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;

use crate::registry::{Safety, Tool, ToolOutcome};

/// Cap output so a noisy workspace doesn't flood the model context.
const MAX_DIAGNOSTICS: usize = 50;

pub struct DiagnosticsTool;

#[async_trait]
impl Tool for DiagnosticsTool {
    fn reaches_network(&self) -> bool {
        false
    }
    fn name(&self) -> &str {
        "diagnostics"
    }

    fn description(&self) -> &str {
        "Run `cargo check` and return structured compiler errors and warnings. \
         Much faster than a full build (skips codegen). Checks test targets too \
         by default, so errors in `#[cfg(test)]` modules and `tests/*.rs` are \
         reported — plain `cargo check` never opens those files and would report \
         a clean result on code `cargo test` cannot compile. Returns deduplicated \
         diagnostics as file:line:col level message."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "package": {
                    "type": "string",
                    "description": "Check only this package (passed as `--package <name>`). Omit to check the whole workspace."
                },
                "all_targets": {
                    "type": "boolean",
                    "description": "Also check test targets — `#[cfg(test)]` modules and `tests/*.rs` (passed as `--all-targets`). Default true: without it cargo compiles only lib/bin targets and never reads the test sources, so it reports no errors on code that `cargo test` cannot compile. Pass false to skip building dev-dependencies when you only touched library code; in steady state that saves under 0.1s, so it is rarely worth it."
                }
            },
            "required": []
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::ReadOnly
    }

    fn max_safety(&self) -> Safety {
        Safety::ReadOnly
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        let package = args
            .get("package")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let all_targets = args
            .get("all_targets")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        match run_check(root, package.as_deref(), all_targets).await {
            Ok(output) => ToolOutcome::ok(output),
            Err(e) => ToolOutcome::error(format!("diagnostics failed: {e}")),
        }
    }
}

/// The exact argv handed to cargo. Split out from [`run_check`] so the wiring is
/// assertable without spawning a build: the tests here can only reach a real
/// compiler by running one, and a test that constructs its own argv would prove
/// nothing about what `run` actually passes.
fn check_args(package: Option<&str>, all_targets: bool) -> Vec<String> {
    let mut args = vec![
        "check".to_string(),
        "--message-format=json".to_string(),
        "--color=never".to_string(),
    ];
    if all_targets {
        args.push("--all-targets".to_string());
    }
    match package {
        Some(pkg) => {
            args.push("--package".to_string());
            args.push(pkg.to_string());
        }
        None => args.push("--workspace".to_string()),
    }
    args
}

async fn run_check(
    root: &Path,
    package: Option<&str>,
    all_targets: bool,
) -> Result<String, String> {
    let mut cmd = Command::new("cargo");
    cmd.args(check_args(package, all_targets));

    cmd.current_dir(root)
        .env("PATH", ff_core::augmented_path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn cargo: {e}"))?;
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("cargo check failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let diagnostics = parse_diagnostics(&stdout);

    if diagnostics.is_empty() {
        return Ok("No errors or warnings.".to_string());
    }

    Ok(diagnostics)
}

/// Parse cargo's JSON message stream and extract deduplicated diagnostics.
fn parse_diagnostics(json_lines: &str) -> String {
    let mut seen = HashSet::new();
    let mut errors = 0u32;
    let mut warnings = 0u32;
    let mut lines: Vec<String> = Vec::new();

    for line in json_lines.lines() {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        // Only process compiler messages, skip build-script/artifact lines.
        if msg.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
            continue;
        }

        let Some(message) = msg.get("message") else {
            continue;
        };

        let level = message
            .get("level")
            .and_then(|l| l.as_str())
            .unwrap_or("unknown");

        // Skip notes — they're attached context, not standalone diagnostics.
        if level == "note" {
            continue;
        }

        let text = message
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("");

        let code = message
            .get("code")
            .and_then(|c| c.get("code"))
            .and_then(|c| c.as_str())
            .unwrap_or("");

        // Find the primary span for location.
        let (file, line_num, col) = message
            .get("spans")
            .and_then(|s| s.as_array())
            .and_then(|spans| {
                spans
                    .iter()
                    .find(|s| s.get("is_primary") == Some(&Value::Bool(true)))
            })
            .map(|span| {
                let f = span
                    .get("file_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let l = span.get("line_start").and_then(|v| v.as_u64()).unwrap_or(0);
                let c = span
                    .get("column_start")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                (f, l, c)
            })
            .unwrap_or(("?", 0, 0));

        // Deduplicate: same file+line+message can appear for multiple targets.
        let key = format!("{file}:{line_num}:{text}");
        if !seen.insert(key) {
            continue;
        }

        match level {
            "error" => errors += 1,
            "warning" => warnings += 1,
            _ => {}
        }

        if lines.len() >= MAX_DIAGNOSTICS {
            continue; // still count but don't render
        }

        let code_suffix = if code.is_empty() {
            String::new()
        } else {
            format!("[{code}]")
        };

        lines.push(format!(
            "{file}:{line_num}:{col} {level}{code_suffix}: {text}"
        ));
    }

    if errors == 0 && warnings == 0 {
        return String::new();
    }

    let mut result = format!("{errors} error(s), {warnings} warning(s)");
    if lines.len() < (errors + warnings) as usize {
        result.push_str(&format!(" (showing first {MAX_DIAGNOSTICS})"));
    }
    result.push_str("\n\n");
    result.push_str(&lines.join("\n"));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_output() {
        assert_eq!(parse_diagnostics(""), "");
    }

    #[test]
    fn parse_clean_check() {
        // Cargo emits non-compiler-message lines for artifacts
        let input = r#"{"reason":"build-script-executed","package_id":"foo"}
{"reason":"compiler-artifact","target":{"name":"foo"}}"#;
        assert_eq!(parse_diagnostics(input), "");
    }

    #[test]
    fn parse_single_error() {
        let input = r#"{"reason":"compiler-message","message":{"level":"error","message":"mismatched types","code":{"code":"E0308"},"spans":[{"file_name":"src/lib.rs","line_start":42,"column_start":5,"is_primary":true}]}}"#;
        let out = parse_diagnostics(input);
        assert!(out.contains("1 error(s), 0 warning(s)"));
        assert!(out.contains("src/lib.rs:42:5 error[E0308]: mismatched types"));
    }

    #[test]
    fn deduplicates_same_error_from_multiple_targets() {
        let line = r#"{"reason":"compiler-message","message":{"level":"error","message":"unused var","code":{"code":"E0001"},"spans":[{"file_name":"src/main.rs","line_start":10,"column_start":1,"is_primary":true}]}}"#;
        let input = format!("{line}\n{line}\n{line}");
        let out = parse_diagnostics(&input);
        assert!(out.contains("1 error(s)"));
        // Only one rendered line
        assert_eq!(out.matches("src/main.rs:10:1").count(), 1);
    }

    #[test]
    fn skips_notes() {
        let input = r#"{"reason":"compiler-message","message":{"level":"note","message":"some hint","code":null,"spans":[]}}"#;
        assert_eq!(parse_diagnostics(input), "");
    }

    #[test]
    fn mixed_errors_and_warnings() {
        let err = r#"{"reason":"compiler-message","message":{"level":"error","message":"type mismatch","code":{"code":"E0308"},"spans":[{"file_name":"src/a.rs","line_start":1,"column_start":1,"is_primary":true}]}}"#;
        let warn = r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused import","code":{"code":"W0001"},"spans":[{"file_name":"src/b.rs","line_start":5,"column_start":1,"is_primary":true}]}}"#;
        let input = format!("{err}\n{warn}");
        let out = parse_diagnostics(&input);
        assert!(out.contains("1 error(s), 1 warning(s)"));
        assert!(out.contains("src/a.rs:1:1 error[E0308]: type mismatch"));
        assert!(out.contains("src/b.rs:5:1 warning[W0001]: unused import"));
    }
    /// Reads `all_targets` the way `run` does, so the assertions below exercise the
    /// real argument plumbing instead of a hand-built value. Keeping this in one
    /// place is what makes the default assertable: a test that passed `true`
    /// explicitly would stay green if `run`'s `unwrap_or` flipped to `false`.
    fn args_from_tool_input(input: serde_json::Value) -> Vec<String> {
        let package = input
            .get("package")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let all_targets = input
            .get("all_targets")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        check_args(package.as_deref(), all_targets)
    }

    #[test]
    fn test_targets_are_checked_by_default() {
        // The bug this guards: plain `cargo check` never opens `#[cfg(test)]` sources,
        // so it reports "No errors or warnings." on code `cargo test` cannot compile.
        // 13 of 14 crates here keep their tests inside `src/`, so the lib-only default
        // was a silent false green across almost the whole workspace.
        let args = args_from_tool_input(serde_json::json!({}));
        assert!(
            args.contains(&"--all-targets".to_string()),
            "omitting `all_targets` must still check test targets; got {args:?}"
        );

        let with_pkg = args_from_tool_input(serde_json::json!({"package": "ff-core"}));
        assert!(
            with_pkg.contains(&"--all-targets".to_string()),
            "a package-scoped check must check test targets too; got {with_pkg:?}"
        );
    }

    #[test]
    fn all_targets_can_be_turned_off() {
        // The escape hatch has to actually work: checking only downstream library
        // compilation is the one case where skipping dev-dependencies pays.
        let args = args_from_tool_input(serde_json::json!({"all_targets": false}));
        assert!(
            !args.contains(&"--all-targets".to_string()),
            "`all_targets: false` must drop the flag; got {args:?}"
        );
        // Dropping the flag must not disturb the rest of the invocation.
        assert!(args.contains(&"--message-format=json".to_string()));
        assert!(args.contains(&"--workspace".to_string()));
    }

    #[test]
    fn package_scope_replaces_workspace_scope() {
        let ws = check_args(None, true);
        assert!(ws.contains(&"--workspace".to_string()));
        assert!(!ws.contains(&"--package".to_string()));

        let pkg = check_args(Some("ff-tools"), true);
        assert_eq!(
            pkg.iter()
                .position(|a| a == "--package")
                .map(|i| &pkg[i + 1]),
            Some(&"ff-tools".to_string()),
            "`--package` must be followed by the package name; got {pkg:?}"
        );
        assert!(
            !pkg.contains(&"--workspace".to_string()),
            "package scope and workspace scope are mutually exclusive; got {pkg:?}"
        );
    }

    #[test]
    fn check_is_the_subcommand_and_json_is_requested() {
        // `parse_diagnostics` only understands cargo's JSON message stream, and the
        // whole tool is a `check` (no codegen) -- both are load-bearing, so pin them.
        let args = check_args(None, true);
        assert_eq!(args.first().map(String::as_str), Some("check"));
        assert!(args.contains(&"--message-format=json".to_string()));
        assert!(args.contains(&"--color=never".to_string()));
    }
}
