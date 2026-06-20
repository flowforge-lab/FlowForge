//! Terminal approval gate — the CLI counterpart to the desktop's `UiApprover`. A
//! write/dangerous tool call becomes a y/N prompt on the controlling TTY by
//! default. `--yes` and `--deny` provide explicit non-interactive policies; when
//! stdin is not a terminal (piped / CI) and no policy was provided, the call is
//! loudly denied rather than silently run.

use std::io::{self, IsTerminal, Write};

use async_trait::async_trait;
use ff_agent::Approver;
use ff_core::Mode;
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
    policy: ApprovalMode,
    agent_mode: Mode,
}

impl CliApprover {
    pub fn new(policy: ApprovalMode, agent_mode: Mode) -> Self {
        Self { policy, agent_mode }
    }

    /// Resolve an approval decision. The explicit `--yes/--deny` policy always wins:
    /// `--deny` is absolute (no mode carve-out can defeat it) and `--yes` allows
    /// everything. Only when the policy would otherwise *prompt* does `agent_mode`
    /// apply its single carve-out: in `Auto`, a `Write` call auto-approves. A
    /// `Dangerous` call never auto-approves from the mode -- it still prompts (TTY)
    /// or is denied (piped), preserving the invariant that dangerous work needs a
    /// deliberate yes.
    pub(crate) fn decide(
        agent_mode: Mode,
        policy: ApprovalMode,
        input: InputMode,
        safety: Safety,
    ) -> ApprovalDecision {
        if safety == Safety::ReadOnly {
            return ApprovalDecision::Allow;
        }

        match policy {
            ApprovalMode::Yes => ApprovalDecision::Allow,
            ApprovalMode::Deny => ApprovalDecision::Deny,
            ApprovalMode::Prompt => {
                if agent_mode == Mode::Auto && safety == Safety::Write {
                    ApprovalDecision::Allow
                } else {
                    match input {
                        InputMode::Tty => ApprovalDecision::Prompt,
                        InputMode::Piped => ApprovalDecision::Deny,
                    }
                }
            }
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

        match Self::decide(self.agent_mode, self.policy, Self::input_mode(), safety) {
            ApprovalDecision::Allow => {
                if self.policy == ApprovalMode::Yes {
                    eprintln!("[approval] auto-approved by --yes");
                } else {
                    eprintln!("[approval] auto-approved (auto mode)");
                }
                true
            }
            ApprovalDecision::Deny => {
                match self.policy {
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
    use ff_core::Mode;
    use ff_tools::Safety;

    // The legacy `--yes/--deny`/prompt matrix is mode-independent for these cases,
    // so we exercise it under `Act` (no carve-out). `Auto`'s Write carve-out is
    // covered separately below.
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

        for (input, policy, safety, want) in cases {
            assert_eq!(
                CliApprover::decide(Mode::Act, policy, input, safety),
                want,
                "input={input:?} policy={policy:?} safety={safety:?}"
            );
        }
    }

    // Auto mode auto-approves Write even with the default prompt policy, but a
    // Dangerous call (e.g. `python`, `rm`) still falls through to that policy --
    // so it prompts on a TTY and is denied when piped.
    #[test]
    fn auto_mode_auto_approves_write_but_still_gates_dangerous() {
        assert_eq!(
            CliApprover::decide(
                Mode::Auto,
                ApprovalMode::Prompt,
                InputMode::Tty,
                Safety::Write
            ),
            ApprovalDecision::Allow
        );
        assert_eq!(
            CliApprover::decide(
                Mode::Auto,
                ApprovalMode::Prompt,
                InputMode::Piped,
                Safety::Write
            ),
            ApprovalDecision::Allow
        );
        assert_eq!(
            CliApprover::decide(
                Mode::Auto,
                ApprovalMode::Prompt,
                InputMode::Tty,
                Safety::Dangerous
            ),
            ApprovalDecision::Prompt
        );
        assert_eq!(
            CliApprover::decide(
                Mode::Auto,
                ApprovalMode::Prompt,
                InputMode::Piped,
                Safety::Dangerous
            ),
            ApprovalDecision::Deny
        );
    }

    // The explicit --yes/--deny policy always wins over Auto's Write carve-out:
    // --deny stays absolute (no silent write), --yes still allows. Regression guard
    // for the ordering bug where Auto+Write short-circuited before the policy match.
    #[test]
    fn explicit_policy_wins_over_auto_write_carve_out() {
        use InputMode::{Piped, Tty};
        // --deny: Write is denied even in Auto, on both TTY and piped input.
        assert_eq!(
            CliApprover::decide(Mode::Auto, ApprovalMode::Deny, Tty, Safety::Write),
            ApprovalDecision::Deny
        );
        assert_eq!(
            CliApprover::decide(Mode::Auto, ApprovalMode::Deny, Piped, Safety::Write),
            ApprovalDecision::Deny
        );
        // --yes: Write is allowed (as before), Auto or not.
        assert_eq!(
            CliApprover::decide(Mode::Auto, ApprovalMode::Yes, Piped, Safety::Write),
            ApprovalDecision::Allow
        );
    }

    // In Act mode a Write call is gated like any other (prompt on TTY), confirming
    // the Auto carve-out does not leak into the other modes.
    #[test]
    fn act_mode_does_not_auto_approve_write() {
        assert_eq!(
            CliApprover::decide(
                Mode::Act,
                ApprovalMode::Prompt,
                InputMode::Tty,
                Safety::Write
            ),
            ApprovalDecision::Prompt
        );
    }

    #[test]
    fn read_only_is_allowed_even_if_the_approver_is_called() {
        for input in [InputMode::Tty, InputMode::Piped] {
            for policy in [ApprovalMode::Prompt, ApprovalMode::Yes, ApprovalMode::Deny] {
                for agent_mode in [Mode::Plan, Mode::Act, Mode::Auto] {
                    assert_eq!(
                        CliApprover::decide(agent_mode, policy, input, Safety::ReadOnly),
                        ApprovalDecision::Allow
                    );
                }
            }
        }
    }

    #[test]
    fn dangerous_calls_are_never_auto_allowed_without_an_explicit_flag() {
        for agent_mode in [Mode::Plan, Mode::Act, Mode::Auto] {
            assert_ne!(
                CliApprover::decide(
                    agent_mode,
                    ApprovalMode::Prompt,
                    InputMode::Tty,
                    Safety::Dangerous
                ),
                ApprovalDecision::Allow
            );
            assert_eq!(
                CliApprover::decide(
                    agent_mode,
                    ApprovalMode::Prompt,
                    InputMode::Piped,
                    Safety::Dangerous
                ),
                ApprovalDecision::Deny
            );
        }
    }
}
