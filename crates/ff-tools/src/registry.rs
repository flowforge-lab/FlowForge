//! The tool abstraction and the registry the agent loop dispatches through.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

/// How much trust a given invocation needs. The agent loop auto-runs
/// [`Safety::ReadOnly`] and defers [`Safety::Write`] / [`Safety::Sensitive`] /
/// [`Safety::Dangerous`] to an approval policy supplied by the host (UI confirm
/// in the desktop shell).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Safety {
    ReadOnly,
    Write,
    /// Externally-visible actions (network egress, sub-agent spawn) that warrant
    /// a distinct trust tier between [`Write`](Self::Write) and
    /// [`Dangerous`](Self::Dangerous) (#682). Data modeling only for now (#698):
    /// treated identically to [`Write`](Self::Write) everywhere — auto-approved
    /// in Auto, prompted in Act/Plan, hidden in Plan-mode advertisement — until a
    /// follow-up PR differentiates its handling.
    Sensitive,
    Dangerous,
}

/// The result of running a tool. `content` is fed back to the model verbatim as the
/// tool message; `success` lets the host render pass/fail without parsing `content`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub success: bool,
    pub content: String,
}

impl ToolOutcome {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            success: true,
            content: content.into(),
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            success: false,
            content: content.into(),
        }
    }
}

/// Sentinel session id meaning "no owning session" — the call has no session
/// affinity. Used by [`Tool::run`] and [`ToolRegistry::run`] when the caller
/// has no session to thread (external/test entry points). Tools that bucket by
/// session (e.g. [`crate::process::ProcessManager`]) treat all such calls as
/// sharing one anonymous bucket, which is fine for one-off calls but would
/// collide if a *real* session id were ever empty. Real session ids are UUIDs
/// assigned by the host and are never empty.
pub const NO_SESSION: &str = "";

/// A callable the model can invoke. Implementors describe themselves as an
/// OpenAI-style function schema and execute against a jailed workspace `root`.
///
/// `run` must never panic or propagate transport errors to the caller — failures
/// are returned as [`ToolOutcome::error`] so the model can read and react to them.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema for the arguments object (the `parameters` field of the function).
    fn parameters(&self) -> Value;
    /// Classify a concrete invocation. Defaults to [`Safety::Write`] — implementors
    /// override when they can prove read-only or flag a destructive call.
    fn safety(&self, _args: &Value) -> Safety {
        Safety::Write
    }
    /// Worst-case safety this tool can ever reach, independent of arguments. Used to
    /// decide whether the tool is advertised at all in capability-restricted modes
    /// (Plan, RFC 0011): only tools whose ceiling is [`Safety::ReadOnly`] are shown.
    /// Defaults to the same conservative [`Safety::Write`] as [`Tool::safety`];
    /// tools with dynamic per-call safety (e.g. `bash`) override to their true ceiling.
    fn max_safety(&self) -> Safety {
        Safety::Write
    }
    /// Interactive tools don't execute against the workspace — they pause the turn to
    /// ask the user something and resume with the answer (e.g. `ask_user`, #44). The
    /// agent loop routes them through [`Approver::ask`] instead of [`Tool::run`].
    ///
    /// Invariant: interactive tools MUST be side-effect-free ([`Safety::ReadOnly`]).
    /// The agent loop resolves them *before* the approval gate, so an interactive
    /// tool that performed `Write` work would bypass approval entirely.
    fn interactive(&self) -> bool {
        false
    }
    /// A stable identity for a *content read*, used by the agent's per-turn semantic
    /// read-dedupe (#458 RC5). A read tool (e.g. `view`) returns a key — typically
    /// the path it reads — so the loop can detect a re-read of the same target this
    /// turn and, when the content is unchanged, return a sentinel instead of
    /// re-injecting the bytes. Non-read tools keep the `None` default and are never
    /// deduped. Pure: it must not perform I/O — keying is by reference, not content.
    fn dedupe_key(&self, _args: &Value) -> Option<String> {
        None
    }
    async fn run(&self, args: Value, root: &Path) -> ToolOutcome;

    /// Session-aware dispatch point. Tools that need per-session affinity
    /// (e.g. `process_manager`, which scopes its live-process table to the
    /// owning session and auto-reaps on close) override this. The default
    /// delegates to [`run`](Self::run), ignoring `session_id`. Callers without
    /// a session pass [`NO_SESSION`] as the sentinel; a real session id is a
    /// non-empty UUID threaded from the host.
    async fn run_with_session(&self, args: Value, root: &Path, session_id: &str) -> ToolOutcome {
        let _ = session_id;
        self.run(args, root).await
    }

    /// Streaming dispatch point (#680). Tools that buffer output until the process
    /// exits (e.g. `bash`) override this to push chunks to `sink` *as they are
    /// produced*, in addition to the full capture they still return in the final
    /// [`ToolOutcome`]. The live stream is additive: the returned result is
    /// byte-for-byte identical to [`run_with_session`](Self::run_with_session). The
    /// default ignores `sink` and delegates, so a non-streaming tool needs no change
    /// and a caller can always pass a sink safely.
    async fn run_streaming(
        &self,
        args: Value,
        root: &Path,
        session_id: &str,
        sink: Option<crate::OutputSink>,
    ) -> ToolOutcome {
        let _ = sink;
        self.run_with_session(args, root, session_id).await
    }
}

