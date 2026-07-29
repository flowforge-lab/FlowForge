//! Semantic recall for [`tool_search`](super) (RFC 0024 Phase 2B, #1138).
//!
//! Phase 2A made BM25F the ranking, and measured its ceiling: on a held-out set
//! of queries the scorer was never fitted to, top-1 sits at 32.8% and seven
//! queries return *nothing at all*. Those seven share no token with their
//! target's text, so no lexical scorer can reach them — the gap is vocabulary,
//! not weighting, and it is why this layer exists.
//!
//! # Recall, not re-ranking
//!
//! `ff-memory`'s [`HybridIndex`](ff_memory::HybridIndex) fuses vectors with BM25
//! by over-fetching a BM25 candidate pool and reordering *within* it. That shape
//! is correct there and useless here: on a vocabulary gap the BM25F pool is
//! empty, and reordering an empty pool recovers nothing.
//!
//! So the two are independent *recall* paths — BM25F top-k and vector top-k, each
//! computed over the whole corpus — fused by Reciprocal Rank Fusion. A tool that
//! only one path finds still surfaces.
//!
//! # Degradation
//!
//! Every failure lands on Phase 2A's behaviour rather than an error. A missing
//! embedder, an unreachable server, a cold cache, or a dimension mismatch all
//! return `None` from the vector path, and fusion with an absent path is the
//! identity — the BM25F order passes through unchanged. This is load-bearing:
//! embeddings are opt-in, so the common case is no embedder at all.

use std::collections::HashMap;
use std::sync::Arc;

use ff_memory::Embedder;

/// RRF's rank-smoothing constant, matching `ff-memory` (RFC 0006 §6) so the two
/// fusions stay comparable. Large enough that a top-1 hit does not dominate a
/// well-placed hit from the other path.
const RRF_C: f64 = 60.0;

/// How many candidates each path contributes to the fusion.
///
/// Wider than the caller's limit on purpose: a tool the vector path ranks 8th may
/// still win after fusion if BM25F also ranks it modestly, and truncating each
/// path to the final limit would discard exactly that evidence.
const RECALL_DEPTH: usize = 20;

/// Corpus vectors, keyed so a stale or foreign entry can never be mistaken for a
/// current one.
///
/// # Why the key includes the model
///
/// Vectors from different models are not comparable. At equal dimensionality —
/// `nomic-embed-text` and `embeddinggemma` are both 768 — mixing them produces
/// cosine noise with no error, no panic, and no failing test: ranking quality
/// degrades silently and the obvious suspect is the fusion weighting, not the
/// cache. Keying by model identity makes that mistake unrepresentable rather than
/// merely unlikely.
///
/// Keying by content hash (not tool name) means an edited description
/// invalidates only its own entry.
#[derive(Debug, Clone)]
pub struct CorpusVectors {
    /// Which model produced these vectors. A change invalidates all of them.
    model: String,
    /// `content_hash -> vector`.
    by_hash: HashMap<u64, Vec<f32>>,
}

impl CorpusVectors {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            by_hash: HashMap::new(),
        }
    }

    /// The vector for this text, if it was embedded by `model`.
    pub fn get(&self, model: &str, text: &str) -> Option<&Vec<f32>> {
        (self.model == model)
            .then(|| self.by_hash.get(&content_hash(text)))
            .flatten()
    }

    pub fn insert(&mut self, text: &str, vector: Vec<f32>) {
        self.by_hash.insert(content_hash(text), vector);
    }

    pub fn len(&self) -> usize {
        self.by_hash.len()
    }

    /// Tests only. Production compares `len()` against the corpus size instead: an
    /// `is_empty` gate on warming is what stranded a partially-embedded corpus for
    /// the life of the process (#1140 review), so the callers that mattered were
    /// deliberately moved off it. Kept because "a failed warm leaves the cache
    /// untouched" is worth asserting directly.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.by_hash.is_empty()
    }
}

/// Stable content hash. `DefaultHasher` is not portable across releases, which is
/// fine while the cache is in-process; the durable form (#1138 step 5) needs a
/// fixed hash instead.
fn content_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// One ranked list, as `(name, rank)` with rank 0 being best.
type Ranked<'a> = Vec<&'a str>;

/// Fuse two ranked lists by Reciprocal Rank Fusion.
///
/// Returns names in fused order. Either list may be empty; fusing with an empty
/// list preserves the other's order exactly, which is what makes the whole
/// semantic path safe to switch off.
pub fn rrf_fuse(lexical: &Ranked<'_>, semantic: &Ranked<'_>) -> Vec<String> {
    let mut score: HashMap<&str, f64> = HashMap::new();
    for list in [lexical, semantic] {
        for (i, name) in list.iter().enumerate() {
            *score.entry(name).or_insert(0.0) += 1.0 / (RRF_C + (i as f64 + 1.0));
        }
    }

    let mut fused: Vec<(&str, f64)> = score.into_iter().collect();
    // Name is the tie-break so the order is deterministic — a flaky ranking would
    // make the retrieval suite's floors meaningless.
    fused.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });
    fused.into_iter().map(|(n, _)| n.to_string()).collect()
}

/// Rank the corpus by cosine similarity to `query_vector`.
///
/// `texts` supplies each tool's index text; entries without a cached vector are
/// skipped rather than treated as distant, so a partially warm cache degrades to
/// a smaller candidate set instead of a wrong one.
pub fn semantic_ranking<'a>(
    query_vector: &[f32],
    texts: &[(&'a str, String)],
    vectors: &CorpusVectors,
    model: &str,
) -> Ranked<'a> {
    let mut scored: Vec<(&str, f64)> = texts
        .iter()
        .filter_map(|(name, text)| {
            let v = vectors.get(model, text)?;
            let sim = cosine(query_vector, v);
            (sim > 0.0).then_some((*name, sim))
        })
        .collect();

    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });
    scored.truncate(RECALL_DEPTH);
    scored.into_iter().map(|(n, _)| n).collect()
}

/// Cosine similarity in `[-1, 1]`; `0.0` for mismatched-length or zero vectors.
///
/// Mismatched length means a model change the cache key failed to catch, so
/// scoring it `0.0` drops the entry rather than comparing incomparable spaces.
fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += f64::from(*x) * f64::from(*y);
        na += f64::from(*x) * f64::from(*x);
        nb += f64::from(*y) * f64::from(*y);
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Embed every corpus text that is not already cached, under `model`.
///
/// Returns the number of vectors added. Failures are skipped, not propagated: a
/// half-warm cache is strictly better than none, and the caller's fallback to
/// BM25F covers the rest.
pub fn warm(
    embedder: &Arc<dyn Embedder>,
    model: &str,
    texts: &[(&str, String)],
    vectors: &mut CorpusVectors,
) -> usize {
    if vectors.model != model {
        *vectors = CorpusVectors::new(model);
    }
    let mut added = 0;
    for (_, text) in texts {
        if vectors.get(model, text).is_some() {
            continue;
        }
        if let Ok(Some(v)) = embedder.embed_chunk(text) {
            vectors.insert(text, v);
            added += 1;
        }
    }
    added
}

#[cfg(test)]
mod tests;
