//! Structured test runner (#852). Runs a test command and parses the output
//! into a summary + structured failure details. On all-pass returns just a
//! compact summary (minimal tokens); on failure includes name, message, and
//! location for each failing test.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::process::Command;

use crate::registry::{Safety, Tool, ToolOutcome};

/// Hard cap on output to avoid flooding model context.
const MAX_OUTPUT_BYTES: usize = 16_000;
/// Default timeout for test commands.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

pub struct TestRunnerTool;

#[async_trait]
impl Tool for TestRunnerTool {
    fn name(&self) -> &str {
        "test_runner"
    }

    fn description(&self) -> &str {
        "Run a test command and return structured results. On success: a compact \
         summary (e.g. '162 passed'). On failure: summary + each failure's name, \
         message, and location. Supports cargo test, pytest, vitest/jest. Falls \
         back to raw output for unrecognized frameworks."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The test command to run (e.g. 'cargo test -p ff-agent', 'pytest tests/', 'npx vitest run')."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default 120)."
                }
            },
            "required": ["command"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::Write
    }

    fn max_safety(&self) -> Safety {
        Safety::Write
    }

    fn min_safety(&self) -> Safety {
        Safety::Write
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        let command = match args.get("command").and_then(|v| v.as_str()) {
            Some(cmd) => cmd.to_string(),
            None => return ToolOutcome::error("missing required parameter: command"),
        };
        let timeout = args
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        match run_tests(root, &command, timeout).await {
            Ok(output) => ToolOutcome::ok(output),
            Err(e) => ToolOutcome::error(format!("test_runner failed: {e}")),
        }
    }
}

async fn run_tests(root: &Path, command: &str, timeout_secs: u64) -> Result<String, String> {
    let output = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(root)
            .env("PATH", ff_core::augmented_path())
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .output(),
    )
    .await
    .map_err(|_| format!("timed out after {timeout_secs}s"))?
    .map_err(|e| format!("failed to spawn: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    let result = parse_test_output(&combined, output.status.success());

    Ok(result)
}

/// Parsed test result.
struct TestResult {
    passed: u32,
    failed: u32,
    skipped: u32,
    failures: Vec<Failure>,
    raw_tail: Option<String>,
}

struct Failure {
    name: String,
    message: String,
}

fn parse_test_output(output: &str, success: bool) -> String {
    // Try each parser in order; fall back to raw output.
    if let Some(result) = parse_cargo_test(output) {
        return format_result(&result);
    }
    if let Some(result) = parse_pytest(output) {
        return format_result(&result);
    }
    if let Some(result) = parse_vitest(output) {
        return format_result(&result);
    }

    // Fallback: return raw output, truncated.
    let status = if success { "PASSED" } else { "FAILED" };
    let truncated = truncate_output(output, MAX_OUTPUT_BYTES);
    format!("Test {status} (output not parsed — raw below):\n\n{truncated}")
}

fn format_result(result: &TestResult) -> String {
    let mut out = String::new();

    // Summary line
    out.push_str(&format!("Tests: {} passed", result.passed));
    if result.failed > 0 {
        out.push_str(&format!(", {} failed", result.failed));
    }
    if result.skipped > 0 {
        out.push_str(&format!(", {} skipped", result.skipped));
    }
    out.push_str(&format!(
        " (total {})",
        result.passed + result.failed + result.skipped
    ));

    if result.failed == 0 {
        // All passed — just the summary line. Minimal tokens.
        return out;
    }

    out.push_str("\n\nFailures:\n");
    for (i, f) in result.failures.iter().enumerate() {
        out.push_str(&format!("\n{}. {}\n", i + 1, f.name));
        // Truncate long failure messages
        let msg = if f.message.len() > 2000 {
            let mut end = 2000;
            while !f.message.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}...[truncated]", &f.message[..end])
        } else {
            f.message.clone()
        };
        out.push_str(&msg);
        if !msg.ends_with('\n') {
            out.push('\n');
        }
    }

    if let Some(tail) = &result.raw_tail {
        out.push_str(&format!("\n---\n{tail}"));
    }

    out
}

