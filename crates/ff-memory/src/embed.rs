//! Embedding seam for the hybrid recall backend (RFC 0006 §6).
//!
//! Recall is FTS5/BM25 by default and forever the floor. An [`Embedder`] is the
//! optional, opt-in addition that lets [`HybridIndex`](crate::index::HybridIndex)
//! fuse vector similarity with BM25. The seam is deliberately tiny: a provider
//! turns text into a vector, or yields `None` to mean "no vector available" — in
//! which case the hybrid index falls back to pure BM25. Never a hard failure.
//!
//! M5.3.0 ships only [`NoopEmbedder`] (always `None`), so behaviour is identical
//! to FTS-only. The real local-model embedder (candle-vLLM endpoint) and the
//! cloud opt-in land in M5.3.1 / M5.3.2 behind this same trait.

use crate::error::Result;

/// Turns memory text into a dense vector for semantic recall.
///
/// Both methods return `Ok(None)` when no vector is available (the provider is
/// disabled, unreachable, or returns a zero/empty vector). Callers MUST treat
/// `None` as "fall back to BM25", not as an error.
///
/// Query and chunk embedding are separate methods because some providers apply
/// an asymmetric instruction prefix to one side; a symmetric provider simply
/// implements both the same way.
pub trait Embedder: Send + Sync {
    /// Embed a recall query.
    fn embed_query(&self, query: &str) -> Result<Option<Vec<f32>>>;
    /// Embed chunk text at index time, for storage alongside the chunk.
    fn embed_chunk(&self, text: &str) -> Result<Option<Vec<f32>>>;
}

/// The default embedder: no vectors, ever. With this in place a
/// [`HybridIndex`](crate::index::HybridIndex) is byte-identical to a bare
/// [`Fts5Index`](crate::index::Fts5Index). This is what ships in M5.3.0 and what
/// is used whenever embeddings are turned off (the default).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopEmbedder;

impl Embedder for NoopEmbedder {
    fn embed_query(&self, _query: &str) -> Result<Option<Vec<f32>>> {
        Ok(None)
    }
    fn embed_chunk(&self, _text: &str) -> Result<Option<Vec<f32>>> {
        Ok(None)
    }
}
