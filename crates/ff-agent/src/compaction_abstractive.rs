//! Tier-2 abstractive cold-tail summary (RFC 0016 §4 Tier 2, M7.0).
//!
//! The conventional "summarize the history" path, deliberately demoted to a
//! *fallback*: it runs only when the mechanical, free Tier-1 extractive pass
//! (`compaction_extractive`) cannot relieve enough context pressure. When it
//! fires, the cold prefix of the transcript is condensed into a single summary
//! message via the session's LLM (or a configured override), and the most
//! recent `keep_recent` messages are kept byte-identical.
//!
//! ## Reversibility (RFC 0016 §7)
//! Like Tier 1, this is lossy *in context* but lossless *on demand*: the
//! verbatim cold block is persisted under a content-hash key and the summary
//! message ends with the shared `[compacted; retrieve key=...]` marker, so the
//! model can pull the original back with `compaction_retrieve`. The summary is a
//! request-only wire transform -- the session store keeps the full verbatim
//! transcript, exactly as the Tier-1 pre-send pass does.
//!
//! ## Default-off
//! [`AbstractiveConfig::enabled`] defaults to `false` (RFC 0016 M7.0 ships the
//! baseline opt-in). The summarizer model defaults to the session model and is
//! overridable via [`AbstractiveConfig::model`]; an override stays on the same
//! provider/connection (cross-connection routing is a follow-up).

use ff_core::{Message, Role};
use ff_llm::{ChatMessage, ChatRequest, Provider};
use futures_util::StreamExt;

use crate::compaction_extractive::{
    content_key, proxy_tokens, CompactionSavings, COMPACTION_MARKER_PREFIX,
};
use crate::{AgentError, CancelToken};

/// Tier-2 knobs. Defaults: off, session model, fire above the Tier-1 fraction.
#[derive(Debug, Clone)]
pub struct AbstractiveConfig {
    /// Master switch. Default `false` (RFC 0016 M7.0 = opt-in baseline).
    pub enabled: bool,
    /// Summarizer model. `None` = use the session model (the default the design
    /// settled on); `Some` = override on the *same* provider/connection.
    pub model: Option<String>,
    /// Context-pressure fraction at which Tier 2 engages. Set above the Tier-1
    /// extractive fraction so abstractive summary is the fallback, not the first
    /// response (RFC 0016 §9 Q1).
    pub fire_at_fraction: f64,
    /// Don't summarize a trivially short cold prefix -- the LLM round-trip would
    /// not pay for itself.
    pub min_cold_messages: usize,
}

impl Default for AbstractiveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: None,
            fire_at_fraction: 0.90,
            min_cold_messages: 4,
        }
    }
}

/// The result of one abstractive pass over the cold prefix.
#[derive(Debug, Clone)]
pub struct SummaryResult {
    /// Wire-ready transcript: `[summary message] + recent verbatim tail`.
    pub messages: Vec<Message>,
    /// `(message_id, key, original)` for the collapsed cold block, to persist so
    /// `compaction_retrieve` can fetch the verbatim original back.
    pub original: Option<(String, String, String)>,
    /// Index into the input transcript that the summary covers (`messages[..boundary]`
    /// were summarized). Lets the host reuse the summary while keeping everything
    /// after the boundary verbatim as the turn grows.
    pub boundary: usize,
    /// Proxy-token savings for this pass.
    pub savings: CompactionSavings,
}

/// Produces Tier-2 abstractive summaries. Holds only config; the provider and
/// transcript are passed per call so one summarizer serves every turn.
#[derive(Debug, Clone, Default)]
pub struct AbstractiveSummarizer {
    pub config: AbstractiveConfig,
}

impl AbstractiveSummarizer {
    #[must_use]
    pub fn new(config: AbstractiveConfig) -> Self {
        Self { config }
    }