// --- Cargo test parser ---

fn parse_cargo_test(output: &str) -> Option<TestResult> {
    // Look for "test result: " lines
    let mut total_passed = 0u32;
    let mut total_failed = 0u32;
    let mut total_ignored = 0u32;
    let mut found_result = false;

    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("test result: ") {
            found_result = true;
            // "ok. 162 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"
            // "FAILED. 160 passed; 2 failed; 0 ignored; ..."
            for part in rest.split(';') {
                let part = part.trim();
                if let Some(n) = extract_number_before(part, " passed") {
                    total_passed += n;
                } else if let Some(n) = extract_number_before(part, " failed") {
                    total_failed += n;
                } else if let Some(n) = extract_number_before(part, " ignored") {
                    total_ignored += n;
                }
            }
        }
    }

    if !found_result {
        return None;
    }

    // Parse failures section
    let failures = parse_cargo_failures(output);

    Some(TestResult {
        passed: total_passed,
        failed: total_failed,
        skipped: total_ignored,
        failures,
        raw_tail: None,
    })
}

fn parse_cargo_failures(output: &str) -> Vec<Failure> {
    // Look for "failures:" section followed by "---- name stdout ----" blocks
    let Some(failures_start) = output.find("\nfailures:\n") else {
        // Try alternate format: "failures:" at line start
        let Some(start) = output.find("failures:\n") else {
            return Vec::new();
        };
        return parse_failure_blocks(&output[start..]);
    };
    parse_failure_blocks(&output[failures_start..])
}

