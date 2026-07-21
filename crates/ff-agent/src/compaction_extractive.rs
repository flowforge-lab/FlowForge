//! Tier-1 extractive, content-aware, reversible compaction (RFC 0016 §4 Tier 1).
//!
//! **Spike (M7.1).** This module is the *pure mechanism* of Tier-1 compaction --
//! content classification + deterministic extractive compression + a reversible
//! cache that lets a future retrieve tool restore the original. It is not yet
//! wired into [`run_turn`](crate::run_turn); the host wiring evolves
//! `CompactionOutcome` to express "compacted N messages, saved K tokens" and is
//! tracked for M7.1 (RFC 0016 §9 Open Questions). Until then this is a tested,
//! default-unused building block: no live behavior changes.
//!
//! Why a separate, deterministic spike: Tier-1's value is "same answers, fewer
//! tokens" with no LLM call at the hot path -- a content-routed, structurally
//! aware compressor whose output is immediately retrievable. Building the
//! mechanism as pure functions lets it be unit-tested end-to-end without a
//! provider, and lets the host integrate it later behind the existing
//! [`CompactionStrategy`](crate::compaction::CompactionStrategy) seam.
//!
//! ## Pieces
//! - [`ContentKind`] / [`classify`] -- detect JSON, code, or prose from the text.
//! - [`ExtractiveCompactor`] -- per-kind deterministic compression (JSON value
//!   truncation + array elision; code/prose head+tail line elision). Only emits
//!   a compressed form when it actually shrinks the text.
//! - [`ReversibleCache`] -- maps a content-hash key to the original; the
//!   compressed text carries the key in a trailing marker so a future
//!   `compaction_retrieve` tool can fetch the original on demand.
//! - [`CompactionSavings`] -- proxy-token before/after, matching
//!   [`ProxyTokenEstimator`](crate::compaction::ProxyTokenEstimator).
//!
//! ## Reversibility contract
//! Compaction is lossy *in context* but lossless *on demand*. Every shrunk blob
//! pushes its full original into the cache and the compressed text ends with a
//! `[compacted; retrieve key=...]` marker. `cache.retrieve(key)` returns the
//! exact original byte-for-byte. Content that does not shrink (small messages,
//! already-compact JSON) is returned unchanged with no cache entry -- the cache
//! holds only what the model might need to pull back.

use ff_core::{Message, Role};
use ff_memory::Salience;
use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Proxy-token estimator that mirrors
/// [`ProxyTokenEstimator`](crate::compaction::ProxyTokenEstimator) (chars / 4).
/// One source of truth for the estimate keeps savings reports comparable to the
/// pressure trigger that motivates them.
#[must_use]
pub fn proxy_tokens(s: &str) -> usize {
    s.len() / 4
}

/// Detected content kind. Drives the per-kind compressor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    /// Parses as a JSON object or array (typical tool output).
    Json,
    /// Looks like source code (fenced block or strong code-shape signals).
    Code,
    /// Everything else -- treated as prose.
    Prose,
}

/// Best-effort classification. Cheap and conservative: ambiguous content falls
/// back to [`ContentKind::Prose`] so the line-elision compressor handles it.
#[must_use]
pub fn classify(content: &str) -> ContentKind {
    let trimmed = content.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return ContentKind::Json;
    }
    // Fenced code block, or two strong code signals (semicolon line endings,
    // function/class keywords, indented braces). Conservative: a prose blob with
    // an incidental brace must not be misclassified.
    if content.contains("```") || code_signal_count(content) >= 2 {
        return ContentKind::Code;
    }
    ContentKind::Prose
}

fn code_signal_count(s: &str) -> usize {
    let mut n = 0;
    if s.lines().filter(|l| l.trim_end().ends_with(';')).count() >= 3 {
        n += 1;
    }
    if s.contains("fn ") || s.contains("def ") || s.contains("class ") || s.contains("function ") {
        n += 1;
    }
    if s.contains("{\n") && s.contains("}\n") {
        n += 1;
    }
    n
}

/// Reversible store of pre-compaction originals, keyed by a stable content hash.
/// A future `compaction_retrieve` tool reads this map; the spike exposes it as a
/// pure API so the wiring is a separate concern.
#[derive(Default, Debug)]
pub struct ReversibleCache {
    map: BTreeMap<String, String>,
}

