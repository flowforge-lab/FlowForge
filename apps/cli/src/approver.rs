//! Terminal approval gate — the CLI counterpart to the desktop's `UiApprover`. A
//! write/dangerous tool call becomes a y/N prompt on the controlling TTY by
//! default. `--yes` and `--deny` provide explicit non-interactive policies; when
//! stdin is not a terminal (piped / CI) and no policy was provided, the call is
//! loudly denied rather than silently run.

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use ff_agent::{ApprovalOutcome, Approver, DenyReason};
use ff_core::{Mode, PermissionMatrix};
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
    /// When set, the approver runs autonomously (goal mode, #1256): the
    /// permission matrix is the sole gate. `Allow` proceeds; anything else is a
    /// clean deny, since there is no human on the TTY to prompt. This is how an
    /// `allow_propose_pr` goal reaches `propose_pr` (a per-tool `Allow` override)
    /// while every other Sensitive/Publish call is refused rather than silently
    /// auto-approved as the old `ApprovalMode::Yes` wiring did.
    matrix: Option<PermissionMatrix>,
    /// Latches `true` the first time a write/dangerous call is denied.
    denied: AtomicBool,
}

impl CliApprover {
    pub fn new(policy: ApprovalMode, agent_mode: Mode) -> Self {
        Self {
            policy,
            agent_mode,
            matrix: None,
            denied: AtomicBool::new(false),
        }
    }

    /// Build an autonomous approver for goal mode (#1256): the given permission
    /// matrix is the sole gate, with no interactive fallback. Callers pass a
    /// matrix carrying any per-tool overrides (e.g. `propose_pr -> Allow` when
    /// the goal is authorised to open a draft PR).
    pub fn autonomous(agent_mode: Mode, matrix: PermissionMatrix) -> Self {
        Self {
            policy: ApprovalMode::Deny,
            agent_mode,
            matrix: Some(matrix),
            denied: AtomicBool::new(false),
        }
    }

    /// Returns `true` if any write/dangerous tool call was denied since
    /// construction.
    ///
    /// Read-only calls bypass the approver entirely, so this only flips
    /// when a call that *needed* approval was refused — by `--deny`, by
    /// the piped-no-policy rule, or by the user answering `N` at a prompt.
    /// The `run` subcommand checks this after the turn to honor the
    /// exit-code contract: a turn in which a required approval was denied
    /// exits non-zero even if the model recovered with a text answer.
    pub fn was_denied(&self) -> bool {
        self.denied.load(Ordering::Relaxed)
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
                // Auto silently auto-approves Write/Sensitive but NOT Publish
                // (`git push`, `gh pr merge`): a remote mutation must prompt (or
                // be denied when piped), matching the desktop Auto/Publish=Ask
                // cell (#1051). Do NOT add `Safety::Publish` to this `matches!`.
                if agent_mode == Mode::Auto && matches!(safety, Safety::Write | Safety::Sensitive) {
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
    ) -> ApprovalOutcome {
        // Autonomous goal mode (#1256): the matrix is the whole gate, evaluated
        // through the shared canonical order (`pre_prompt_decision`) that the
        // desktop and Slack approvers use. `Allow` proceeds; `Deny` and `Ask`
        // both become a clean deny — there is no TTY to prompt, and an
        // indefinite block would strand the goal loop. The model reads the
        // refusal and routes to the report-only branch.
        if let Some(matrix) = &self.matrix {
            let resolved_arg = ff_core::resolve_tool_arg(name, args);
            let cell = matrix.effective_cell(name, self.agent_mode, safety);
            let scoped_effect =
                matrix.evaluate_rules(name, resolved_arg.as_deref(), self.agent_mode);
            let scoped_deny_rule_desc = matrix
                .matching_deny_rule(name, resolved_arg.as_deref())
                .map(|r| format!("{} ({})", r.tool, r.matcher.description()));
            let outcome = match ff_core::pre_prompt_decision(
                cell,
                false,
                scoped_effect,
                safety,
                self.agent_mode,
                scoped_deny_rule_desc,
            ) {
                ff_core::PrePromptDecision::Allow => ApprovalOutcome::Allowed,
                ff_core::PrePromptDecision::Deny(reason) => ApprovalOutcome::Denied(reason),
                ff_core::PrePromptDecision::Prompt => {
                    ApprovalOutcome::Denied(DenyReason::NoInteractiveTerminal)
                }
            };
            if !matches!(outcome, ApprovalOutcome::Allowed) {
                self.denied.store(true, Ordering::Relaxed);
            }
            return outcome;
        }

        let label = match safety {
            Safety::Write => "write",
            Safety::Sensitive => "sensitive",
            Safety::Dangerous => "DANGEROUS",
            Safety::Publish => "publish",
            Safety::ReadOnly => "read-only",
        };
        eprintln!("\n[approval] {name} ({label})");
        let pretty = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
        eprintln!("{pretty}");
        // #1252: show what a propose_pr push is about to carry, in parity with the
        // desktop gate. Best-effort against the CLI's workspace root.
        if name == ff_tools::PROPOSE_PR_TOOL_NAME {
            if let Some(s) =
                ff_tools::propose_pr_scope_summary(args, &crate::host::workspace_root()).await
            {
                // Suppress the "N files changed" clause for an all-new-file push
                // (finding 4): "0 files changed, 2 new files" reads as a bug.
                let mut parts = Vec::new();
                if s.files_changed > 0 || s.new_files == 0 {
                    parts.push(format!(
                        "{} file(s) changed, +{} -{}",
                        s.files_changed, s.insertions, s.deletions
                    ));
                }
                if s.new_files > 0 {
                    parts.push(format!("{} new file(s)", s.new_files));
                }
                eprintln!("[approval] about to push: {}", parts.join(", "));
                for f in &s.per_file {
                    eprintln!("[approval]   {} +{} -{}", f.path, f.insertions, f.deletions);
                }
            }
        }

        let outcome = match Self::decide(self.agent_mode, self.policy, Self::input_mode(), safety) {
            ApprovalDecision::Allow => {
                if self.policy == ApprovalMode::Yes {
                    eprintln!("[approval] auto-approved by --yes");
                } else {
                    eprintln!("[approval] auto-approved (auto mode)");
                }
                ApprovalOutcome::Allowed
            }
            ApprovalDecision::Deny => match self.policy {
                ApprovalMode::Deny => {
                    eprintln!("[approval] auto-denied by --deny");
                    ApprovalOutcome::Denied(DenyReason::NoInteractiveTerminal)
                }
                ApprovalMode::Prompt => {
                    eprintln!(
                        "[approval] no interactive terminal and no --yes/--deny flag; denying"
                    );
                    ApprovalOutcome::Denied(DenyReason::NoInteractiveTerminal)
                }
                ApprovalMode::Yes => {
                    unreachable!("--yes always allows")
                }
            },
            ApprovalDecision::Prompt => {
                eprint!("[approval] allow this call? [y/N] ");
                let _ = io::stderr().flush();
                let mut line = String::new();
                let approved = if io::stdin().read_line(&mut line).is_err() {
                    false
                } else {
                    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
                };
                if approved {
                    ApprovalOutcome::Allowed
                } else {
                    ApprovalOutcome::Denied(DenyReason::User)
                }
            }
        };

        if !matches!(outcome, ApprovalOutcome::Allowed) {
            self.denied.store(true, Ordering::Relaxed);
        }
        outcome
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
mod tests;
