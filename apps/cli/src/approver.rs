//! Terminal approval gate — the CLI counterpart to the desktop's `UiApprover`. A
//! write/dangerous tool call becomes a y/N prompt on the controlling TTY. When
//! stdin is not a terminal (piped / CI), the call is denied rather than silently
//! run; a non-interactive `--yes` flag is tracked under the CLI epic.

use std::io::{self, IsTerminal, Write};

use async_trait::async_trait;
use ff_agent::Approver;
use ff_tools::Safety;

pub struct CliApprover;

#[async_trait]
impl Approver for CliApprover {
    async fn approve(
        &self,
        _message_id: &str,
        _call_id: &str,
        name: &str,
        safety: Safety,
        args: &serde_json::Value,
    ) -> bool {
        let label = match safety {
            Safety::Write => "write",
            Safety::Dangerous => "DANGEROUS",
            Safety::ReadOnly => "read-only",
        };
        eprintln!("\n[approval] {name} ({label})");
        let pretty = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
        eprintln!("{pretty}");

        if !io::stdin().is_terminal() {
            eprintln!("[approval] no interactive terminal; denying");
            return false;
        }
        eprint!("[approval] allow this call? [y/N] ");
        let _ = io::stderr().flush();
        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }

    async fn ask(
        &self,
        _message_id: &str,
        _call_id: &str,
        args: &serde_json::Value,
    ) -> Option<String> {
        let question = args
            .get("question")
            .and_then(|q| q.as_str())
            .unwrap_or("(the agent is asking for input)");
        eprintln!("\n[question] {question}");
        if !io::stdin().is_terminal() {
            return None;
        }
        eprint!("> ");
        let _ = io::stderr().flush();
        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            return None;
        }
        let answer = line.trim().to_string();
        (!answer.is_empty()).then_some(answer)
    }
}
