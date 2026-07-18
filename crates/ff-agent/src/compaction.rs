//! Context compaction: deciding *when* a session is under context pressure and
//! *how* to relieve it (RFC 0006 §7.2).
//!
//! This module is the extension seam for FlowForge's long-term goal of being the
//! place that gets compaction right. Two traits are kept deliberately separate so
//! the policy and the mechanism evolve independently:
//!
//! - [`ContextPressureEstimator`] — *when*. v1 ships [`ProxyTokenEstimator`]
//!   (tokenx-rs heuristic, ~96% accurate, vs a fixed budget). Per-model
//!   context-window metadata plugs in here later without touching the trigger
//!   that consumes it.
//! - [`CompactionStrategy`] — *how*. v1 ships [`MemoryFlush`] (a silent agentic turn
//!   that persists durable facts to memory before they are summarized away). Future
//!   strategies (knowledge-graph compression, hierarchical summarization, …) become
//!   sibling implementations the host can swap in.
//!
//! M5.2 wires `ProxyTokenEstimator` + `MemoryFlush`; the traits exist so later work
//! is additive.

use async_trait::async_trait;
use ff_core::{Message, Role};
use ff_llm::{ChatMessage, ChatRequest, FunctionCall, Provider, ToolCall as LlmToolCall};
use ff_session::SessionStore;
use ff_tools::{ToolOutcome, ToolRegistry};
use futures_util::StreamExt;

use crate::compaction_extractive::GradedBands;
use crate::{to_chat, AgentError, CancelToken, KEEP_RECENT_VERBATIM};

/// Default per-session context budget, in proxy tokens. Conservative so the flush
/// fires well before any real context limit. The host owns the real value; this is
/// the v1 stand-in until per-model context windows land (see [`ProxyTokenEstimator`]).
pub const DEFAULT_CONTEXT_BUDGET_TOKENS: u64 = 24_000;

/// Default fraction of the budget at which compaction should fire.
pub const DEFAULT_FLUSH_AT_FRACTION: f64 = 0.75;

/// A flush makes at most this many provider round-trips (e.g. search-to-dedupe, then
/// write). Bounds the silent turn so a misbehaving model cannot spin.
const MAX_FLUSH_ITERATIONS: usize = 3;

/// How close a session's transcript is to its context budget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextPressure {
    /// Estimated tokens currently in the transcript.
    pub estimated_tokens: u64,
    /// The budget those tokens are measured against.
    pub budget_tokens: u64,
}

impl ContextPressure {
    /// Fraction of the budget consumed (`0.0` when the budget is zero).
    pub fn fraction(&self) -> f64 {
        if self.budget_tokens == 0 {
            0.0
        } else {
            self.estimated_tokens as f64 / self.budget_tokens as f64
        }
    }

    /// Whether the transcript is at or past `fraction` of the budget.
    pub fn is_over(&self, fraction: f64) -> bool {
        self.fraction() >= fraction
    }
}

/// Estimates how full a session's context is. The seam where per-model
/// context-window metadata replaces the proxy without changing the trigger.
pub trait ContextPressureEstimator: Send + Sync {
    /// Assess `messages` (the transcript) against this estimator's budget. `model`
    /// is passed so a future estimator can size the budget to the model's window;
    /// the v1 [`ProxyTokenEstimator`] ignores it.
    fn assess(&self, messages: &[Message], model: &str) -> ContextPressure;
}

/// v1 estimator: `tokenx-rs` heuristic (~96% accurate against tiktoken cl100k_base)
/// over the transcript vs a fixed budget. Model-agnostic, zero-cost, and never wrong
/// in a way that loses data -- it only decides when to *offer* to persist memory.
#[derive(Debug, Clone, Copy)]
pub struct ProxyTokenEstimator {
    pub budget_tokens: u64,
}

impl Default for ProxyTokenEstimator {
    fn default() -> Self {
        Self {
            budget_tokens: DEFAULT_CONTEXT_BUDGET_TOKENS,
        }
    }
}

