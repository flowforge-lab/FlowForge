//! `ask_user` — pause the turn to ask the user a clarifying question and resume with
//! their answer (#44). Unlike every other tool it does not touch the workspace: it is
//! [`Tool::interactive`], so the agent loop routes it through `Approver::ask` instead
//! of [`Tool::run`], surfacing the question in the UI and feeding the typed answer back
//! as the tool result. `Safety::ReadOnly` — asking a question is not a trust decision,
//! so it never hits the approval gate.

use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

use crate::registry::{Safety, Tool, ToolOutcome};

pub struct AskUserTool;

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Ask the user a clarifying question and wait for their answer. Use this when the \
         request is ambiguous, you need a decision between options, or you are missing a \
         detail you cannot safely assume. Prefer asking over guessing. The user's reply is \
         returned as the result."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to put to the user. Be specific and concise."
                }
            },
            "required": ["question"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::ReadOnly
    }

    fn max_safety(&self) -> Safety {
        Safety::ReadOnly
    }

    fn interactive(&self) -> bool {
        true
    }

    /// Never reached in the normal flow — the agent loop intercepts interactive tools
    /// before dispatch. Implemented defensively so a host that bypasses that routing
    /// gets a clear error instead of a hang.
    async fn run(&self, _args: Value, _root: &Path) -> ToolOutcome {
        ToolOutcome::error("ask_user is interactive and must be handled by the host, not run")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertises_interactive_readonly_question_schema() {
        let t = AskUserTool;
        assert_eq!(t.name(), "ask_user");
        assert!(t.interactive());
        assert_eq!(t.safety(&Value::Null), Safety::ReadOnly);
        let params = t.parameters();
        assert_eq!(params["required"][0], "question");
        assert_eq!(params["properties"]["question"]["type"], "string");
    }
}