impl ReversibleCache {
    /// New empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `original` and return the retrieve key. Idempotent: identical
    /// originals share one key (deduped via the content hash).
    pub fn put(&mut self, original: String) -> String {
        let key = content_key(&original);
        self.map.entry(key.clone()).or_insert(original);
        key
    }

    /// Look up the original by retrieve key.
    #[must_use]
    pub fn retrieve(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    /// How many distinct originals are cached.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Stable content-hash key for a blob, as carried in the trailing
/// `[compacted; retrieve key=...]` marker. Shared with the Tier-2 abstractive
/// path so both tiers key their persisted originals identically.
pub(crate) fn content_key(original: &str) -> String {
    let mut h = DefaultHasher::new();
    original.hash(&mut h);
    // 16 hex chars are plenty to disambiguate a per-session cache and stay
    // human-readable in the trailing marker.
    format!("{:016x}", h.finish())
}

/// The literal prefix every compaction marker carries. Used to detect content
/// that was already compacted (e.g. tool results compacted at ingest in M7.1a)
/// so the cold-prefix pass does not double-compact it.
pub const COMPACTION_MARKER_PREFIX: &str = "[compacted; retrieve key=";

/// Result of compressing a single blob in a storage-agnostic way.
/// `text` is what should be sent to the model; `original` carries the
/// `(key, original)` the caller must persist (e.g. to a DB) to make the
/// compaction reversible. `original` is `None` when nothing shrank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressOutcome {
    /// The compressed (or, when nothing shrank, unchanged) text.
    pub text: String,
    /// `Some((key, original))` when the input shrank and the verbatim original
    /// must be persisted under `key` for the `compaction_retrieve` tool; `None` otherwise.
    pub original: Option<(String, String)>,
}

/// Token savings from a compaction pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionSavings {
    /// Sum of [`proxy_tokens`] across the inputs.
    pub before_tokens: usize,
    /// Sum of [`proxy_tokens`] across the outputs.
    pub after_tokens: usize,
    /// How many distinct originals the cache now holds (i.e. how many blobs
    /// actually shrank). Blobs that were already minimal stay verbatim and are
    /// not cached -- the cache holds only what may need retrieval.
    pub originals_cached: usize,
}

impl CompactionSavings {
    /// Tokens saved (`before - after`), saturating at zero.
    #[must_use]
    pub fn saved(&self) -> usize {
        self.before_tokens.saturating_sub(self.after_tokens)
    }

    /// Saved fraction of the input (0.0 when nothing was saved or input was empty).
    #[must_use]
    pub fn ratio(&self) -> f64 {
        if self.before_tokens == 0 {
            0.0
        } else {
            self.saved() as f64 / self.before_tokens as f64
        }
    }
}

/// Outcome of a storage-agnostic cold-prefix compaction over a transcript.
/// `messages` is the wire-ready transcript (recent messages verbatim, cold
/// messages compacted); `originals` carries `(message_id, key, original)` for
/// every blob that actually shrank, which the caller must persist to make the
/// compaction reversible via the `compaction_retrieve` tool.
#[derive(Debug, Clone)]
pub struct ColdCompaction {
    /// The wire-ready transcript to send to the model.
    pub messages: Vec<Message>,
    /// `(message_id, key, original)` for each blob that shrank and must be
    /// persisted so the verbatim original can be retrieved later.
    pub originals: Vec<(String, String, String)>,
    /// Proxy-token savings for this pass.
    pub savings: CompactionSavings,
}

/// Configuration for [`ExtractiveCompactor`]. Defaults are tuned conservatively:
/// the spike must shrink large blobs decisively, but should leave small ones
/// alone so the cache stays focused on content the model might want back.
#[derive(Debug, Clone, Copy)]
pub struct ExtractiveCompactor {
    /// JSON: truncate string values longer than this.
    pub max_value_chars: usize,
    /// JSON: keep at most this many array items; the rest are elided.
    pub max_array_items: usize,
    /// Line elision: keep this many leading lines.
    pub keep_head_lines: usize,
    /// Line elision: keep this many trailing lines.
    pub keep_tail_lines: usize,
    /// Don't elide blobs shorter than this (in lines).
    pub min_lines_to_elide: usize,
    /// Don't compact blobs smaller than this (in proxy tokens). Avoids cache
    /// churn for tiny messages where the marker would erase the win.
    pub min_tokens_to_compact: usize,
}