impl ContextPressureEstimator for ProxyTokenEstimator {
    fn assess(&self, messages: &[Message], _model: &str) -> ContextPressure {
        // Count reasoning too (#378): it is replayed on the wire for reasoning
        // gateways (#375 PR-2), so a turn's true cost includes its persisted CoT.
        // Without this, injected reasoning bloats the request without ever tripping
        // compaction -- the window blows before pressure is even noticed.
        let tokens: usize = messages
            .iter()
            .map(|m| {
                ff_llm::count_tokens(&m.content)
                    + m.reasoning.as_deref().map_or(0, ff_llm::count_tokens)
            })
            .sum();
        ContextPressure {
            estimated_tokens: tokens as u64,
            budget_tokens: self.budget_tokens,
        }
    }
}

/// What a compaction strategy needs to do its work. References only — the strategy
/// reads the transcript from `store` and dispatches tools through `registry`.
pub struct CompactionContext<'a> {
    pub provider: &'a dyn Provider,
    pub store: &'a SessionStore,
    pub registry: &'a ToolRegistry,
    /// Workspace root, for tool dispatch parity (memory tools ignore it).
    pub root: &'a std::path::Path,
    pub session_id: &'a str,
    pub model: &'a str,
    pub cancel: CancelToken,
}

/// The result of running a compaction strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionOutcome {
    /// The strategy persisted `writes` memory writes.
    Wrote { writes: usize },
    /// The strategy chose to persist nothing (e.g. a `NO_REPLY` flush).
    NoReply,
}

/// Relieves context pressure. The *how* seam — see the module docs.
#[async_trait]
pub trait CompactionStrategy: Send + Sync {
    async fn compact(&self, ctx: CompactionContext<'_>) -> Result<CompactionOutcome, AgentError>;
}

/// v1 strategy: a silent agentic turn that asks the model to persist anything
/// durable to memory before the context is summarized away (RFC 0006 §7.2).
///
/// Silent *by construction*: it snapshots the transcript read-only, runs a bounded
/// tool loop exposing only the `memory_*` tools, and persists nothing to the visible
/// message store — `memory_write` writes go to the user's Markdown memory on disk.
/// A turn that writes nothing (the model replied `NO_REPLY`) yields
/// [`CompactionOutcome::NoReply`].
pub struct MemoryFlush;

#[async_trait]
impl CompactionStrategy for MemoryFlush {
    async fn compact(&self, ctx: CompactionContext<'_>) -> Result<CompactionOutcome, AgentError> {
        let history = ctx.store.get_messages(ctx.session_id);
        if history.is_empty() {
            return Ok(CompactionOutcome::NoReply);
        }

        // Only the memory tools are offered during a flush — the model cannot run
        // bash, edit files, etc. in this autonomous, unattended turn.
        let tools: Vec<serde_json::Value> = ctx
            .registry
            .openai_tools()
            .into_iter()
            .filter(|t| {
                t["function"]["name"]
                    .as_str()
                    .is_some_and(|n| n.starts_with("memory_"))
            })
            .collect();

        // Local conversation, never persisted to the visible transcript.
        let mut messages = vec![ChatMessage {
            role: "system".to_string(),
            content: Some(crate::build_flush_prompt()),
            tool_calls: None,
            tool_call_id: None,
            name: None,

            attachments: Vec::new(),
            reasoning: None,
        }];
        // #993: feed the flush the Tier-1 *compacted* wire, not the full verbatim
        // history. The flush only needs to spot durable facts, and a big tool-result
        // blob (codegraph dump, diff) carries almost none — so truncating them cuts
        // background token cost with no loss of the conversational detail flush reads.
        // `graded_v1(0)` is the reversible, lossless-enough depth-graded pass (NOT the
        // Tier-2 abstractive summary); recent turns stay verbatim; already-compacted
        // (ingest) blobs are skipped by the marker guard. Deterministic + mechanical,
        // so re-deriving it here (rather than plumbing run_turn's wire through the
        // post-turn flush) yields the same bytes at negligible cost.
        let compacted = GradedBands::graded_v1(0)
            .compact_graded_collect(&history, KEEP_RECENT_VERBATIM)
            .messages;
        messages.extend(to_chat(&compacted));

        let mut writes = 0usize;
        for _ in 0..MAX_FLUSH_ITERATIONS {
            if ctx.cancel.is_cancelled() {
                break;
            }
            let req = ChatRequest {
                model: ctx.model.to_string(),
                messages: messages.clone(),
                tools: tools.clone(),
                // Internal summarization turn — never stream reasoning here.
                thinking: false,
                // Bounded internal output; no large tool-call payload to protect.
                max_tokens: None,
                cache_messages: false,
            };
            let calls = collect_tool_calls(ctx.provider, req).await?;

            // No tool calls -> the model answered with text (discarded) or NO_REPLY.
            if calls.is_empty() {
                break;
            }

            messages.push(assistant_tool_calls_message(&calls));
            for call in &calls {
                let args: serde_json::Value =
                    serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::Null);
                let outcome = if call.name.starts_with("memory_") {
                    ctx.registry
                        .run_with_session(&call.name, args, ctx.root, ctx.session_id)
                        .await
                } else {
                    ToolOutcome::error("only memory tools are available during a flush")
                };
                if call.name == "memory_write" && outcome.success {
                    writes += 1;
                }
                messages.push(tool_result_message(&call.id, outcome.content));
            }
        }

