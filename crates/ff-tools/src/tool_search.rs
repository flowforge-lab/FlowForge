//! Just-in-time tool discovery (RFC 0024 Layer 1).
//!
//! Every advertised tool's full definition sits in the provider's cached prompt
//! prefix on every turn. Measured on a live workstation, 77 MCP-bridged tools cost
//! 33,890 tokens — 81% of a ~41,400-token standing block — spent keeping tools on
//! standby before any work happens. Past ~30-50 simultaneously-offered tools,
//! selection accuracy degrades too.
//!
//! So tools that opt into [`Tool::defer`](crate::Tool::defer) are kept *out* of the
//! standing block and reached through the [`ToolSearchTool`] meta-tool instead: the
//! model searches, and the hits are re-admitted into that session's allow-set for
//! the rest of the turn.
//!
//! Ranking is lexical, pure and deterministic — no embeddings and no I/O. It mirrors
//! [`ff_skills::search_skills`]'s coarse scoring bands, with one deliberate
//! difference: tool queries are natural-language phrases ("run something in the
//! background") rather than single terms, and tool descriptions are long, so scoring
//! is **per-query-term** and summed. Whole-string matching would miss almost
//! everything. Semantic and action-level retrieval are RFC 0024 Phase 2.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::registry::{Tool, ToolOutcome, ToolRegistry};

/// Hard cap on definitions injected per search, independent of the model's `limit`.
///
/// The point of deferral is to keep the tools block small; an unbounded search
/// would let the model undo that in one call. RFC 0024 §5.1 fixes this at 5.
pub const MAX_HITS: usize = 5;

/// Per-session set of deferred tools re-admitted by [`ToolSearchTool`].
///
/// Lives in `ff-tools` rather than `ff-agent` because the dependency runs
/// `ff-agent -> ff-tools`: the tool writes to it and the agent's advertise pass
/// reads it, so it has to sit at or below the lower crate.
///
/// Keyed by session id — the same isolation the `observer` supervisor uses — so a
/// search in one pane never widens another pane's tool surface.
///
/// The set only ever grows within a session: a tool the model found once stays
/// reachable for the rest of the session. That is deliberate, and it is what keeps
/// the prompt prefix append-only (RFC 0024 §6); revoking mid-session would reorder
/// the block and cost a full re-prefill.
#[derive(Debug, Default)]
pub struct ToolSearchState {
    admitted: Mutex<HashMap<String, HashSet<String>>>,
}

impl ToolSearchState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tools `session_id` has unlocked so far. Empty for an unknown session.
    pub fn admitted(&self, session_id: &str) -> HashSet<String> {
        self.admitted
            .lock()
            .map(|m| m.get(session_id).cloned().unwrap_or_default())
            .unwrap_or_default()
    }

    /// Re-admit `names` for `session_id`. Idempotent.
    pub fn admit(&self, session_id: &str, names: impl IntoIterator<Item = String>) {
        if let Ok(mut m) = self.admitted.lock() {
            m.entry(session_id.to_string()).or_default().extend(names);
        }
    }

    /// Whether `session_id` has unlocked anything yet. Lets the caller skip
    /// rebuilding the schema list when nothing has changed.
    pub fn is_empty(&self, session_id: &str) -> bool {
        self.admitted
            .lock()
            .map(|m| m.get(session_id).is_none_or(HashSet::is_empty))
            .unwrap_or(true)
    }
}

/// One deferred tool's searchable text, captured when the index is built.
///
/// A snapshot rather than a live registry reference, for two reasons. It breaks a
/// circularity — `tool_search` is itself registered in the registry it would
/// otherwise have to borrow — and searching only ever needs a tool's *static*
/// metadata (name, description, schema keywords), so there is nothing to keep live.
#[derive(Debug, Clone)]
pub struct IndexedTool {
    name: String,
    description: String,
    summary: String,
    schema_keywords: String,
}

/// The searchable corpus of deferred tools.
#[derive(Debug, Clone, Default)]
pub struct ToolSearchIndex {
    tools: Vec<IndexedTool>,
}

impl ToolSearchIndex {
    /// Snapshot every tool in `registry` that opts into deferral.
    ///
    /// Built once per turn alongside the registry, so a server connecting or
    /// disconnecting mid-turn cannot change the corpus underneath a search — the same
    /// snapshot discipline the registry itself uses.
    pub fn from_registry(registry: &ToolRegistry) -> Self {
        let tools = registry
            .iter_tools()
            .filter(|t| t.defer())
            .map(|t| IndexedTool {
                name: t.name().to_string(),
                description: t.description().to_lowercase(),
                summary: first_sentence(t.description()),
                schema_keywords: schema_keywords(&t.parameters()),
            })
            .collect();
        Self { tools }
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }
}

/// One ranked search result.
struct Hit {
    name: String,
    description: String,
    score: u32,
}

/// The `tool_search` meta-tool: the only way a deferred tool becomes reachable.
///
/// Holds an `Arc` of the shared allow-set and dispatches through
/// [`Tool::run_with_session`] so hits are scoped to the calling session — the same
/// shape the `observer` tool uses for its supervisor.
pub struct ToolSearchTool {
    state: Arc<ToolSearchState>,
    index: ToolSearchIndex,
}

impl ToolSearchTool {
    pub fn new(state: Arc<ToolSearchState>, index: ToolSearchIndex) -> Self {
        Self { state, index }
    }

