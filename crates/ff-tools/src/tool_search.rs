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

use crate::registry::{Safety, Tool, ToolOutcome, ToolRegistry};

/// Hard cap on definitions injected per search, independent of the model's `limit`.
///
/// The point of deferral is to keep the tools block small; an unbounded search
/// would let the model undo that in one call. RFC 0024 §5.1 fixes this at 5.
pub const MAX_HITS: usize = 5;

/// Words carrying no retrieval signal in a tool query.
///
/// Without this list a phrase like "why did my build fail" scores tools on
/// "why"/"did"/"my", and because a long description is mechanically more likely
/// to contain a common word, verbose tools surface for queries they have nothing
/// to do with. Measured symptom: a query matched an unrelated tool purely on the
/// phrase "for a list of".
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "did", "do", "does", "for", "from", "get",
    "here", "how", "i", "if", "in", "is", "it", "me", "my", "of", "on", "or", "so", "that", "the",
    "then", "than", "there", "this", "to", "was", "what", "which", "why", "with",
];

/// Query-side vocabulary bridges.
///
/// BM25 can only rank documents containing the query's words, so a query whose
/// vocabulary appears in no tool's text is unreachable by *any* lexical scorer —
/// measured cases include "understand how a function works" and "what did I write
/// today". A table is the cheapest fix available: no dependency, inspectable, and
/// extendable one line at a time as real misses appear.
///
/// Expansion is additive — the original term is always kept — so an entry can
/// only add recall, never remove it.
const SYNONYMS: &[(&str, &[&str])] = &[
    ("understand", &["explore", "explain"]),
    ("explain", &["explore", "understand"]),
    ("journal", &["daily", "note"]),
    ("today", &["daily"]),
    ("yesterday", &["daily"]),
    ("notes", &["note", "vault"]),
    ("docs", &["document", "documentation"]),
    ("doc", &["document"]),
    ("repo", &["repository"]),
    ("repos", &["repository"]),
    ("codebase", &["code", "source", "repository"]),
    ("function", &["symbol", "code"]),
    ("broken", &["failed", "failure"]),
    ("fail", &["failed", "failure"]),
    ("fails", &["failed", "failure"]),
    ("failing", &["failed", "failure"]),
    ("write", &["append", "create"]),
    ("wrote", &["append", "create"]),
    ("make", &["create"]),
    ("delete", &["remove"]),
    ("find", &["search"]),
    ("look", &["search"]),
    ("logs", &["log"]),
    ("tests", &["test"]),
    ("builds", &["build"]),
    ("tickets", &["ticket"]),
    ("deploy", &["deployment"]),
    ("deployments", &["deployment"]),
];

/// Field weights.
///
/// Preserve the original scorer's ordering — a name match beats a description
/// match beats a schema match — but as *frequency* contributions that IDF and
/// length normalisation then damp. That damping is the point of the change:
/// previously a name hit won outright even when the term was too common to mean
/// anything, so every search-shaped query landed on the one tool whose name
/// contains "search".
const W_NAME: f64 = 2.5;
const W_DESC: f64 = 1.0;
const W_SCHEMA: f64 = 0.6;

/// BM25 term-frequency saturation and length-normalisation constants. Standard
/// defaults, deliberately untuned: the corpus is whatever third-party servers
/// happen to expose, so fitting these to one sample would not generalise.
const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

/// Split text into scoreable terms: lowercased alphanumeric runs, with stop words
/// and single characters dropped.
///
/// Every non-alphanumeric character separates, so `daily_append`,
/// `search-tickets` and `daily:append` all yield their parts — a tool's real
/// capability often lives only in a compound name or an `action` enum value.
fn terms_of(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1)
        .map(str::to_lowercase)
        .filter(|t| !STOP_WORDS.contains(&t.as_str()))
        .collect()
}