impl Default for ExtractiveCompactor {
    fn default() -> Self {
        Self {
            max_value_chars: 256,
            max_array_items: 8,
            keep_head_lines: 4,
            keep_tail_lines: 4,
            min_lines_to_elide: 12,
            min_tokens_to_compact: 64,
        }
    }
}

impl ExtractiveCompactor {
    /// Compress one content blob. If the result actually shrinks the input *and*
    /// the input is above [`Self::min_tokens_to_compact`], cache the original
    /// and return the compressed text with a trailing retrieve marker. Otherwise
    /// return the input unchanged and leave the cache untouched.
    #[must_use]
    pub fn compress(&self, content: &str, cache: &mut ReversibleCache) -> String {
        let outcome = self.compress_one(content);
        if let Some((_, original)) = &outcome.original {
            let _ = cache.put(original.clone());
        }
        outcome.text
    }

    /// Storage-agnostic compression. Returns the compressed text and, when the
    /// input actually shrank, the `(key, original)` the caller must persist to
    /// make the result retrievable. `original` is `None` when the input was
    /// below `min_tokens_to_compact` or would not shrink -- in that case
    /// `text` is the input unchanged and nothing needs to be stored. The key is
    /// the same content hash [`ReversibleCache`] uses, so a marker emitted here
    /// resolves against either an in-memory cache or a persistent store.
    #[must_use]
    pub fn compress_one(&self, content: &str) -> CompressOutcome {
        if proxy_tokens(content) < self.min_tokens_to_compact {
            return CompressOutcome {
                text: content.to_string(),
                original: None,
            };
        }
        let compressed = match classify(content) {
            ContentKind::Json => self.compress_json(content),
            ContentKind::Code | ContentKind::Prose => self.compress_lines(content),
        };
        // Only mark (and ask the caller to persist) when the compression
        // actually shrank the text by enough to amortize the trailing marker.
        let key = content_key(content);
        let with_marker = format!(
            "{compressed}\n[compacted; retrieve key={key}]",
            compressed = compressed.trim_end()
        );
        if proxy_tokens(&with_marker) < proxy_tokens(content) {
            CompressOutcome {
                text: with_marker,
                original: Some((key, content.to_string())),
            }
        } else {
            CompressOutcome {
                text: content.to_string(),
                original: None,
            }
        }
    }

    /// Compact the cold prefix of a transcript, leaving the most recent
    /// `keep_recent` messages verbatim. Recent messages stay byte-identical
    /// (the model needs exact recent state); older messages are routed through
    /// [`Self::compress`].
    #[must_use]
    pub fn compact_cold(
        &self,
        messages: &[Message],
        keep_recent: usize,
        cache: &mut ReversibleCache,
    ) -> (Vec<Message>, CompactionSavings) {
        let n = messages.len();
        let cold_end = n.saturating_sub(keep_recent);
        let cache_len_before = cache.len();
        let mut before = 0usize;
        let mut after = 0usize;
        let mut out = Vec::with_capacity(n);
        for (i, m) in messages.iter().enumerate() {
            before += proxy_tokens(&m.content);
            if i < cold_end && m.role != Role::Assistant {
                let new_content = self.compress(&m.content, cache);
                after += proxy_tokens(&new_content);
                let mut clone = m.clone();
                clone.content = new_content;
                out.push(clone);
            } else {
                after += proxy_tokens(&m.content);
                out.push(m.clone());
            }
        }
        (
            out,
            CompactionSavings {
                before_tokens: before,
                after_tokens: after,
                originals_cached: cache.len() - cache_len_before,
            },
        )
    }