        if writes > 0 {
            Ok(CompactionOutcome::Wrote { writes })
        } else {
            Ok(CompactionOutcome::NoReply)
        }
    }
}

/// A tool call collected from one streamed response.
struct CollectedCall {
    id: String,
    name: String,
    arguments: String,
}

/// Stream one provider response, discarding any assistant text and returning the
/// (possibly empty) tool calls it requested.
async fn collect_tool_calls(
    provider: &dyn Provider,
    req: ChatRequest,
) -> Result<Vec<CollectedCall>, AgentError> {
    use std::collections::BTreeMap;
    let mut stream = provider.chat_stream(req).await?;
    let mut bufs: BTreeMap<u32, CollectedCall> = BTreeMap::new();
    while let Some(item) = stream.next().await {
        let chunk = item?;
        for frag in chunk.tool_calls {
            let buf = bufs.entry(frag.index).or_insert_with(|| CollectedCall {
                id: String::new(),
                name: String::new(),
                arguments: String::new(),
            });
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
    Ok(bufs.into_values().collect())
}

fn assistant_tool_calls_message(calls: &[CollectedCall]) -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: None,
        tool_calls: Some(
            calls
                .iter()
                .map(|c| LlmToolCall {
                    id: c.id.clone(),
                    kind: "function".to_string(),
                    function: FunctionCall {
                        name: c.name.clone(),
                        arguments: c.arguments.clone(),
                    },
                })
                .collect(),
        ),
        tool_call_id: None,
        name: None,

        attachments: Vec::new(),
        reasoning: None,
    }
}

fn tool_result_message(call_id: &str, content: String) -> ChatMessage {
    ChatMessage {
        role: role_str(Role::Tool).to_string(),
        content: Some(content),
        tool_calls: None,
        tool_call_id: Some(call_id.to_string()),
        name: None,

        attachments: Vec::new(),
        reasoning: None,
    }
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::Tool => "tool",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    }
}

/// Decide whether a session is due for a pre-compaction flush. Pure so the trigger
/// policy is testable independent of the host (RFC 0006 §7.2):
/// - never flush below `fraction` of the budget;
/// - flush the first time over budget (`last_flush_count == None`);
/// - thereafter flush again only once the transcript has grown by `reflush_interval`
///   messages, so a long over-budget session does not flush every turn.
pub fn flush_due(
    pressure: ContextPressure,
    message_count: u64,
    last_flush_count: Option<u64>,
    fraction: f64,
    reflush_interval: u64,
) -> bool {
    if !pressure.is_over(fraction) {
        return false;
    }
    match last_flush_count {
        None => true,
        Some(prev) => message_count >= prev.saturating_add(reflush_interval),
    }
}

#[cfg(test)]
mod tests;
