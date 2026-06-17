//! Terminal approval gate — the CLI counterpart to the desktop's `UiApprover`. A
//! write/dangerous tool call becomes a y/N prompt on the controlling TTY by
//! default. `--yes` and `--deny` provide explicit non-interactive policies; when
//! stdin is not a terminal (piped / CI) and no policy was provided, the call is
//! loudly denied rather than silently run.

use std::io::{self, IsTerminal, Write};

use async_trait::async_trait;
use ff_agent::Approver;
use ff_tools::Safety;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalMode {
    Prompt,
    Yes,
    Deny,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputMode {
    Tty,
    Piped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApprovalDecision {
    Allow,
    Deny,
    Prompt,
}

pub struct CliApprover {
    mode: ApprovalMode,
}

impl CliApprover {
    pub fn new(mode: ApprovalMode) -> Self {
        Self { mode }
    }

    pub(crate) fn decide(mode: ApprovalMode, input: InputMode, safety: Safety) -> ApprovalDecision {
        if safety == Safety::ReadOnly {
            return ApprovalDecision::Allow;
        }

        match mode {
            ApprovalMode::Yes => ApprovalDecision::Allow,
            ApprovalMode::Deny => ApprovalDecision::Deny,
            ApprovalMode::Prompt => match input {
                InputMode::Tty => ApprovalDecision::Prompt,
                InputMode::Piped => ApprovalDecision::Deny,
            },
        }
    }

    fn input_mode() -> InputMode {
        if io::stdin().is_terminal() {
            InputMode::Tty
        } else {
            InputMode::Piped
        }
    }
}

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

        match Self::decide(self.mode, Self::input_mode(), safety) {
            ApprovalDecision::Allow => {
                eprintln!("[approval] auto-approved by --yes");
                true
            }
            ApprovalDecision::Deny => {
                match self.mode {
                    ApprovalMode::Deny => eprintln!("[approval] auto-denied by --deny"),
                    ApprovalMode::Prompt => {
                        eprintln!(
                            "[approval] no interactive terminal and no --yes/--deny flag; denying"
                        );
                    }
                    ApprovalMode::Yes => {}
                }
                false
            }
            ApprovalDecision::Prompt => {
                eprint!("[approval] allow this call? [y/N] ");
                let _ = io::stderr().flush();
                let mut line = String::new();
                if io::stdin().read_line(&mut line).is_err() {
                    return false;
                }
                matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
            }
        }
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

#[cfg(test)]
mod tests {
    use super::{ApprovalDecision, ApprovalMode, CliApprover, InputMode};
    use ff_tools::Safety;

    #[test]
    fn approval_decision_matrix_for_non_read_only_calls() {
        use ApprovalDecision::{Allow, Deny, Prompt};
        use ApprovalMode::{Deny as AutoDeny, Prompt as DefaultPrompt, Yes};
        use InputMode::{Piped, Tty};
        use Safety::{Dangerous, Write};

        let cases = [
            (Tty, Yes, Write, Allow),
            (Tty, Yes, Dangerous, Allow),
            (Piped, Yes, Write, Allow),
            (Piped, Yes, Dangerous, Allow),
            (Tty, AutoDeny, Write, Deny),
            (Tty, AutoDeny, Dangerous, Deny),
            (Piped, AutoDeny, Write, Deny),
            (Piped, AutoDeny, Dangerous, Deny),
            (Tty, DefaultPrompt, Write, Prompt),
            (Tty, DefaultPrompt, Dangerous, Prompt),
            (Piped, DefaultPrompt, Write, Deny),
            (Piped, DefaultPrompt, Dangerous, Deny),
        ];

        for (input, mode, safety, want) in cases {
            assert_eq!(
                CliApprover::decide(mode, input, safety),
                want,
                "input={input:?} mode={mode:?} safety={safety:?}"
            );
        }
    }

    #[test]
    fn read_only_is_allowed_even_if_the_approver_is_called() {
        for input in [InputMode::Tty, InputMode::Piped] {
            for mode in [ApprovalMode::Prompt, ApprovalMode::Yes, ApprovalMode::Deny] {
                assert_eq!(
                    CliApprover::decide(mode, input, Safety::ReadOnly),
                    ApprovalDecision::Allow
                );
            }
        }
    }

    #[test]
    fn dangerous_calls_are_never_auto_allowed_without_an_explicit_flag() {
        assert_ne!(
            CliApprover::decide(ApprovalMode::Prompt, InputMode::Tty, Safety::Dangerous),
            ApprovalDecision::Allow
        );
        assert_eq!(
            CliApprover::decide(ApprovalMode::Prompt, InputMode::Piped, Safety::Dangerous),
            ApprovalDecision::Deny
        );
    }
}