/// Name -> tool. Built with the M2 defaults (bash, view, edit) and queried by the
/// agent loop to (a) advertise schemas to the model and (b) dispatch calls.
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The built-in M2 toolset.
    pub fn with_defaults() -> Self {
        let mut r = Self::new();
        r.register(Box::new(crate::bash::BashTool));
        r.register(Box::new(crate::python::PythonTool));
        r.register(Box::new(crate::view::ViewTool));
        r.register(Box::new(crate::edit::EditTool));
        r.register(Box::new(crate::write::WriteTool));
        r.register(Box::new(crate::apply_patch::ApplyPatchTool));
        r.register(Box::new(crate::grep::GrepTool));
        r.register(Box::new(crate::glob::GlobTool));
        r.register(Box::new(crate::tree::TreeTool));
        r.register(Box::new(crate::todo::TodoTool));
        r.register(Box::new(crate::web_fetch::WebFetchTool::new()));
        r.register(Box::new(crate::ask_user::AskUserTool));
        r.register(Box::new(crate::diagnostics::DiagnosticsTool));
        r.register(Box::new(crate::agent_tool::AgentTool));
        r
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|b| b.as_ref())
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// All tools as OpenAI `tools` request entries.
    pub fn openai_tools(&self) -> Vec<Value> {
        self.openai_tools_for(None, true)
    }

    /// OpenAI `tools` entries, optionally restricted to a sub-agent's allowlist and
    /// with the `agent` delegation tool suppressed once the depth cap is reached
    /// (so a sub-agent at max depth is never even offered a spawn it cannot make).
    pub fn openai_tools_for(
        &self,
        allowed: Option<&HashSet<String>>,
        allow_subagent: bool,
    ) -> Vec<Value> {
        self.tools
            .values()
            .filter(|t| allowed.is_none_or(|set| set.contains(t.name())))
            .filter(|t| allow_subagent || !is_subagent(t.name()))
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters(),
                    }
                })
            })
            .collect()
    }

    /// Names of every tool whose worst-case safety is [`Safety::ReadOnly`]. This is
    /// the set advertised in Plan mode (RFC 0011): mutating tools are absent from the
    /// schema entirely, so the model cannot call them. Tools with an unknown/elevated
    /// ceiling are excluded (fail safe).
    pub fn readonly_tool_names(&self) -> HashSet<String> {
        self.tools
            .values()
            .filter(|t| t.max_safety() == Safety::ReadOnly)
            .map(|t| t.name().to_string())
            .collect()
    }

    /// Dispatch a call by name with an anonymous session ([`NO_SESSION`]). No
    /// session affinity — equivalent to
    /// [`run_with_session`](Self::run_with_session) with [`NO_SESSION`].
    pub async fn run(&self, name: &str, args: Value, root: &Path) -> ToolOutcome {
        self.run_with_session(name, args, root, NO_SESSION).await
    }

    /// Dispatch a call by name, threading the owning `session_id` to tools
    /// that implement [`Tool::run_with_session`]. Unknown tools and malformed
    /// arguments return an error outcome rather than failing the turn.
    pub async fn run_with_session(
        &self,
        name: &str,
        args: Value,
        root: &Path,
        session_id: &str,
    ) -> ToolOutcome {
        match self.get(name) {
            Some(tool) => tool.run_with_session(args, root, session_id).await,
            // Name the registered tools so a model that hallucinated a tool name
            // (e.g. `codegraph_explore`, #646) can self-correct in one turn instead
            // of guessing again. The list is sorted for a stable, diff-friendly hint.
            None => ToolOutcome::error(format!(
                "unknown tool: {name}. Available tools: {}",
                self.sorted_names().join(", ")
            )),
        }
    }

    /// Dispatch a call by name with an optional live-output `sink` (#680), threading
    /// the owning `session_id`. Streaming tools (e.g. `bash`) push chunks to `sink`
    /// as they are produced; non-streaming tools ignore it. The returned outcome is
    /// identical to [`run_with_session`](Self::run_with_session).
    pub async fn run_streaming(
        &self,
        name: &str,
        args: Value,
        root: &Path,
        session_id: &str,
        sink: Option<crate::OutputSink>,
    ) -> ToolOutcome {
        match self.get(name) {
            Some(tool) => tool.run_streaming(args, root, session_id, sink).await,
            None => ToolOutcome::error(format!(
                "unknown tool: {name}. Available tools: {}",
                self.sorted_names().join(", ")
            )),
        }
    }

    /// The registered tool names, sorted. Used to make an unknown-tool error
    /// actionable (#646).
    fn sorted_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.tools.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// Safety of a concrete call (defaults to [`Safety::Dangerous`] for unknown
    /// tools so an unrecognized name can never be auto-approved).
    pub fn safety(&self, name: &str, args: &Value) -> Safety {
        match self.get(name) {
            Some(tool) => tool.safety(args),
            None => Safety::Dangerous,
        }
    }

    /// Whether a tool pauses the turn for user input rather than executing (#44).
    /// Unknown tools are never interactive.
    pub fn is_interactive(&self, name: &str) -> bool {
        self.get(name).is_some_and(Tool::interactive)
    }

    /// The per-turn read-dedupe key for a call (#458 RC5), or `None` for an unknown
    /// tool or one that isn't a content read.
    pub fn dedupe_key(&self, name: &str, args: &Value) -> Option<String> {
        self.get(name).and_then(|tool| tool.dedupe_key(args))
    }
}