fn parse_failure_blocks(section: &str) -> Vec<Failure> {
    let mut failures = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_body = String::new();

    for line in section.lines() {
        if let Some(rest) = line.strip_prefix("---- ") {
            if let Some(name) = rest.strip_suffix(" stdout ----") {
                // Flush previous
                if let Some(prev_name) = current_name.take() {
                    failures.push(Failure {
                        name: prev_name,
                        message: current_body.trim().to_string(),
                    });
                }
                current_name = Some(name.to_string());
                current_body.clear();
                continue;
            }
        }
        if line == "failures:" || line.starts_with("    ") && current_name.is_none() {
            // Skip the "failures:" header or the name-only list at the end
            continue;
        }
        if current_name.is_some() {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }
    // Flush last
    if let Some(name) = current_name {
        failures.push(Failure {
            name,
            message: current_body.trim().to_string(),
        });
    }
    failures
}

// --- Pytest parser ---

fn parse_pytest(output: &str) -> Option<TestResult> {
    // Look for "=== N passed ===" or "=== N passed, M failed ==="
    let summary_line = output
        .lines()
        .rev()
        .find(|l| l.contains(" passed") && (l.contains("===") || l.contains("====")))?;

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;

    for word_pair in summary_line.split(',') {
        let trimmed = word_pair.trim().trim_matches('=').trim();
        if let Some(n) = extract_number_before(trimmed, " passed") {
            passed = n;
        } else if let Some(n) = extract_number_before(trimmed, " failed") {
            failed = n;
        } else if let Some(n) = extract_number_before(trimmed, " skipped") {
            skipped = n;
        } else if let Some(n) = extract_number_before(trimmed, " error") {
            failed += n;
        }
    }

    if passed == 0 && failed == 0 {
        return None;
    }

    let failures = parse_pytest_failures(output);

    Some(TestResult {
        passed,
        failed,
        skipped,
        failures,
        raw_tail: None,
    })
}

fn parse_pytest_failures(output: &str) -> Vec<Failure> {
    let mut failures = Vec::new();
    let mut in_failures = false;
    let mut current_name: Option<String> = None;
    let mut current_body = String::new();

    for line in output.lines() {
        if line.starts_with("___") && line.ends_with("___") {
            // "_____ test_name _____" header
            let name = line.trim_matches('_').trim().to_string();
            if let Some(prev) = current_name.take() {
                failures.push(Failure {
                    name: prev,
                    message: current_body.trim().to_string(),
                });
            }
            current_name = Some(name);
            current_body.clear();
            in_failures = true;
            continue;
        }
        if line.starts_with("=") && line.contains("short test summary") {
            // End of failure details
            if let Some(prev) = current_name.take() {
                failures.push(Failure {
                    name: prev,
                    message: current_body.trim().to_string(),
                });
            }
            in_failures = false;
            continue;
        }
        if in_failures && current_name.is_some() {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }
    if let Some(name) = current_name {
        failures.push(Failure {
            name,
            message: current_body.trim().to_string(),
        });
    }
    failures
}

// --- Vitest/Jest parser ---

fn parse_vitest(output: &str) -> Option<TestResult> {
    // vitest: "Tests  27 passed (27)" or "Tests  2 failed | 25 passed (27)"
    // jest: "Tests:       2 failed, 25 passed, 27 total"
    let summary_line = output
        .lines()
        .rev()
        .find(|l| l.contains("Tests") && (l.contains("passed") || l.contains("failed")))?;

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;

    // vitest format: "N failed | N passed (N)"
    // jest format: "N failed, N passed, N total"
    for segment in summary_line.split(&['|', ','][..]) {
        let trimmed = segment.trim();
        if let Some(n) = extract_number_before(trimmed, " passed") {
            passed = n;
        } else if let Some(n) = extract_number_before(trimmed, " failed") {
            failed = n;
        } else if let Some(n) = extract_number_before(trimmed, " skipped") {
            skipped = n;
        } else if let Some(n) = extract_number_before(trimmed, " todo") {
            skipped += n;
        }
    }

    if passed == 0 && failed == 0 {
        return None;
    }

    // Parse vitest/jest failures (look for "FAIL" or "●" prefixed lines)
    let failures = parse_vitest_failures(output);

    Some(TestResult {
        passed,
        failed,
        skipped,
        failures,
        raw_tail: None,
    })
}

fn parse_vitest_failures(output: &str) -> Vec<Failure> {
    let mut failures = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_body = String::new();

    for line in output.lines() {
        // vitest: " FAIL  src/lib/foo.test.ts > suite > test name"
        // or "  × test name" (vitest v2+)
        if (line.contains("FAIL") && line.contains(">"))
            || line.trim_start().starts_with("× ")
            || line.trim_start().starts_with("✕ ")
        {
            if let Some(prev) = current_name.take() {
                failures.push(Failure {
                    name: prev,
                    message: current_body.trim().to_string(),
                });
            }
            let name = line
                .rsplit('>')
                .next()
                .unwrap_or(line)
                .trim()
                .trim_start_matches("× ")
                .trim_start_matches("✕ ")
                .to_string();
            current_name = Some(name);
            current_body.clear();
            continue;
        }
        if current_name.is_some() {
            // Stop collecting on next test or summary
            if line.starts_with(" Test Files") || line.starts_with("Tests") {
                failures.push(Failure {
                    name: current_name.take().unwrap(),
                    message: current_body.trim().to_string(),
                });
                break;
            }
            current_body.push_str(line);
            current_body.push('\n');
        }
    }
    if let Some(name) = current_name {
        failures.push(Failure {
            name,
            message: current_body.trim().to_string(),
        });
    }
    failures
}

// --- Helpers ---

fn extract_number_before(s: &str, suffix: &str) -> Option<u32> {
    // Find suffix anywhere in s (not just at the end), to tolerate trailing
    // content like "in 2.31s" or "(27)".
    let idx = s.find(suffix)?;
    let before = s[..idx].trim();
    let num_str = before.split_whitespace().last()?;
    num_str.parse().ok()
}

fn truncate_output(output: &str, max_bytes: usize) -> &str {
    if output.len() <= max_bytes {
        return output;
    }
    // Take the tail (most recent output is most useful)
    let mut start = output.len() - max_bytes;
    // Advance to the next char boundary to avoid panicking on multibyte UTF-8.
    while !output.is_char_boundary(start) {
        start += 1;
    }
    // Find the next newline to avoid cutting mid-line
    match output[start..].find('\n') {
        Some(pos) => &output[start + pos + 1..],
        None => &output[start..],
    }
}

#[cfg(test)]
mod tests;