/// Query terms plus their synonym expansions, deduplicated.
fn expand_query(query: &str) -> Vec<String> {
    let mut terms = terms_of(query);
    let mut extra = Vec::new();
    for term in &terms {
        if let Some((_, synonyms)) = SYNONYMS.iter().find(|(key, _)| key == term) {
            extra.extend(synonyms.iter().map(|s| (*s).to_string()));
        }
    }
    for candidate in extra {
        if !terms.contains(&candidate) {
            terms.push(candidate);
        }
    }
    terms
}

/// Build a tool's field-weighted term bag and its weighted length.
fn build_bag(name: &str, description: &str, schema_keywords: &str) -> (HashMap<String, f64>, f64) {
    let mut bag: HashMap<String, f64> = HashMap::new();
    for (text, weight) in [
        (name, W_NAME),
        (description, W_DESC),
        (schema_keywords, W_SCHEMA),
    ] {
        for term in terms_of(text) {
            *bag.entry(term).or_default() += weight;
        }
    }
    let len = bag.values().sum();
    (bag, len)
}

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
    /// Field-weighted term frequencies, precomputed at index time so a query
    /// never re-tokenizes the corpus. This is the BM25F pseudo-document: one bag
    /// whose frequencies already carry the name > description > schema preference
    /// that IDF and length normalisation then damp.
    bag: HashMap<String, f64>,
    /// Sum of `bag`'s weights — this document's length for normalisation.
    len: f64,
}

/// The searchable corpus of deferred tools.
#[derive(Debug, Clone, Default)]
pub struct ToolSearchIndex {
    tools: Vec<IndexedTool>,
    /// Per-term document frequency across the corpus, for IDF.
    doc_freq: HashMap<String, usize>,
    /// Mean weighted document length, for length normalisation.
    avg_len: f64,
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
            .map(|t| {
                let name = t.name().to_string();
                let description = t.description().to_lowercase();
                let schema_keywords = schema_keywords(&t.parameters());
                let (bag, len) = build_bag(&name, &description, &schema_keywords);
                IndexedTool {
                    name,
                    description,
                    summary: first_sentence(t.description()),
                    schema_keywords,
                    bag,
                    len,
                }
            })
            .collect();
        Self::from_tools(tools)
    }

    /// Finish an index by deriving the corpus-wide statistics BM25F needs.
    fn from_tools(tools: Vec<IndexedTool>) -> Self {
        let mut doc_freq: HashMap<String, usize> = HashMap::new();
        for tool in &tools {
            for term in tool.bag.keys() {
                *doc_freq.entry(term.clone()).or_default() += 1;
            }
        }
        let avg_len = if tools.is_empty() {
            0.0
        } else {
            tools.iter().map(|t| t.len).sum::<f64>() / tools.len() as f64
        };
        Self {
            tools,
            doc_freq,
            avg_len,
        }
    }

    /// BM25F relevance of one tool to already-expanded query terms. `0.0` means no
    /// term matched.
    fn bm25f(&self, tool: &IndexedTool, terms: &[String]) -> f64 {
        if terms.is_empty() || self.tools.is_empty() {
            return 0.0;
        }
        let corpus = self.tools.len() as f64;
        let mut score = 0.0;
        for term in terms {
            let freq = tool.bag.get(term).copied().unwrap_or(0.0);
            if freq <= 0.0 {
                continue;
            }
            let df = self.doc_freq.get(term).copied().unwrap_or(1) as f64;
            // Probabilistic IDF, +1 smoothed so a term present in every document
            // contributes a small positive weight rather than zero or negative.
            let idf = (1.0 + (corpus - df + 0.5) / (df + 0.5)).ln();
            let norm =
                BM25_K1 * (1.0 - BM25_B + BM25_B * tool.len / self.avg_len.max(f64::EPSILON));
            score += idf * (freq * (BM25_K1 + 1.0)) / (freq + norm);
        }
        score
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
    score: f64,
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
    ///
    /// Two recall paths, in order:
    ///
    /// 1. **BM25F** — the primary ranking. IDF stops a term appearing in nearly
    ///    every tool from deciding the winner, and length normalisation stops a
    ///    verbose description from out-scoring a precise one.
    /// 2. **Substring fallback**, run *only* when BM25F matched nothing at all.
    ///
    /// The fallback is a floor, not a tie-breaker. BM25F needs a whole-token
    /// match, so a query whose vocabulary is absent from every tool scores zero
    /// everywhere and would return an empty list — and empty is the one outcome
    /// worse than imprecise, because a wrong hit is visible to the model and
    /// prompts another search whereas nothing at all invites it to give up on a
    /// capability that is in fact available. That is the "invisible tool" failure
    /// this whole mechanism exists to avoid.
    fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let terms = expand_query(query);

        let mut hits: Vec<Hit> = self
            .index
            .tools
            .iter()
            .filter_map(|t| {
                let score = self.index.bm25f(t, &terms);
                (score > 0.0).then(|| Hit {
                    name: t.name.clone(),
                    description: t.summary.clone(),
                    score,
                })
            })
            .collect();

        if hits.is_empty() {
            hits = self
                .index
                .tools
                .iter()
                .filter_map(|t| {
                    let score = score_indexed(t, &terms);
                    (score > 0).then(|| Hit {
                        name: t.name.clone(),
                        description: t.summary.clone(),
                        score: f64::from(score),
                    })
                })
                .collect();
        }

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });
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
///
/// The cap is byte-based but descriptions are arbitrary third-party MCP metadata,
/// so a fixed 160 can land inside a multi-byte codepoint — slicing `&str` there
/// panics. Walk `char_indices` to the last boundary at or before the cap instead.
fn first_sentence(desc: &str) -> String {
    let flat = desc.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = match flat.find(". ") {
        // The byte after '.' is a boundary, since '.' is single-byte ASCII.
        Some(i) => i + 1,
        None => floor_char_boundary(&flat, 160),
    };
    let mut s = flat[..cut].to_string();
    if s.len() < flat.len() {
        s.push('…');
    }
    s
}