    /// Rank the deferred corpus against `query`, best first.
    fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let terms: Vec<String> = query
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|t| t.len() > 1)
            .map(str::to_string)
            .collect();

        let mut hits: Vec<Hit> = self
            .index
            .tools
            .iter()
            .filter_map(|t| {
                let score = score_indexed(t, &terms);
                (score > 0).then(|| Hit {
                    name: t.name.clone(),
                    description: t.summary.clone(),
                    score,
                })
            })
            .collect();
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.name.cmp(&b.name)));
        hits.truncate(limit.min(MAX_HITS));
        hits
    }
}

/// Relevance of one tool to the query's terms. `0` means no match.
///
/// Scored per term and summed, so a multi-word phrase concentrates on the tool
/// matching the most of it. The bands mirror `ff_skills::search_skills`: a name hit
/// beats a description hit, which beats a schema hit. Schema text is included at the
/// lowest weight because a polymorphic tool's real capability often lives only in
/// its `action` enum, not its prose — the single biggest recall gap in Phase 1.
fn score_indexed(tool: &IndexedTool, terms: &[String]) -> u32 {
    score_text(&tool.name, &tool.description, &tool.schema_keywords, terms)
}

/// Relevance of one tool's text to the query's terms. `0` means no match.
///
/// `desc_l` and `schema_l` are expected pre-lowercased (the index stores them that
/// way); `name` is lowercased here since it is also used for exact comparison.
fn score_text(name: &str, desc_l: &str, schema_l: &str, terms: &[String]) -> u32 {
    if terms.is_empty() {
        return 0;
    }
    let name_l = name.to_lowercase();
    terms
        .iter()
        .map(|t| {
            if name_l == *t {
                8
            } else if name_l.contains(t.as_str()) {
                4
            } else if desc_l.contains(t.as_str()) {
                2
            } else if schema_l.contains(t.as_str()) {
                1
            } else {
                0
            }
        })
        .sum()
}

/// Enum values and property names from a parameter schema, lowercased.
///
/// Only these carry discovery signal; types and prose descriptions inside the schema
/// add noise at this granularity. Indexing the `action`/`kind` enums is what lets a
/// query hit one action of a many-action tool.
fn schema_keywords(schema: &Value) -> String {
    let mut out = String::new();
    collect_keywords(schema, &mut out, 0);
    out
}

fn collect_keywords(v: &Value, out: &mut String, depth: usize) {
    if depth > 6 {
        return;
    }
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                if k == "enum" {
                    if let Some(items) = val.as_array() {
                        for i in items.iter().filter_map(Value::as_str) {
                            out.push_str(&i.to_lowercase());
                            out.push(' ');
                        }
                    }
                } else if k == "properties" {
                    if let Some(props) = val.as_object() {
                        for pk in props.keys() {
                            out.push_str(&pk.to_lowercase());
                            out.push(' ');
                        }
                    }
                }
                collect_keywords(val, out, depth + 1);
            }
        }
        Value::Array(items) => {
            for i in items {
                collect_keywords(i, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// First sentence of `desc`, capped, for a one-line result summary.
fn first_sentence(desc: &str) -> String {
    let flat = desc.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = flat
        .find(". ")
        .map(|i| i + 1)
        .unwrap_or_else(|| flat.len().min(160));
    let mut s = flat[..cut.min(flat.len())].to_string();
    if s.len() < flat.len() {
        s.push('…');
    }
    s
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        "tool_search"
    }

    fn description(&self) -> &str {
        "Find and load tools that are not currently listed. Your visible tool list is \
         deliberately small; additional tools are available on demand and must be \
         searched for before you can call them. Search whenever a task needs a \
         capability you cannot see a tool for — external or third-party systems, \
         issue trackers and ticketing, deployment and build pipelines, code search \
         across other repositories, documentation and internal knowledge lookup, \
         notes and knowledge bases, on-call and monitoring, or any integration \
         specific to this workspace. Do NOT assume a capability is unavailable just \
         because it is absent from your tool list — search first. Describe the task \
         in words (\"file a ticket\", \"check pipeline status\", \"search my notes\"); \
         matching tools are added to your available set and stay callable for the \
         rest of the session."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "What you need to do, in words. Natural-language \
                                    phrases work better than single keywords."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max tools to load (default 5, hard cap 5)."
                }
            },
            "required": ["query"]
        })
    }

    async fn run(&self, args: Value, root: &Path) -> ToolOutcome {
        self.run_with_session(args, root, crate::registry::NO_SESSION)
            .await
    }

    async fn run_with_session(&self, args: Value, _root: &Path, session_id: &str) -> ToolOutcome {
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolOutcome::error("tool_search: `query` is required");
        };
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(MAX_HITS);

        let hits = self.search(query, limit);
        if hits.is_empty() {
            return ToolOutcome::ok(format!(
                "No additional tools matched \"{query}\". Try different words, or \
                 proceed with the tools you already have."
            ));
        }

        self.state
            .admit(session_id, hits.iter().map(|h| h.name.clone()));

        let mut out = format!(
            "Loaded {} tool{} — callable now, and for the rest of this session:\n",
            hits.len(),
            if hits.len() == 1 { "" } else { "s" }
        );
        for h in &hits {
            out.push_str(&format!("\n- `{}` — {}", h.name, h.description));
        }
        ToolOutcome::ok(out)
    }
}

#[cfg(test)]
mod tests;
