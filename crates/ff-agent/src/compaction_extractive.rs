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

use ff_core::Message;
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
            if i < cold_end {
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
            if i < cold_end && !m.content.contains(COMPACTION_MARKER_PREFIX) {
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
        out.push_str(&format!("... [{elided} lines elided]\n"));
        for line in &lines[lines.len() - self.keep_tail_lines..] {
            out.push_str(line);
            out.push('\n');
        }
        out
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
                items.push(Value::String(format!("<...{dropped} items elided>")));
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
mod tests {
    use super::*;
    use ff_core::Role;

    fn msg(role: Role, content: &str) -> Message {
        Message {
            id: String::new(),
            session_id: String::new(),
            role,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            attachments: None,
            reasoning: None,
            created_at: 0,
        }
    }

    #[test]
    fn classify_detects_json_code_prose() {
        assert_eq!(classify(r#"{"a": 1, "b": [1,2]}"#), ContentKind::Json);
        assert_eq!(classify("[1,2,3]"), ContentKind::Json);
        // Fenced code block is unambiguous.
        assert_eq!(classify("```rust\nfn f() {}\n```"), ContentKind::Code);
        // Strong unfenced code: function keyword + a balanced brace pair.
        assert_eq!(
            classify("fn main() {\n    println!(\"hi\");\n}\n"),
            ContentKind::Code
        );
        // Prose with an incidental brace must not be misclassified.
        assert_eq!(
            classify("Choose option {1}: it is the cleanest way."),
            ContentKind::Prose
        );
        // Malformed JSON falls back to prose, not Json.
        assert_eq!(classify("{ not really json"), ContentKind::Prose);
    }

    #[test]
    fn cache_dedupes_identical_originals() {
        let mut cache = ReversibleCache::new();
        let k1 = cache.put("hello".to_string());
        let k2 = cache.put("hello".to_string());
        assert_eq!(k1, k2);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.retrieve(&k1), Some("hello"));
        assert_eq!(cache.retrieve("not-a-key"), None);
    }

    #[test]
    fn small_blobs_are_left_alone() {
        // Below `min_tokens_to_compact` the compactor must not cache or mark.
        let comp = ExtractiveCompactor::default();
        let mut cache = ReversibleCache::new();
        let small = "short message";
        let out = comp.compress(small, &mut cache);
        assert_eq!(out, small);
        assert!(cache.is_empty());
    }

    #[test]
    fn json_compression_shrinks_and_round_trips() {
        let comp = ExtractiveCompactor::default();
        let mut cache = ReversibleCache::new();
        // A pretty-printed JSON object with a long string and a long array --
        // exactly the shape of a chatty tool output.
        let big_string = "x".repeat(400);
        let big_array: Vec<i32> = (0..50).collect();
        let original = serde_json::to_string_pretty(&serde_json::json!({
            "summary": big_string,
            "items": big_array,
            "ok": true,
        }))
        .unwrap();
        let before = proxy_tokens(&original);

        let out = comp.compress(&original, &mut cache);
        assert!(
            out.len() < original.len(),
            "JSON compression must shrink: before={} after={}",
            original.len(),
            out.len()
        );
        assert!(out.contains("[compacted; retrieve key="));
        assert_eq!(cache.len(), 1, "the original must be cached for retrieval");
        let after = proxy_tokens(&out);
        assert!(
            after < before,
            "proxy tokens must decrease: before={before} after={after}"
        );

        // The retrieve key in the marker maps back to the verbatim original.
        let key = out
            .rsplit("key=")
            .next()
            .unwrap()
            .trim_end_matches(']')
            .to_string();
        assert_eq!(cache.retrieve(&key), Some(original.as_str()));
    }

    #[test]
    fn line_elision_keeps_head_and_tail() {
        let comp = ExtractiveCompactor {
            keep_head_lines: 2,
            keep_tail_lines: 2,
            min_lines_to_elide: 6,
            min_tokens_to_compact: 0,
            ..ExtractiveCompactor::default()
        };
        let mut cache = ReversibleCache::new();
        let original = (1..=20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = comp.compress(&original, &mut cache);
        assert!(out.contains("line 1\n"));
        assert!(out.contains("line 2\n"));
        assert!(out.contains("line 19\n"));
        assert!(out.contains("line 20"));
        assert!(out.contains("16 lines elided"));
        assert!(out.contains("[compacted; retrieve key="));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn compact_cold_keeps_recent_verbatim() {
        let comp = ExtractiveCompactor {
            keep_head_lines: 2,
            keep_tail_lines: 2,
            min_lines_to_elide: 6,
            min_tokens_to_compact: 0,
            ..ExtractiveCompactor::default()
        };
        let mut cache = ReversibleCache::new();
        let big = (1..=30)
            .map(|i| format!("row {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let small = "ok";
        let messages = vec![
            msg(Role::User, &big),      // cold (will compress)
            msg(Role::Assistant, &big), // cold (will compress)
            msg(Role::User, small),     // recent (verbatim)
        ];

        let (out, savings) = comp.compact_cold(&messages, 1, &mut cache);

        assert_eq!(out.len(), 3);
        // Cold messages were rewritten.
        assert_ne!(out[0].content, big);
        assert_ne!(out[1].content, big);
        assert!(out[0].content.contains("[compacted; retrieve key="));
        assert!(out[1].content.contains("[compacted; retrieve key="));
        // Recent message is byte-identical.
        assert_eq!(out[2].content, small);
        // Savings make sense.
        assert!(savings.before_tokens > savings.after_tokens);
        assert!(savings.saved() > 0);
        assert!(savings.ratio() > 0.0);
        // Cache deduped: both cold messages had identical content -> one entry.
        assert_eq!(savings.originals_cached, 1);
        assert_eq!(cache.len(), 1);

        // And the retrieve key in either compressed message resolves to the
        // verbatim original.
        let key = out[0]
            .content
            .rsplit("key=")
            .next()
            .unwrap()
            .trim_end_matches(']')
            .to_string();
        assert_eq!(cache.retrieve(&key), Some(big.as_str()));
    }

    #[test]
    fn already_minimal_content_is_not_marked() {
        // A blob that is too tight to shrink (compact JSON, short, no arrays)
        // must come back unchanged with no cache pollution.
        let comp = ExtractiveCompactor::default();
        let mut cache = ReversibleCache::new();
        let tight = serde_json::to_string(&serde_json::json!({"ok": true, "n": 1})).unwrap();
        let out = comp.compress(&tight, &mut cache);
        assert_eq!(out, tight);
        assert!(cache.is_empty());
    }

    #[test]
    fn savings_zero_ratio_when_no_input() {
        let s = CompactionSavings {
            before_tokens: 0,
            after_tokens: 0,
            originals_cached: 0,
        };
        assert_eq!(s.saved(), 0);
        assert_eq!(s.ratio(), 0.0);
    }

    #[test]
    fn json_string_truncation_is_unicode_safe() {
        // A JSON value with a long multi-byte string. Truncation must split on
        // a char boundary, never a byte mid-codepoint. The payload is long
        // enough that compression amortizes the marker so the cache fires.
        let comp = ExtractiveCompactor {
            max_value_chars: 5,
            min_tokens_to_compact: 0,
            ..ExtractiveCompactor::default()
        };
        let mut cache = ReversibleCache::new();
        let long = "héllo wörld 你好 こんにちは ".repeat(20);
        let original = serde_json::to_string(&serde_json::json!({ "msg": long })).unwrap();
        let out = comp.compress(&original, &mut cache);
        // Output must be valid UTF-8 (the cargo test framework would already panic
        // otherwise, but make the contract explicit) and must not contain a raw
        // codepoint replacement marker from a bad split.
        assert!(out.is_char_boundary(out.len()));
        assert!(!out.contains('\u{FFFD}'));
        // Round-trip: the marker must point back to the verbatim original.
        let key = out
            .rsplit("key=")
            .next()
            .unwrap()
            .trim_end_matches(']')
            .to_string();
        assert_eq!(cache.retrieve(&key), Some(original.as_str()));
    }

    #[test]
    fn compress_one_returns_key_and_original_on_shrink() {
        let comp = ExtractiveCompactor {
            min_tokens_to_compact: 0,
            ..ExtractiveCompactor::default()
        };
        let big = (1..=40)
            .map(|i| format!("log line number {i} with some filler text"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = comp.compress_one(&big);
        let (key, original) = out.original.expect("a large blob must shrink and cache");
        assert_eq!(original, big, "the cached original must be byte-identical");
        assert!(
            out.text.contains(&format!("key={key}")),
            "the marker must carry the same key the caller persists"
        );
        assert!(proxy_tokens(&out.text) < proxy_tokens(&big));
    }

    #[test]
    fn compress_one_returns_none_below_threshold() {
        let comp = ExtractiveCompactor::default();
        let small = "a short tool result";
        let out = comp.compress_one(small);
        assert_eq!(out.text, small);
        assert!(out.original.is_none());
        assert!(!out.text.contains("[compacted"));
    }

    #[test]
    fn compress_one_key_matches_marker() {
        let comp = ExtractiveCompactor {
            max_value_chars: 8,
            min_tokens_to_compact: 0,
            ..ExtractiveCompactor::default()
        };
        let blob = serde_json::to_string(&serde_json::json!({
            "field": "x".repeat(2000)
        }))
        .unwrap();
        let out = comp.compress_one(&blob);
        let (key, _) = out.original.clone().expect("shrinks");
        let marker_key = out
            .text
            .rsplit("key=")
            .next()
            .unwrap()
            .trim_end_matches(']')
            .to_string();
        assert_eq!(marker_key, key);
    }

    fn msg_with_id(id: &str, role: Role, content: &str) -> Message {
        let mut m = msg(role, content);
        m.id = id.to_string();
        m
    }

    #[test]
    fn compact_cold_collect_keeps_recent_verbatim() {
        let comp = ExtractiveCompactor {
            keep_head_lines: 2,
            keep_tail_lines: 2,
            min_lines_to_elide: 6,
            min_tokens_to_compact: 0,
            ..ExtractiveCompactor::default()
        };
        let cold_a = (1..=30)
            .map(|i| format!("a{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let cold_b = (1..=30)
            .map(|i| format!("b{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let recent = "the exact recent state";
        let messages = vec![
            msg_with_id("m0", Role::User, &cold_a),
            msg_with_id("m1", Role::Assistant, &cold_b),
            msg_with_id("m2", Role::User, recent),
        ];

        let cold = comp.compact_cold_collect(&messages, 1);

        assert_eq!(cold.messages.len(), 3);
        // Cold prefix compacted.
        assert!(cold.messages[0].content.contains(COMPACTION_MARKER_PREFIX));
        assert!(cold.messages[1].content.contains(COMPACTION_MARKER_PREFIX));
        // Recent message is byte-identical.
        assert_eq!(cold.messages[2].content, recent);
        assert!(cold.savings.saved() > 0);
    }

    #[test]
    fn compact_cold_collect_skips_already_compacted() {
        let comp = ExtractiveCompactor {
            min_tokens_to_compact: 0,
            ..ExtractiveCompactor::default()
        };
        // A cold message that already carries the ingest-time marker must pass
        // through untouched -- no double-compaction, no original collected.
        let already = format!("short summary\n{COMPACTION_MARKER_PREFIX}deadbeefdeadbeef]");
        let messages = vec![
            msg_with_id("m0", Role::Tool, &already),
            msg_with_id("m1", Role::User, "recent"),
        ];

        let cold = comp.compact_cold_collect(&messages, 1);

        assert_eq!(cold.messages[0].content, already);
        assert!(cold.originals.is_empty());
    }

    #[test]
    fn compact_cold_collect_collects_originals_with_message_ids() {
        let comp = ExtractiveCompactor {
            max_value_chars: 8,
            min_tokens_to_compact: 0,
            ..ExtractiveCompactor::default()
        };
        let blob = serde_json::to_string(&serde_json::json!({
            "field": "x".repeat(2000)
        }))
        .unwrap();
        let messages = vec![
            msg_with_id("cold-id", Role::Tool, &blob),
            msg_with_id("recent-id", Role::User, "recent"),
        ];

        let cold = comp.compact_cold_collect(&messages, 1);

        assert_eq!(cold.originals.len(), 1);
        let (mid, key, original) = &cold.originals[0];
        assert_eq!(mid, "cold-id");
        assert_eq!(original, &blob);
        // The collected key matches the marker emitted on the wire content.
        assert!(cold.messages[0].content.contains(&format!("key={key}]")));
        assert_eq!(cold.savings.originals_cached, 1);
    }
}