    /// Summarize the cold prefix of `messages`, leaving the most recent
    /// `keep_recent` byte-identical. Returns `Ok(None)` (no substitution) when
    /// disabled, when the cold prefix is too short, when the model returns an
    /// empty summary, or when the summary would not actually shrink the block --
    /// the same "only emit when it shrinks" discipline as the Tier-1 compactor.
    pub async fn summarize_cold(
        &self,
        provider: &dyn Provider,
        session_model: &str,
        messages: &[Message],
        keep_recent: usize,
        cancel: &CancelToken,
    ) -> Result<Option<SummaryResult>, AgentError> {
        if !self.config.enabled {
            return Ok(None);
        }
        let n = messages.len();
        let cold_end = n.saturating_sub(keep_recent);
        if cold_end < self.config.min_cold_messages {
            return Ok(None);
        }
        let cold = &messages[..cold_end];
        let source = render_cold(cold);
        if source.trim().is_empty() {
            return Ok(None);
        }
        let before = proxy_tokens(&source);

        let model = self.config.model.as_deref().unwrap_or(session_model);
        let req = ChatRequest {
            model: model.to_string(),
            messages: vec![
                ChatMessage::text("system", build_summary_prompt()),
                ChatMessage::text("user", source.clone()),
            ],
            tools: Vec::new(),
            // Internal condensation turn -- never stream reasoning here.
            thinking: false,
            // Bounded internal output; no large tool-call payload to protect.
            max_tokens: None,
            cache_messages: false,
        };
        let summary = collect_text(provider, req, cancel).await?;
        let summary = summary.trim();
        if summary.is_empty() || cancel.is_cancelled() {
            return Ok(None);
        }

        let key = content_key(&source);
        let content = format!(
            "Summary of {cold_end} earlier messages in this conversation:\n{summary}\n{COMPACTION_MARKER_PREFIX}{key}]"
        );
        // Only substitute when the summary actually shrinks the cold block.
        if proxy_tokens(&content) >= before {
            return Ok(None);
        }
        let after = proxy_tokens(&content);

        let mut out = Vec::with_capacity(keep_recent + 1);
        out.push(summary_message(&content, cold));
        out.extend_from_slice(&messages[cold_end..]);
        // Key the persisted original under the last cold message id: retrieval is
        // by content-hash (`key`), but the mid anchors cascade-on-session-delete.
        // Tier 1 keys per-message because each tool result is its own retrievable
        // unit; Tier 2 collapses the whole cold block into one stored original.
        let mid = cold.last().map(|m| m.id.clone()).unwrap_or_default();
        Ok(Some(SummaryResult {
            messages: out,
            original: Some((mid, key, source)),
            boundary: cold_end,
            savings: CompactionSavings {
                before_tokens: before,
                after_tokens: after,
                originals_cached: 1,
            },
        }))
    }
}

/// Whether the cold prefix is due for a (re-)summary: always on first sight, and
/// thereafter only once the transcript has grown by `reflush_interval` messages,
/// so a long over-budget turn reuses the cached summary instead of paying for an
/// LLM round-trip every tool round. Mirrors [`crate::flush_due`]'s re-fire policy.
#[must_use]
pub fn summary_due(
    message_count: u64,
    last_summary_count: Option<u64>,
    reflush_interval: u64,
) -> bool {
    match last_summary_count {
        None => true,
        Some(prev) => message_count >= prev.saturating_add(reflush_interval),
    }
}

/// The system prompt that drives the cold-tail condensation.
#[must_use]
pub fn build_summary_prompt() -> String {
    "You are compacting the earlier part of a long conversation to save context. \
Condense the conversation below into a compact summary that preserves: decisions made, \
concrete facts and identifiers, the user's goals and constraints, important tool results, \
and any open questions or unfinished work. Omit pleasantries and redundant detail. Do not \
invent anything that is not present in the conversation. Output only the summary itself -- \
tight prose or bullet points, no preamble."
        .to_string()
}

/// Render the cold transcript into a single plain-text block for the summarizer.
/// Empty-content messages (e.g. an assistant turn that only carried tool calls)
/// are skipped; the tool result that followed carries the substance.
fn render_cold(messages: &[Message]) -> String {
    let mut out = String::new();
    for m in messages {
        if m.content.trim().is_empty() {
            continue;
        }
        out.push_str(role_label(m.role));
        out.push_str(": ");
        out.push_str(&m.content);
        out.push_str("\n\n");
    }
    out
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::Tool => "Tool",
        Role::System => "System",
    }
}

/// Build the synthetic, request-only summary message. Never persisted to the
/// store, so a fixed id and the cold block's session id / first timestamp keep it
/// well-formed without colliding with real messages.
fn summary_message(content: &str, cold: &[Message]) -> Message {
    Message {
        id: "compaction-summary".to_string(),
        session_id: cold
            .first()
            .map(|m| m.session_id.clone())
            .unwrap_or_default(),
        // User (not System): on Anthropic/Bedrock every system-role message is
        // hoisted into the top-level system param regardless of position, which
        // would tear this summary out of its chronological slot before the recent
        // verbatim tail. A user-role message preserves position on all providers.
        role: Role::User,
        content: content.to_string(),
        tool_calls: None,
        tool_call_id: None,
        attachments: None,
        reasoning: None,
        stop_reason: None,
        author_name: None,
        created_at: cold.first().map(|m| m.created_at).unwrap_or(0),
    }
}

/// Stream one provider response and accumulate its assistant text, discarding
/// reasoning and any tool-call fragments (the summarizer is offered no tools).
async fn collect_text(
    provider: &dyn Provider,
    req: ChatRequest,
    cancel: &CancelToken,
) -> Result<String, AgentError> {
    let mut stream = provider.chat_stream(req).await?;
    let mut text = String::new();
    while let Some(item) = stream.next().await {
        if cancel.is_cancelled() {
            break;
        }
        let chunk = item?;
        text.push_str(&chunk.delta);
        if chunk.done {
            break;
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests;