/// Largest index `<= max` that is a char boundary of `s` (clamped to `s.len()`).
///
/// `str::floor_char_boundary` is still unstable, so derive it from `char_indices`.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    s.char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= max)
        .last()
        .unwrap_or(0)
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

    /// Scoring reads an in-memory index snapshot and admitting a name mutates only
    /// this session's admitted set — harness bookkeeping, never the workspace, the
    /// filesystem, or a remote. That holds for every argument shape, so the ceiling
    /// is `ReadOnly` and `min_safety` inherits it (floor == ceiling).
    ///
    /// This is load-bearing, not cosmetic: `tool_search` is the *only* gateway to
    /// the deferred registry, so a `Write` ceiling would strip it from Plan (the
    /// Plan matrix Denies `Write`) and make every deferred tool permanently
    /// unreachable there.
    fn safety(&self, _args: &Value) -> Safety {
        Safety::ReadOnly
    }

    fn max_safety(&self) -> Safety {
        Safety::ReadOnly
    }

    /// Searching consults a local index only. Without this the `true` fail-safe
    /// default would drop `tool_search` under a `LocalOnly` phenotype, again
    /// sealing off every deferred tool.
    fn reaches_network(&self) -> bool {
        false
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
                "No tools matched \"{query}\". {total} deferred tool{plural} remain unloaded — \
                 this does NOT mean the capability is missing, only that these words did not \
                 match. Retrieval is lexical, so it needs the vocabulary the tool itself uses. \
                 Search again with a synonym, the underlying concept, or a broader single word \
                 (e.g. \"deployment\" rather than \"roll back a bad release\").",
                total = self.index.len(),
                plural = if self.index.len() == 1 { "" } else { "s" },
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
mod retrieval_tests;
#[cfg(test)]
mod tests;