    /// Compact the cold prefix of a transcript in a storage-agnostic way,
    /// leaving the most recent `keep_recent` messages byte-identical. Unlike
    /// [`Self::compact_cold`], this collects the `(message_id, key, original)`
    /// triples the caller must persist (rather than mutating an in-memory
    /// cache), so it can be wired directly to a durable store.
    ///
    /// Messages whose content already carries a [`COMPACTION_MARKER_PREFIX`]
    /// (e.g. tool results compacted at ingest) are passed through untouched to
    /// avoid double-compaction.
    #[must_use]
    pub fn compact_cold_collect(&self, messages: &[Message], keep_recent: usize) -> ColdCompaction {
        let n = messages.len();
        let cold_end = n.saturating_sub(keep_recent);
        let mut before = 0usize;
        let mut after = 0usize;
        let mut out = Vec::with_capacity(n);
        let mut originals = Vec::new();
        for (i, m) in messages.iter().enumerate() {
            before += proxy_tokens(&m.content);
            if i < cold_end
                && m.role != Role::Assistant
                && !m.content.contains(COMPACTION_MARKER_PREFIX)
            {
                let outcome = self.compress_one(&m.content);
                after += proxy_tokens(&outcome.text);
                if let Some((key, original)) = outcome.original {
                    originals.push((m.id.clone(), key, original));
                }
                let mut clone = m.clone();
                clone.content = outcome.text;
                out.push(clone);
            } else {
                after += proxy_tokens(&m.content);
                out.push(m.clone());
            }
        }
        let originals_cached = originals.len();
        ColdCompaction {
            messages: out,
            originals,
            savings: CompactionSavings {
                before_tokens: before,
                after_tokens: after,
                originals_cached,
            },
        }
    }

    fn compress_json(&self, content: &str) -> String {
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(content.trim_start()) else {
            return self.compress_lines(content);
        };
        truncate_value(&mut value, self.max_value_chars, self.max_array_items);
        // Compact serialization (no whitespace) is the headline JSON token win.
        serde_json::to_string(&value).unwrap_or_else(|_| content.to_string())
    }

