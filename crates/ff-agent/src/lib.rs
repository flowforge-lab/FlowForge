//! The agent turn loop.
//!
//! A turn is now multi-step: build history (advertising the tool schemas) -> stream
//! from the provider -> if the assistant only produced text, finish; if it requested
//! tool calls, execute each (subject to an approval policy), append the results, and
//! loop. The loop is capped by [`ToolContext::max_iterations`] so a misbehaving model
//! cannot spin forever.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ff_core::{Message, Role};
use ff_llm::{ChatMessage, ChatRequest, FunctionCall, Provider, ToolCall as LlmToolCall};
use ff_session::SessionStore;
use ff_tools::{Safety, ToolRegistry};
use futures_util::StreamExt;
use serde::Serialize;

mod compaction;
mod system_prompt;
pub use compaction::{
    flush_due, CompactionContext, CompactionOutcome, CompactionStrategy, ContextPressure,
    ContextPressureEstimator, MemoryFlush, ProxyTokenEstimator, DEFAULT_CONTEXT_BUDGET_TOKENS,
    DEFAULT_FLUSH_AT_FRACTION,
};
pub use system_prompt::{build_flush_prompt, build_system_prompt, TimeOfDay, UserContext};

/// Events the agent emits during a turn. The host (Tauri shell or a test) decides
/// how to surface them — over IPC, to a channel, or into assertions.
#[derive(Debug, Clone, Serialize)]
pub enum AgentEvent {
    Token {
        message_id: String,
        delta: String,
    },
    Reasoning {
        message_id: String,
        delta: String,
    },
    ToolCallStarted {
        message_id: String,
        call_id: String,
        name: String,
        args: serde_json::Value,
    },
    ToolCallFinished {
        message_id: String,
        call_id: String,
        success: bool,
        result: String,
    },
    Done {
        message_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        final_message: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        turns: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        token_count: Option<u32>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Llm(#[from] ff_llm::LlmError),
}

/// Decides whether a non-read-only tool call may run. The host supplies this; the
/// desktop shell routes it to a UI confirmation (an async round-trip), so the call
/// is `async`. Read-only calls bypass it entirely. `message_id` + `call_id` let the
/// host correlate the request with the exact tool step it is rendering.
#[async_trait::async_trait]
pub trait Approver: Send + Sync {
    async fn approve(
        &self,
        message_id: &str,
        call_id: &str,
        name: &str,
        safety: Safety,
        args: &serde_json::Value,
    ) -> bool;

    /// Pause the turn and put a question to the user (the `ask_user` tool, #44),
    /// resuming with their answer. `args` carries the tool call's arguments (the
    /// `question` field); `message_id`/`call_id` correlate the request with the tool
    /// step the host is rendering. Returns the answer, or `None` if it was dismissed
    /// or cancelled — the loop turns `None` into a tool result, never a hang.
    ///
    /// Defaults to `None`: a host with no interactive surface simply dismisses the
    /// question rather than blocking the turn.
    async fn ask(
        &self,
        _message_id: &str,
        _call_id: &str,
        _args: &serde_json::Value,
    ) -> Option<String> {
        None
    }
}

/// Default cap on sub-agent delegation depth (#234): a top-level agent may spawn
/// children, but those children may not spawn further sub-agents. Prevents an
/// unbounded sub-agent tree.
pub const DEFAULT_MAX_DELEGATION_DEPTH: usize = 1;

/// Everything the loop needs to dispatch tools.
pub struct ToolContext<'a> {
    pub registry: &'a ToolRegistry,
    /// Per-session workspace root. File tools are jailed to it; `bash` runs in it.
    pub root: &'a Path,
    pub approve: &'a dyn Approver,
    pub max_iterations: usize,
    /// Current delegation depth: 0 at the top level, +1 per nested sub-agent (#234).
    pub depth: usize,
    /// Depth at which sub-agent spawning is refused.
    pub max_depth: usize,
    /// When `Some`, the only tool names this (sub-)agent may call or be advertised.
    /// `None` = the full registry. Used to scope a delegated subtask.
    pub allowed: Option<std::collections::HashSet<String>>,
}

impl<'a> ToolContext<'a> {
    /// A top-level context: full toolset, no delegation parent, default depth cap.
    pub fn new(
        registry: &'a ToolRegistry,
        root: &'a Path,
        approve: &'a dyn Approver,
        max_iterations: usize,
    ) -> Self {
        Self {
            registry,
            root,
            approve,
            max_iterations,
            depth: 0,
            max_depth: DEFAULT_MAX_DELEGATION_DEPTH,
            allowed: None,
        }
    }
}

/// Cooperative cancellation flag, shared between a running turn and `cancel`.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "tool",
    }
}