/// Whether `name` is the `agent` delegation tool the loop intercepts to spawn a
/// scoped sub-agent (#234) rather than dispatching through [`Tool::run`].
pub fn is_subagent(name: &str) -> bool {
    name == crate::agent_tool::AGENT_TOOL_NAME
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unknown_tool_is_error_not_panic() {
        let reg = ToolRegistry::with_defaults();
        let out = reg.run("nope", serde_json::json!({}), Path::new(".")).await;
        assert!(!out.success);
        assert!(out.content.contains("unknown tool"));
    }

    #[tokio::test]
    async fn unknown_tool_error_lists_available_tools() {
        let reg = ToolRegistry::with_defaults();
        let out = reg
            .run("codegraph_explore", serde_json::json!({}), Path::new("."))
            .await;
        assert!(!out.success);
        assert!(out.content.contains("unknown tool: codegraph_explore"));
        assert!(
            out.content.contains("Available tools:"),
            "error should name the registered tools so the model can self-correct"
        );
        // Every registered tool is named, in sorted order.
        let mut expected: Vec<&str> = reg.tools.keys().map(String::as_str).collect();
        expected.sort_unstable();
        assert!(!expected.is_empty(), "default registry has tools");
        for name in &expected {
            assert!(
                out.content.contains(name),
                "available-tools hint should include {name}"
            );
        }
    }

    #[test]
    fn advertises_default_schemas() {
        let reg = ToolRegistry::with_defaults();
        let tools = reg.openai_tools();
        assert_eq!(tools.len(), 14);
        let names: Vec<_> = tools
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        for expected in [
            "bash",
            "python",
            "view",
            "edit",
            "write",
            "apply_patch",
            "grep",
            "glob",
            "tree",
            "todo",
            "web_fetch",
            "ask_user",
            "agent",
        ] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
    }

    #[test]
    fn openai_tools_for_honors_allowlist_and_depth() {
        let reg = ToolRegistry::with_defaults();

        let allowed: HashSet<String> = ["view", "grep"].iter().map(|s| s.to_string()).collect();
        let restricted = reg.openai_tools_for(Some(&allowed), true);
        let names: Vec<_> = restricted
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"view") && names.contains(&"grep"));

        // At the depth cap the delegation tool is not advertised at all.
        let no_subagent = reg.openai_tools_for(None, false);
        let names: Vec<_> = no_subagent
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(!names.contains(&"agent"));
        assert_eq!(no_subagent.len(), 13);
    }

    #[test]
    fn search_and_plan_tools_are_read_only() {
        let reg = ToolRegistry::with_defaults();
        for name in ["grep", "glob", "tree", "todo"] {
            assert_eq!(
                reg.safety(name, &serde_json::json!({})),
                Safety::ReadOnly,
                "{name} should be read-only"
            );
        }
    }

    #[test]
    fn readonly_tool_names_excludes_mutating_and_dynamic_tools() {
        let reg = ToolRegistry::with_defaults();
        let ro = reg.readonly_tool_names();

        // Every ReadOnly-ceiling default tool is present.
        for name in ["view", "grep", "glob", "tree", "todo", "ask_user", "diagnostics"] {
            assert!(ro.contains(name), "{name} should be a read-only tool");
        }
        // Mutating tools and dynamically-classified tools (bash) are absent: in Plan
        // mode they must not even be advertised to the model. web_fetch is Write
        // (network egress), so it is excluded too.
        for name in [
            "bash",
            "python",
            "edit",
            "write",
            "apply_patch",
            "web_fetch",
            "agent",
        ] {
            assert!(!ro.contains(name), "{name} must not be a read-only tool");
        }
    }

    #[test]
    fn unknown_tool_safety_is_dangerous() {
        let reg = ToolRegistry::with_defaults();
        assert_eq!(
            reg.safety("nope", &serde_json::json!({})),
            Safety::Dangerous
        );
    }

    #[test]
    fn dedupe_key_only_for_read_tools() {
        // #458 RC5: `view` exposes a read identity; non-read tools and unknown names
        // return None, so the per-turn dedupe is scoped to file reads.
        let reg = ToolRegistry::with_defaults();
        assert_eq!(
            reg.dedupe_key("view", &serde_json::json!({"path": "a.rs"})),
            Some("a.rs".to_string())
        );
        assert_eq!(
            reg.dedupe_key("bash", &serde_json::json!({"command": "ls"})),
            None
        );
        assert_eq!(reg.dedupe_key("nope", &serde_json::json!({})), None);
    }
}