    fn compress_lines(&self, content: &str) -> String {
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() < self.min_lines_to_elide
            || lines.len() <= self.keep_head_lines + self.keep_tail_lines
        {
            return content.to_string();
        }
        let elided = lines.len() - self.keep_head_lines - self.keep_tail_lines;
        let mut out = String::new();
        for line in &lines[..self.keep_head_lines] {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(&format!("<compacted lines=\"{elided}\"/>\n"));
        for line in &lines[lines.len() - self.keep_tail_lines..] {
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    /// Compact **only tool-result messages**, unconditionally (independent of any
    /// pressure gate), leaving the most recent `keep_recent` messages verbatim.
    /// This is the #933 B.2 ingest-time pass: big tool-result blobs
    /// (`codegraph_explore`, diffs) are shrunk the moment they enter the wire so a
    /// session stays under budget longer and the lossy Tier-2 summarizer engages
    /// later — while the compacted bytes are stable (written once), so they enter
    /// the provider's KV cache instead of being rewritten under pressure by Tier-1.
    ///
    /// Length- and order-preserving (one output per input, same ids/roles), so the
    /// caller's count-based bookkeeping is unaffected. Non-tool messages and the
    /// recent tail pass through untouched; already-compacted content
    /// ([`COMPACTION_MARKER_PREFIX`]) is skipped so the later Tier-1/Tier-2 passes
    /// never double-compact it. Returns the `(message_id, key, original)` triples
    /// the caller must persist for `compaction_retrieve`.
    #[must_use]
    pub fn compact_tool_results_collect(
        &self,
        messages: &[Message],
        keep_recent: usize,
    ) -> ColdCompaction {
        let n = messages.len();
        let cold_end = n.saturating_sub(keep_recent);
        let mut before = 0usize;
        let mut after = 0usize;
        let mut out = Vec::with_capacity(n);
        let mut originals = Vec::new();
        for (i, m) in messages.iter().enumerate() {
            before += proxy_tokens(&m.content);
            if i < cold_end && m.role == Role::Tool && !m.content.contains(COMPACTION_MARKER_PREFIX)
            {
                let outcome = self.compress_one(&m.content);
                after += proxy_tokens(&outcome.text);
                if let Some((key, original)) = outcome.original {
                    originals.push((m.id.clone(), key, original));
                }
                let mut clone = m.clone();
                clone.content = outcome.text;
                out.push(clone);
            } else {
                after += proxy_tokens(&m.content);
                out.push(m.clone());
            }
        }
        let originals_cached = originals.len();
        ColdCompaction {
            messages: out,
            originals,
            savings: CompactionSavings {
                before_tokens: before,
                after_tokens: after,
                originals_cached,
            },
        }
    }

    /// Compact an arbitrary slice of messages — all are treated as cold (eligible
    /// for compression). Used by the frozen-boundary tier-1 path (#933 A.2) to
    /// compress only *newly cold* messages beyond the cached boundary, without
    /// reprocessing the already-frozen prefix.
    ///
    /// Returns a `ColdCompaction` with the compressed messages and any originals
    /// that must be persisted. Semantics are identical to `compact_cold_collect`
    /// applied to this slice with `keep_recent = 0`.
    #[must_use]
    pub fn compact_range_collect(&self, messages: &[Message]) -> ColdCompaction {
        let mut before = 0usize;
        let mut after = 0usize;
        let mut out = Vec::with_capacity(messages.len());
        let mut originals = Vec::new();
        for m in messages {
            before += proxy_tokens(&m.content);
            if m.role != Role::Assistant && !m.content.contains(COMPACTION_MARKER_PREFIX) {
                let outcome = self.compress_one(&m.content);
                after += proxy_tokens(&outcome.text);
                if let Some((key, original)) = outcome.original {
                    originals.push((m.id.clone(), key, original));
                }
                let mut clone = m.clone();
                clone.content = outcome.text;
                out.push(clone);
            } else {
                after += proxy_tokens(&m.content);
                out.push(m.clone());
            }
        }
        let originals_cached = originals.len();
        ColdCompaction {
            messages: out,
            originals,
            savings: CompactionSavings {
                before_tokens: before,
                after_tokens: after,
                originals_cached,
            },
        }
    }
}

/// Maximum target-seeking deepening level (#989). Beyond this the extractive ladder
/// has done all it can and the caller falls through to the Tier-2 fallback. Small:
/// each level roughly halves the per-message caps, so 4 levels shrink caps ~16×.
pub const MAX_COMPACTION_LEVEL: usize = 4;

/// Deepen a compactor by `level` for target-seeking (#989): each level shrinks the
/// per-message caps toward a hard floor, monotonically (a higher level never yields
/// more tokens). `level == 0` returns the input unchanged (the non-regression
/// anchor). Halving with a floor keeps the transform stable and reversible — the
/// markers/originals machinery is untouched, only the caps tighten.
#[must_use]
fn deepen(c: ExtractiveCompactor, level: usize) -> ExtractiveCompactor {
    if level == 0 {
        return c;
    }
    // Halve per level, floored so a band never collapses to nothing.
    let shrink = |v: usize, floor: usize| (v >> level).max(floor);
    ExtractiveCompactor {
        max_value_chars: shrink(c.max_value_chars, 16),
        max_array_items: shrink(c.max_array_items, 1),
        keep_head_lines: shrink(c.keep_head_lines, 1),
        keep_tail_lines: shrink(c.keep_tail_lines, 1),
        // Elide sooner at deeper levels, but never below a couple of lines.
        min_lines_to_elide: shrink(c.min_lines_to_elide, 3),
        // Compact smaller blobs too as we deepen (floor keeps tiny content alone).
        min_tokens_to_compact: shrink(c.min_tokens_to_compact, 16),
    }
}

/// Depth-graded tier-1 compaction (#933 / RFC 0022 Step 1): older messages are
/// compressed harder, newer ones gently, instead of one uniform strength across
/// the whole cold prefix.
///
/// **Band membership is a pure function of a message's absolute index**, not its
/// distance from the moving keep-recent frontier. This is the invariant that keeps
/// the A.2 frozen-boundary optimization intact: because `compactor_for(index)`
/// depends only on the index, a full compaction pass and an incremental
/// (freeze-the-prefix, compact-only-the-new-slice) pass select the *same* compactor
/// for the same message, so both produce byte-identical output and the cached
/// prefix never drifts. Grading by distance-from-frontier would make the two paths
/// disagree and bust the cache every turn.
///
/// The **last (highest-index) band equals the plain [`ExtractiveCompactor::default`]**,
/// so newest-cold messages compress exactly as they do today — the change is a
/// strict non-regression, only deepening compression for older messages.
#[derive(Debug, Clone)]
pub struct GradedBands {
    /// `(start_index, compactor)` sorted ascending by `start_index`. A message at
    /// index `i` uses the compactor of the band with the greatest `start_index <= i`.
    /// The first band must start at 0 so every index is covered.
    bands: Vec<(usize, ExtractiveCompactor)>,
}

impl Default for GradedBands {
    /// A single band == today's behavior. This is the equivalence anchor: with the
    /// default, `compact_graded_collect` is byte-identical to `compact_cold_collect`,
    /// so grading is strictly opt-in.
    fn default() -> Self {
        Self {
            bands: vec![(0, ExtractiveCompactor::default())],
        }
    }
}

impl GradedBands {
    /// Build from explicit `(start_index, compactor)` bands. Normalizes by sorting
    /// on `start_index` and ensuring the first band starts at 0 (a missing zero-band
    /// is filled with [`ExtractiveCompactor::default`]).
    #[must_use]
    pub fn new(mut bands: Vec<(usize, ExtractiveCompactor)>) -> Self {
        bands.sort_by_key(|(start, _)| *start);
        if bands.first().is_none_or(|(start, _)| *start != 0) {
            bands.insert(0, (0, ExtractiveCompactor::default()));
        }
        Self { bands }
    }

    /// The default v1 grading preset at compaction `level` (#989 target-seeking):
    /// three depth bands from aggressive (oldest) to today's strength (newest-cold),
    /// each deepened by `level`. `graded_v1(0)` is **byte-identical to the original
    /// Step-1 preset** (strict non-regression anchor); higher levels shrink every
    /// band's caps monotonically so the target-seeking loop can drive `wire ≤ T`
    /// without an LLM round-trip. `level` never touches `KEEP_RECENT_VERBATIM`
    /// (reserved for the hard floor) — it only intensifies per-message compression.
    #[must_use]
    pub fn graded_v1(level: usize) -> Self {
        Self::new(vec![
            // Oldest messages: fold hard.
            (
                0,
                deepen(
                    ExtractiveCompactor {
                        max_value_chars: 96,
                        max_array_items: 3,
                        keep_head_lines: 2,
                        keep_tail_lines: 2,
                        min_lines_to_elide: 8,
                        min_tokens_to_compact: 64,
                    },
                    level,
                ),
            ),
            // Mid-history: between aggressive and default.
            (
                40,
                deepen(
                    ExtractiveCompactor {
                        max_value_chars: 160,
                        max_array_items: 5,
                        keep_head_lines: 3,
                        keep_tail_lines: 3,
                        min_lines_to_elide: 10,
                        min_tokens_to_compact: 64,
                    },
                    level,
                ),
            ),
            // Newest-cold: today's strength at level 0 (non-regression anchor).
            (100, deepen(ExtractiveCompactor::default(), level)),
        ])
    }

    /// The band slot (index into `bands`) governing a message at absolute `index`,
    /// by depth alone. `bands` is non-empty and starts at 0, so this always resolves.
    fn band_slot_for(&self, index: usize) -> usize {
        let mut slot = 0;
        for (i, (start, _)) in self.bands.iter().enumerate() {
            if *start <= index {
                slot = i;
            } else {
                break;
            }
        }
        slot
    }

    /// The compactor governing a message at absolute `index` (depth only).
    #[must_use]
    pub fn compactor_for(&self, index: usize) -> &ExtractiveCompactor {
        &self.bands[self.band_slot_for(index)].1
    }

    /// Salience-shifted band (#933 / RFC 0022 Step 2b): start from the depth-based
    /// band, then shift by the message's value score so an *important* older message
    /// stays in a gentler band (or verbatim-ish) while low-value bulk folds harder,
    /// regardless of position. Bands are ordered gentlest-last, so a higher score
    /// moves toward a later (gentler) slot.
    ///
    /// Cache-stable by construction: `index` is absolute and `score` is a pure
    /// function of the message's content (see [`crate::MessageSalience`]), so the
    /// resolved band is identical every turn — the frozen-boundary invariant #970
    /// relies on. `score == None` reproduces pure depth grading (Step 1).
    #[must_use]
    pub fn compactor_for_scored(&self, index: usize, score: Option<f32>) -> &ExtractiveCompactor {
        let base = self.band_slot_for(index) as isize;
        let delta = match score {
            // Gentler for high-value, harder for low-value; flat in the middle.
            Some(s) if s >= 0.75 => 2,
            Some(s) if s >= 0.55 => 1,
            Some(s) if s <= 0.35 => -1,
            _ => 0,
        };
        let slot = (base + delta).clamp(0, self.bands.len() as isize - 1) as usize;
        &self.bands[slot].1
    }

    /// Graded counterpart of [`ExtractiveCompactor::compact_cold_collect`]: compact
    /// the cold prefix `messages[..len-keep_recent]`, choosing each message's
    /// compression strength by its absolute index. Recent tail, assistant messages,
    /// and already-compacted content are passed through unchanged.
    #[must_use]
    pub fn compact_graded_collect(
        &self,
        messages: &[Message],
        keep_recent: usize,
    ) -> ColdCompaction {
        self.compact_graded_range(
            messages,
            0,
            messages.len().saturating_sub(keep_recent),
            None,
        )
    }

    /// Graded counterpart of [`ExtractiveCompactor::compact_range_collect`]: compact
    /// `messages` treated as occupying absolute indices `base_index..`, used by the
    /// frozen-boundary path to grade only the newly-cold slice while the cached
    /// prefix stays frozen. `cold_end` is the absolute index (exclusive) past which
    /// messages are kept verbatim.
    ///
    /// `scorer` (#933 / RFC 0022 Step 2b) makes band choice value-aware: an
    /// important older message stays in a gentler band, low-value bulk folds harder.
    /// `None` reproduces pure depth grading (Step 1). Cache stability holds either
    /// way because band choice is a pure function of (absolute index, message
    /// content) — see [`Self::compactor_for_scored`].
    #[must_use]
    pub fn compact_graded_range(
        &self,
        messages: &[Message],
        base_index: usize,
        cold_end: usize,
        scorer: Option<&dyn Salience<Message>>,
    ) -> ColdCompaction {
        let mut before = 0usize;
        let mut after = 0usize;
        let mut out = Vec::with_capacity(messages.len());
        let mut originals = Vec::new();
        for (j, m) in messages.iter().enumerate() {
            let index = base_index + j;
            before += proxy_tokens(&m.content);
            if index < cold_end
                && m.role != Role::Assistant
                && !m.content.contains(COMPACTION_MARKER_PREFIX)
            {
                let score = scorer.map(|s| s.score(m, 0));
                let outcome = self
                    .compactor_for_scored(index, score)
                    .compress_one(&m.content);
                after += proxy_tokens(&outcome.text);
                if let Some((key, original)) = outcome.original {
                    originals.push((m.id.clone(), key, original));
                }
                let mut clone = m.clone();
                clone.content = outcome.text;
                out.push(clone);
            } else {
                after += proxy_tokens(&m.content);
                out.push(m.clone());
            }
        }
        let originals_cached = originals.len();
        ColdCompaction {
            messages: out,
            originals,
            savings: CompactionSavings {
                before_tokens: before,
                after_tokens: after,
                originals_cached,
            },
        }
    }
}

/// Sentinel compaction level marking a prefix that was collapsed into a single
/// timeline-digest message (#1045). One past [`MAX_COMPACTION_LEVEL`] so the
/// frozen-boundary cache can carry it in the same `level` slot; the reuse path
/// treats it as "no incremental extension possible — full digest pass on tick".
pub const DIGEST_LEVEL: usize = MAX_COMPACTION_LEVEL + 1;

/// Max chars of a message's first content line kept in its digest entry. Short
/// enough that a digest line is ~15-25 proxy tokens; long enough to keep the
/// gist ("what was asked / what was concluded") recognizable.
const DIGEST_LINE_CHARS: usize = 96;

/// A cold prefix collapsed into one deterministic timeline-digest message (#1045).
pub struct DigestResult {
    /// The single synthetic message replacing the folded range.
    pub message: Message,
    /// `(anchor_message_id, retrieve_key, original_rendering)` to persist so
    /// `compaction_retrieve` can fetch the folded block back verbatim.
    pub original: (String, String, String),
}

/// Mechanical fold-of-fold (#1045): collapse `messages` (the Mid layer) into a
/// **single** timeline-digest message capped at `cap_tokens` proxy tokens. The
/// per-message graded fold has a nonzero floor, so a long enough transcript
/// outgrows any Mid ceiling — this is the deterministic, LLM-free ceiling
/// enforcement. One chronological line per message (role + first content line);
/// when over the cap, the **oldest** lines are elided behind a count so the
/// digest stays a connected recent-history spine. Pure function of
/// `(messages, cap_tokens)` — byte-stable between folds, per the frozen-boundary
/// invariant (#1027).
#[must_use]
pub fn digest_block(messages: &[Message], cap_tokens: usize) -> Option<DigestResult> {
    if messages.is_empty() {
        return None;
    }
    let lines: Vec<String> = messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let first = m
                .content
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("");
            let mut snippet: String = first.chars().take(DIGEST_LINE_CHARS).collect();
            if first.chars().count() > DIGEST_LINE_CHARS {
                snippet.push_str("...");
            }
            format!("[{i}] {}: {snippet}", role_label(m.role))
        })
        .collect();

    // The verbatim original for `compaction_retrieve`: every folded message,
    // rendered whole. Persisted, never sent.
    let original: String = messages
        .iter()
        .map(|m| format!("{}: {}\n\n", role_label(m.role), m.content))
        .collect();
    let key = content_key(&original);

    // Assemble newest-last, dropping the oldest lines until the digest fits the
    // cap. Header + marker are part of the budget.
    let total = messages.len();
    let mut kept = lines.len();
    loop {
        let elided = total - kept;
        let header = if elided == 0 {
            format!("Timeline digest of the {total} earliest messages in this conversation (one line each; originals retrievable):")
        } else {
            format!("Timeline digest of the {total} earliest messages in this conversation ({elided} oldest elided; one line each; originals retrievable):")
        };
        let body = lines[total - kept..].join("\n");
        let content = format!("{header}\n{body}\n{COMPACTION_MARKER_PREFIX}{key}]");
        if proxy_tokens(&content) <= cap_tokens || kept == 0 {
            let anchor = messages.last().map(|m| m.id.clone()).unwrap_or_default();
            let message = Message {
                id: "compaction-digest".to_string(),
                session_id: messages
                    .first()
                    .map(|m| m.session_id.clone())
                    .unwrap_or_default(),
                // User (not System) for the same reason as the Tier-2 summary:
                // system-role messages get hoisted out of chronological position
                // on Anthropic/Bedrock.
                role: Role::User,
                content,
                tool_calls: None,
                tool_call_id: None,
                attachments: None,
                reasoning: None,
                stop_reason: None,
                author_name: None,
                created_at: messages.first().map(|m| m.created_at).unwrap_or(0),
            };
            return Some(DigestResult {
                message,
                original: (anchor, key, original),
            });
        }
        // Drop the oldest ~10% per probe (at least 1) so the fit loop is O(10)
        // rather than O(n) for a badly over-cap digest.
        kept -= (kept / 10).max(1);
    }
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::Tool => "Tool",
        Role::System => "System",
    }
}

fn truncate_value(value: &mut serde_json::Value, max_value_chars: usize, max_array_items: usize) {
    use serde_json::Value;
    match value {
        Value::String(s) if s.chars().count() > max_value_chars => {
            // Keep first `max_value_chars` chars and a marker; chars-aware so we
            // never split a multi-byte codepoint.
            let kept: String = s.chars().take(max_value_chars).collect();
            *s = format!("{kept}<...truncated>");
        }
        Value::Array(items) => {
            if items.len() > max_array_items {
                let dropped = items.len() - max_array_items;
                items.truncate(max_array_items);
                items.push(Value::String(format!("<compacted items=\"{dropped}\"/>")));
            }
            for item in items.iter_mut() {
                truncate_value(item, max_value_chars, max_array_items);
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                truncate_value(v, max_value_chars, max_array_items);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