pub(crate) fn to_chat(messages: &[Message]) -> Vec<ChatMessage> {
    messages
        .iter()
        .map(|m| {
            let tool_calls = m.tool_calls.as_ref().map(|calls| {
                calls
                    .iter()
                    .map(|tc| LlmToolCall {
                        id: tc.id.clone(),
                        kind: "function".to_string(),
                        function: FunctionCall {
                            name: tc.name.clone(),
                            arguments: tc.arguments.clone(),
                        },
                    })
                    .collect()
            });
            // An assistant message that only carries tool calls sends `content: null`.
            let content = if m.content.is_empty() && tool_calls.is_some() {
                None
            } else {
                Some(m.content.clone())
            };
            ChatMessage {
                role: role_str(m.role).to_string(),
                content,
                tool_calls,
                tool_call_id: m.tool_call_id.clone(),
                name: None,
            }
        })
        .collect()
}

/// Accumulates streamed tool-call fragments keyed by `index`.
#[derive(Default)]
struct CallBuf {
    id: String,
    name: String,
    arguments: String,
}

/// Runs one assistant turn for `session_id`, executing any tool calls the model
/// requests until it produces a plain text answer (or the iteration cap is hit).
/// `on_event` is called synchronously as the turn progresses. The final assistant
/// message is persisted and returned.
///
/// When `enable_reasoning` is true, provider reasoning streams are requested and
/// emitted as [`AgentEvent::Reasoning`] (not persisted in message content).
#[allow(clippy::too_many_arguments)]
pub async fn run_turn(
    provider: &dyn Provider,
    store: &SessionStore,
    tools: &ToolContext<'_>,
    session_id: &str,
    model: &str,
    // Optional system prompt prepended to every request this turn (skills + persona
    // + ambient context). Built by the host via `build_system_prompt`.
    system_prompt: Option<&str>,
    enable_reasoning: bool,
    cancel: CancelToken,
    mut on_event: impl FnMut(AgentEvent),
) -> Result<Message, AgentError> {
    let allow_subagent = tools.depth < tools.max_depth;
    let tool_schemas = tools
        .registry
        .openai_tools_for(tools.allowed.as_ref(), allow_subagent);
    let mut last: Option<Message> = None;

    let mut turn_count: u32 = 0;
    for _ in 0..tools.max_iterations.max(1) {
        turn_count += 1;
        if cancel.is_cancelled() {
            break;
        }

        let history = store.get_messages(session_id);
        let mut messages = Vec::new();
        if let Some(system) = system_prompt {
            // Transient: the system prompt is injected into the request only, never
            // persisted to the store, so message history stays user/assistant/tool.
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: Some(system.to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
        messages.extend(to_chat(&history));
        let req = ChatRequest {
            model: model.to_string(),
            messages,
            tools: tool_schemas.clone(),
            thinking: enable_reasoning,
        };

        // Reserve the assistant message id up front so the frontend can route tokens.
        let message_id = store
            .add_message(session_id, Role::Assistant, String::new())
            .id;

        let mut stream = match provider.chat_stream(req).await {
            Ok(s) => s,
            Err(e) => {
                on_event(AgentEvent::Error {
                    message: e.to_string(),
                });
                return Err(e.into());
            }
        };

        let mut acc = String::new();
        let mut calls: BTreeMap<u32, CallBuf> = BTreeMap::new();
        while let Some(item) = stream.next().await {
            if cancel.is_cancelled() {
                break;
            }
            match item {
                Ok(chunk) => {
                    if enable_reasoning && !chunk.reasoning_delta.is_empty() {
                        on_event(AgentEvent::Reasoning {
                            message_id: message_id.clone(),
                            delta: chunk.reasoning_delta,
                        });
                    }
                    if !chunk.delta.is_empty() {
                        acc.push_str(&chunk.delta);
                        on_event(AgentEvent::Token {
                            message_id: message_id.clone(),
                            delta: chunk.delta,
                        });
                    }
                    for frag in chunk.tool_calls {
                        let buf = calls.entry(frag.index).or_default();
                        if let Some(id) = frag.id {
                            buf.id = id;
                        }
                        if let Some(name) = frag.name {
                            buf.name = name;
                        }
                        buf.arguments.push_str(&frag.arguments);
                    }
                    if chunk.done {
                        break;
                    }
                }
                Err(e) => {
                    on_event(AgentEvent::Error {
                        message: e.to_string(),
                    });
                    return Err(e.into());
                }
            }
        }

        let final_text = acc.clone();
        let finalized = store.set_message_content(&message_id, session_id, acc);

        // No tool calls -> this is the final text answer.
        if calls.is_empty() {
            on_event(AgentEvent::Done {
                message_id: message_id.clone(),
                final_message: Some(final_text),
                turns: Some(turn_count),
                token_count: None,
            });
            return Ok(finalized);
        }

        // Persist the tool calls on the assistant message, then execute each.
        let core_calls: Vec<ff_core::ToolCall> = calls
            .values()
            .map(|c| ff_core::ToolCall {
                id: c.id.clone(),
                name: c.name.clone(),
                arguments: c.arguments.clone(),
            })
            .collect();
        store.attach_tool_calls(&message_id, session_id, core_calls);

        let mut executed: Vec<String> = Vec::new();
        for call in calls.values() {
            if cancel.is_cancelled() {
                break;
            }
            let args: serde_json::Value =
                serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::Null);

            on_event(AgentEvent::ToolCallStarted {
                message_id: message_id.clone(),
                call_id: call.id.clone(),
                name: call.name.clone(),
                args: args.clone(),
            });

            // Interactive tools (`ask_user`, #44) don't execute against the workspace:
            // pause the turn, put the question to the host, and use the answer as the
            // tool result. A dismissed/cancelled question yields a result too, so the
            // assistant `tool_calls` message always has a matching reply (no malformed
            // history). Asking is read-only, so it never reaches the approval gate.
            let permitted = tools
                .allowed
                .as_ref()
                .is_none_or(|set| set.contains(&call.name));
            let outcome = if tools.registry.is_interactive(&call.name) {
                match tools.approve.ask(&message_id, &call.id, &args).await {
                    Some(answer) => ff_tools::ToolOutcome::ok(answer),
                    None => ff_tools::ToolOutcome::error("[no answer: question dismissed]"),
                }
            } else if !permitted {
                ff_tools::ToolOutcome::error(format!(
                    "tool `{}` is not permitted for this sub-agent",
                    call.name
                ))
            } else if ff_tools::is_subagent(&call.name) {
                // Delegation (#234): drive a child turn in a fresh ephemeral session and
                // return only its summary. The child reuses the same provider/approver,
                // so its tool calls hit the identical approval gate (no escalation).
                run_subagent(
                    provider,
                    store,
                    tools,
                    model,
                    system_prompt,
                    cancel.clone(),
                    &args,
                )
                .await
            } else {
                let safety = tools.registry.safety(&call.name, &args);
                let approved = safety == Safety::ReadOnly
                    || tools
                        .approve
                        .approve(&message_id, &call.id, &call.name, safety, &args)
                        .await;
                if approved {
                    tools.registry.run(&call.name, args, tools.root).await
                } else {
                    ff_tools::ToolOutcome::error(format!(
                        "call to `{}` was not approved",
                        call.name
                    ))
                }
            };

            store.add_tool_result_message(session_id, call.id.clone(), outcome.content.clone());
            executed.push(call.id.clone());
            on_event(AgentEvent::ToolCallFinished {
                message_id: message_id.clone(),
                call_id: call.id.clone(),
                success: outcome.success,
                result: outcome.content,
            });
        }

        // If we were cancelled mid-loop, every requested call still needs a matching
        // tool result, or the next request's history is malformed (an assistant
        // `tool_calls` message with no tool replies -> provider rejects it with 400).
        for call in calls.values() {
            if !executed.contains(&call.id) {
                store.add_tool_result_message(
                    session_id,
                    call.id.clone(),
                    "[cancelled]".to_string(),
                );
            }
        }

        last = Some(finalized);
    }

    // Hit the iteration cap (or was cancelled) without a plain text answer.
    let mut msg =
        last.unwrap_or_else(|| store.add_message(session_id, Role::Assistant, String::new()));
    if msg.content.is_empty() {
        // The final assistant message only carried tool calls, so it would render as
        // an empty bubble. Replace it with a notice explaining why the turn stopped.
        let notice = if cancel.is_cancelled() {
            "[stopped]"
        } else {
            "[stopped: reached tool-call limit]"
        };
        msg = store.set_message_content(&msg.id, session_id, notice.to_string());
    }
    on_event(AgentEvent::Done {
        message_id: msg.id.clone(),
        final_message: Some(msg.content.clone()),
        turns: Some(turn_count),
        token_count: None,
    });
    Ok(msg)
}

/// Spawns a scoped child agent for an `agent` tool call (#234): a fresh ephemeral
/// session seeded with the `task`, run to completion against the same workspace and
/// approver, returning only the child's final message as the tool result. The child
/// session is deleted afterward — the parent never inherits its transcript.
#[allow(clippy::too_many_arguments)]
async fn run_subagent(
    provider: &dyn Provider,
    store: &SessionStore,
    parent: &ToolContext<'_>,
    model: &str,
    system_prompt: Option<&str>,
    cancel: CancelToken,
    args: &serde_json::Value,
) -> ff_tools::ToolOutcome {
    if parent.depth >= parent.max_depth {
        return ff_tools::ToolOutcome::error(
            "sub-agents cannot spawn further sub-agents (max delegation depth reached)",
        );
    }

    let task = match args.get("task").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => {
            return ff_tools::ToolOutcome::error(
                "agent: `task` is required and must be a non-empty string",
            )
        }
    };

    // Clamp the child's iteration budget to a safe ceiling.
    const SUBAGENT_ITER_CAP: usize = 16;
    let max_iterations = args
        .get("max_iterations")
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize).clamp(1, SUBAGENT_ITER_CAP))
        .unwrap_or(SUBAGENT_ITER_CAP);

    // Optional tool allowlist for the subtask (e.g. a read-only audit).
    let allowed: Option<std::collections::HashSet<String>> =
        args.get("tools").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        });

    let child = store.create_session(Some(task.clone()));
    store.add_message(&child.id, Role::User, task);

    let child_ctx = ToolContext {
        registry: parent.registry,
        root: parent.root,
        approve: parent.approve,
        max_iterations,
        depth: parent.depth + 1,
        max_depth: parent.max_depth,
        allowed,
    };

    // Child events are swallowed: the parent receives only the summary, never the
    // child's token/tool stream — the whole point of fresh-context delegation.
    let result = Box::pin(run_turn(
        provider,
        store,
        &child_ctx,
        &child.id,
        model,
        system_prompt,
        false,
        cancel,
        |_event| {},
    ))
    .await;

    store.delete_session(&child.id);

    match result {
        Ok(msg) if !msg.content.trim().is_empty() => ff_tools::ToolOutcome::ok(msg.content),
        Ok(_) => ff_tools::ToolOutcome::ok("[sub-agent finished without a summary]"),
        Err(e) => ff_tools::ToolOutcome::error(format!("sub-agent failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ff_llm::{Chunk, ChunkStream, LlmError, ToolCallDelta};
    use std::sync::atomic::AtomicUsize;

    struct AlwaysApprove;
    #[async_trait]
    impl Approver for AlwaysApprove {
        async fn approve(
            &self,
            _message_id: &str,
            _call_id: &str,
            _name: &str,
            _safety: Safety,
            _args: &serde_json::Value,
        ) -> bool {
            true
        }
    }

    struct AlwaysDeny;
    #[async_trait]
    impl Approver for AlwaysDeny {
        async fn approve(
            &self,
            _message_id: &str,
            _call_id: &str,
            _name: &str,
            _safety: Safety,
            _args: &serde_json::Value,
        ) -> bool {
            false
        }
    }

    /// Approves, but cancels the turn first — to exercise the cancel-mid-loop path.
    struct CancelOnApprove(CancelToken);
    #[async_trait]
    impl Approver for CancelOnApprove {
        async fn approve(
            &self,
            _message_id: &str,
            _call_id: &str,
            _name: &str,
            _safety: Safety,
            _args: &serde_json::Value,
        ) -> bool {
            self.0.cancel();
            true
        }
    }

    /// Yields once before approving, proving the loop actually awaits the decision.
    struct YieldThenApprove;
    #[async_trait]
    impl Approver for YieldThenApprove {
        async fn approve(
            &self,
            _message_id: &str,
            _call_id: &str,
            _name: &str,
            _safety: Safety,
            _args: &serde_json::Value,
        ) -> bool {
            tokio::task::yield_now().await;
            true
        }
    }

    fn ctx<'a>(
        registry: &'a ToolRegistry,
        root: &'a Path,
        approve: &'a dyn Approver,
    ) -> ToolContext<'a> {
        ToolContext::new(registry, root, approve, 8)
    }

    struct TextProvider;

    #[async_trait]
    impl Provider for TextProvider {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let chunks = vec![
                Ok(Chunk {
                    delta: "Hel".into(),
                    ..Chunk::default()
                }),
                Ok(Chunk {
                    delta: "lo".into(),
                    done: true,
                    ..Chunk::default()
                }),
            ];
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    /// First call requests a `bash` tool call; second call returns plain text.
    struct ToolThenText {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for ToolThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("call_1".into()),
                        name: Some("bash".into()),
                        arguments: r#"{"command":"echo wired"}"#.into(),
                    }],
                    done: true,
                    ..Chunk::default()
                })]
            } else {
                vec![Ok(Chunk {
                    delta: "done: wired".into(),
                    done: true,
                    ..Chunk::default()
                })]
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    /// First call requests an `ask_user` tool call; second call returns plain text.
    struct AskThenText {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for AskThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = if n == 0 {
                vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("ask_1".into()),
                        name: Some("ask_user".into()),
                        arguments: r#"{"question":"Which file?"}"#.into(),
                    }],
                    done: true,
                    ..Chunk::default()
                })]
            } else {
                vec![Ok(Chunk {
                    delta: "using main.rs".into(),
                    done: true,
                    ..Chunk::default()
                })]
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    /// First call requests an `agent` (sub-agent) call; the child's call returns a
    /// summary; the parent's final call returns plain text. One shared counter drives
    /// parent and child turns through the same provider instance.
    struct AgentThenText {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for AgentThenText {
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let chunks = match n {
                0 => vec![Ok(Chunk {
                    tool_calls: vec![ToolCallDelta {
                        index: 0,
                        id: Some("agent_1".into()),
                        name: Some("agent".into()),
                        arguments: r#"{"task":"audit the foo module"}"#.into(),
                    }],
                    done: true,
                    ..Chunk::default()
                })],
                1 => vec![Ok(Chunk {
                    delta: "child: audit complete, 0 issues".into(),
                    done: true,
                    ..Chunk::default()
                })],
                _ => vec![Ok(Chunk {
                    delta: "parent: delegated and done".into(),
                    done: true,
                    ..Chunk::default()
                })],
            };
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    /// Answers an interactive `ask`; denies everything that needs approval (it should
    /// never be asked to approve an interactive tool).
    struct CannedAnswer(&'static str);
    #[async_trait]
    impl Approver for CannedAnswer {
        async fn approve(
            &self,
            _message_id: &str,
            _call_id: &str,
            _name: &str,
            _safety: Safety,
            _args: &serde_json::Value,
        ) -> bool {
            false
        }
        async fn ask(
            &self,
            _message_id: &str,
            _call_id: &str,
            args: &serde_json::Value,
        ) -> Option<String> {
            // The host receives the tool args and reads the `question` field.
            assert_eq!(args["question"], "Which file?");
            Some(self.0.to_string())
        }
    }

    #[tokio::test]
    async fn streams_and_persists_text_turn() {
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "hi".into());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;

        let mut tokens = String::new();
        let mut done = false;
        let msg = run_turn(
            &TextProvider,
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| match ev {
                AgentEvent::Token { delta, .. } => tokens.push_str(&delta),
                AgentEvent::Done { .. } => done = true,
                AgentEvent::Error { .. } => panic!("unexpected error"),
                _ => {}
            },
        )
        .await
        .unwrap();

        assert_eq!(tokens, "Hello");
        assert!(done);
        assert_eq!(msg.content, "Hello");
    }

    #[tokio::test]
    async fn executes_tool_then_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "run echo".into());
        let registry = ToolRegistry::with_defaults();
        let approve = AlwaysApprove;
        let provider = ToolThenText {
            calls: AtomicUsize::new(0),
        };

        let mut started = 0;
        let mut finished_ok = false;
        let mut final_text = String::new();
        let msg = run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| match ev {
                AgentEvent::ToolCallStarted { name, .. } => {
                    assert_eq!(name, "bash");
                    started += 1;
                }
                AgentEvent::ToolCallFinished {
                    success, result, ..
                } => {
                    finished_ok = success;
                    assert!(result.contains("wired"));
                }
                AgentEvent::Token { delta, .. } => final_text.push_str(&delta),
                AgentEvent::Reasoning { .. } => {}
                AgentEvent::Error { message } => panic!("error: {message}"),
                AgentEvent::Done { .. } => {}
            },
        )
        .await
        .unwrap();

        assert_eq!(started, 1);
        assert!(finished_ok);
        assert_eq!(final_text, "done: wired");
        assert_eq!(msg.content, "done: wired");

        // History should be: user, assistant(tool_calls), tool(result), assistant(final).
        let history = store.get_messages(&s.id);
        assert_eq!(history.len(), 4);
        assert_eq!(history[1].role, Role::Assistant);
        assert!(history[1].tool_calls.is_some());
        assert_eq!(history[2].role, Role::Tool);
        assert_eq!(history[2].tool_call_id.as_deref(), Some("call_1"));
    }

    /// #44: an `ask_user` call routes to `Approver::ask`; the answer becomes the tool
    /// result and the turn resumes, with well-formed history.
    #[tokio::test]
    async fn ask_user_round_trips_answer_as_tool_result() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "edit the file".into());
        let registry = ToolRegistry::with_defaults();
        let approve = CannedAnswer("main.rs");
        let provider = AskThenText {
            calls: AtomicUsize::new(0),
        };

        let mut started_name = String::new();
        let mut result = String::new();
        let mut ok = false;
        let mut final_text = String::new();
        let msg = run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| match ev {
                AgentEvent::ToolCallStarted { name, .. } => started_name = name,
                AgentEvent::ToolCallFinished {
                    success, result: r, ..
                } => {
                    ok = success;
                    result = r;
                }
                AgentEvent::Token { delta, .. } => final_text.push_str(&delta),
                AgentEvent::Reasoning { .. } => {}
                AgentEvent::Error { message } => panic!("error: {message}"),
                AgentEvent::Done { .. } => {}
            },
        )
        .await
        .unwrap();

        assert_eq!(started_name, "ask_user");
        assert!(ok, "an answered question is a successful tool result");
        assert_eq!(result, "main.rs");
        assert_eq!(final_text, "using main.rs");
        assert_eq!(msg.content, "using main.rs");

        // History: user, assistant(tool_calls), tool(answer), assistant(final).
        let history = store.get_messages(&s.id);
        assert_eq!(history.len(), 4);
        assert_eq!(history[2].role, Role::Tool);
        assert_eq!(history[2].tool_call_id.as_deref(), Some("ask_1"));
        assert_eq!(history[2].content, "main.rs");
    }

    /// #44: a dismissed question (the default `ask` returns `None`) still emits a
    /// matching tool result, so history never goes malformed.
    #[tokio::test]
    async fn dismissed_ask_emits_tool_result_not_hang() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "edit the file".into());
        let registry = ToolRegistry::with_defaults();
        // AlwaysDeny uses the default `ask` (returns None) -> dismissed.
        let approve = AlwaysDeny;
        let provider = AskThenText {
            calls: AtomicUsize::new(0),
        };

        let mut result = String::new();
        let msg = run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::ToolCallFinished { result: r, .. } = ev {
                    result = r;
                }
            },
        )
        .await
        .unwrap();

        assert!(result.contains("no answer"));
        let history = store.get_messages(&s.id);
        assert_eq!(history[2].role, Role::Tool);
        assert_eq!(history[2].tool_call_id.as_deref(), Some("ask_1"));
        // Turn still completed with the follow-up assistant text.
        assert_eq!(msg.content, "using main.rs");
    }

    /// Cancelling mid-execution must still leave a matching tool result for every
    /// requested call, so the next turn's history stays well-formed.
    #[tokio::test]
    async fn cancel_mid_loop_backfills_tool_results() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "do two things".into());
        let registry = ToolRegistry::with_defaults();

        struct TwoCalls;
        #[async_trait]
        impl Provider for TwoCalls {
            async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
                let chunks = vec![Ok(Chunk {
                    tool_calls: vec![
                        ToolCallDelta {
                            index: 0,
                            id: Some("call_a".into()),
                            name: Some("bash".into()),
                            arguments: r#"{"command":"touch a"}"#.into(),
                        },
                        ToolCallDelta {
                            index: 1,
                            id: Some("call_b".into()),
                            name: Some("bash".into()),
                            arguments: r#"{"command":"touch b"}"#.into(),
                        },
                    ],
                    done: true,
                    ..Chunk::default()
                })];
                Ok(futures_util::stream::iter(chunks).boxed())
            }
        }

        // Approving the first (write) call cancels the turn, so the second is skipped.
        let cancel = CancelToken::new();
        let approve = CancelOnApprove(cancel.clone());

        let msg = run_turn(
            &TwoCalls,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            cancel,
            |_| {},
        )
        .await
        .unwrap();

        // Every requested tool call must have a matching Role::Tool reply.
        let history = store.get_messages(&s.id);
        let assistant = history
            .iter()
            .find(|m| m.tool_calls.is_some())
            .expect("assistant tool-call message");
        let requested: Vec<&str> = assistant
            .tool_calls
            .as_ref()
            .unwrap()
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        let replied: Vec<String> = history
            .iter()
            .filter(|m| m.role == Role::Tool)
            .filter_map(|m| m.tool_call_id.clone())
            .collect();
        for id in &requested {
            assert!(
                replied.iter().any(|r| r == id),
                "missing tool result for {id}"
            );
        }
        // The skipped call is recorded as cancelled.
        assert!(history
            .iter()
            .any(|m| m.role == Role::Tool && m.content == "[cancelled]"));
        // The final bubble is never empty.
        assert!(!msg.content.is_empty());
    }

    #[tokio::test]
    async fn denied_write_tool_reports_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "old\n").unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "edit it".into());
        let registry = ToolRegistry::with_defaults();
        // Deny everything that needs approval.
        let deny = AlwaysDeny;

        struct EditProvider {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl Provider for EditProvider {
            async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                let chunks = if n == 0 {
                    vec![Ok(Chunk {
                        tool_calls: vec![ToolCallDelta {
                            index: 0,
                            id: Some("call_e".into()),
                            name: Some("edit".into()),
                            arguments: r#"{"path":"f.txt","old_str":"old","new_str":"new"}"#.into(),
                        }],
                        done: true,
                        ..Chunk::default()
                    })]
                } else {
                    vec![Ok(Chunk {
                        delta: "ok".into(),
                        done: true,
                        ..Chunk::default()
                    })]
                };
                Ok(futures_util::stream::iter(chunks).boxed())
            }
        }

        let mut denied_reported = false;
        run_turn(
            &EditProvider {
                calls: AtomicUsize::new(0),
            },
            &store,
            &ctx(&registry, dir.path(), &deny),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::ToolCallFinished {
                    success, result, ..
                } = ev
                {
                    if !success && result.contains("not approved") {
                        denied_reported = true;
                    }
                }
            },
        )
        .await
        .unwrap();

        assert!(denied_reported);
        // The file must be untouched because the edit was denied.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "old\n"
        );
    }

    /// The loop must await an async approval decision before running the tool.
    #[tokio::test]
    async fn awaits_async_approval() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "run echo".into());
        let registry = ToolRegistry::with_defaults();
        let approve = YieldThenApprove;
        let provider = ToolThenText {
            calls: AtomicUsize::new(0),
        };

        let mut finished_ok = false;
        run_turn(
            &provider,
            &store,
            &ctx(&registry, dir.path(), &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |ev| {
                if let AgentEvent::ToolCallFinished { success, .. } = ev {
                    finished_ok = success;
                }
            },
        )
        .await
        .unwrap();

        assert!(finished_ok, "tool should run after async approval resolves");
    }

    /// Captures the `ChatRequest` it receives so a test can assert what reached the
    /// provider (the system prompt is transient — never stored in history).
    struct RecordingProvider {
        seen: Arc<std::sync::Mutex<Vec<ChatMessage>>>,
    }

    #[async_trait]
    impl Provider for RecordingProvider {
        async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
            *self.seen.lock().unwrap() = req.messages;
            Ok(futures_util::stream::iter(vec![Ok(Chunk {
                delta: "ok".into(),
                done: true,
                ..Chunk::default()
            })])
            .boxed())
        }
    }

    #[tokio::test]
    async fn system_prompt_is_injected_into_request_not_history() {
        use ff_skills::SkillRegistry;

        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("rust-debug");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: rust-debug\ndescription: Systematic Rust debugging\nversion: 0.1.0\n---\nBisect with bash.\n",
        )
        .unwrap();
        let (skills, errs) = SkillRegistry::load_dir(dir.path());
        assert!(errs.is_empty());

        let user = UserContext {
            local_date: "2026-06-13".into(),
            timezone: "America/Chicago".into(),
            time_of_day: TimeOfDay::Morning,
        };
        let system = build_system_prompt(None, &skills, &[], &user, None);

        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "hi".into());
        let registry = ToolRegistry::new();
        let root = std::env::current_dir().unwrap();
        let approve = AlwaysApprove;
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = RecordingProvider { seen: seen.clone() };

        run_turn(
            &provider,
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            Some(&system),
            false,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let msgs = seen.lock().unwrap();
        assert_eq!(msgs[0].role, "system");
        let sys = msgs[0].content.as_deref().unwrap();
        assert!(
            sys.contains("- rust-debug: Systematic Rust debugging"),
            "{sys}"
        );
        assert!(sys.contains("Current: 2026-06-13, morning (America/Chicago)."));
        assert_eq!(msgs[1].role, "user");

        // The system prompt must not be persisted: history is just [user, assistant].
        let history = store.get_messages(&s.id);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, Role::User);
        assert_eq!(history[1].role, Role::Assistant);
    }

    #[tokio::test]
    async fn subagent_delegates_and_returns_summary() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "delegate an audit".into());
        let registry = ToolRegistry::with_defaults();
        let root = dir.path().to_path_buf();
        let approve = AlwaysApprove;
        let provider = AgentThenText {
            calls: AtomicUsize::new(0),
        };

        let msg = run_turn(
            &provider,
            &store,
            &ctx(&registry, &root, &approve),
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        // Parent finished with its own answer, having delegated mid-turn.
        assert_eq!(msg.content, "parent: delegated and done");

        // The child's summary came back as the parent's tool result.
        let history = store.get_messages(&s.id);
        let tool_result = history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("parent should have a tool result for the agent call");
        assert_eq!(tool_result.content, "child: audit complete, 0 issues");

        // The ephemeral child session was deleted — only the parent remains.
        assert_eq!(store.list_sessions().len(), 1);
    }

    #[tokio::test]
    async fn subagent_depth_guard_refuses_nested_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "try to delegate from a child".into());
        let registry = ToolRegistry::with_defaults();
        let root = dir.path().to_path_buf();
        let approve = AlwaysApprove;
        let provider = AgentThenText {
            calls: AtomicUsize::new(0),
        };

        // Simulate an agent already at the depth cap.
        let at_cap = ToolContext {
            registry: &registry,
            root: &root,
            approve: &approve,
            max_iterations: 8,
            depth: 1,
            max_depth: 1,
            allowed: None,
        };

        run_turn(
            &provider,
            &store,
            &at_cap,
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let history = store.get_messages(&s.id);
        let tool_result = history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("the refused spawn still produces a tool result");
        assert!(
            tool_result.content.contains("max delegation depth"),
            "{}",
            tool_result.content
        );
        // No child session was ever created.
        assert_eq!(store.list_sessions().len(), 1);
    }

    #[tokio::test]
    async fn subagent_allowlist_blocks_disallowed_tool() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new();
        let s = store.create_session(None);
        store.add_message(&s.id, Role::User, "scoped read-only run".into());
        let registry = ToolRegistry::with_defaults();
        let root = dir.path().to_path_buf();
        let approve = AlwaysApprove;
        let provider = ToolThenText {
            calls: AtomicUsize::new(0),
        };

        // A sub-agent scoped to read-only tools tries to call `bash`.
        let scoped = ToolContext {
            registry: &registry,
            root: &root,
            approve: &approve,
            max_iterations: 8,
            depth: 1,
            max_depth: 1,
            allowed: Some(["view".to_string()].into_iter().collect()),
        };

        run_turn(
            &provider,
            &store,
            &scoped,
            &s.id,
            "mock",
            None,
            false,
            CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let history = store.get_messages(&s.id);
        let tool_result = history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("the disallowed call still produces a tool result");
        assert!(
            tool_result.content.contains("not permitted"),
            "{}",
            tool_result.content
        );
    }
}
