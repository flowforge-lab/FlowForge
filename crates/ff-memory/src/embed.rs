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

/// A local-first [`Embedder`] backed by an OpenAI-compatible `/v1/embeddings`
/// HTTP endpoint (RFC 0006 §6). This is the M5.3.1 "local model server" path:
/// point it at a candle-vLLM / vLLM / LM Studio / Ollama server running an
/// **embedding-capable model** and recall fuses vector similarity with BM25.
///
/// It holds firmly to the [`Embedder`] contract: **any** failure — connection
/// refused, non-2xx (e.g. a chat-only server with no embeddings route),
/// malformed body, or a zero/empty vector — degrades to `Ok(None)` so the
/// [`HybridIndex`](crate::index::HybridIndex) falls back to pure BM25. The user
/// is never blocked because their embedding server is down or misconfigured.
///
/// The same client serves the M5.3.2 cloud path: pass an `api_key` and an
/// `https` base URL. Embeddings remain opt-in and off by default (RFC 0006 §8).
#[derive(Debug)]
pub struct OpenAiEmbedder {
    client: reqwest::blocking::Client,
    endpoint: String,
    model: String,
    api_key: Option<String>,
}

impl OpenAiEmbedder {
    /// Build an embedder targeting `{base_url}/embeddings` with `model`. A
    /// trailing slash on `base_url` is tolerated. `api_key` is sent as a bearer
    /// token when present (cloud); local servers leave it `None`.
    ///
    /// The blocking HTTP client owns its own runtime thread, so this is safe to
    /// call from sync code (the watcher, `build_memory`) — and callers in an
    /// async context should invoke `search`/`reindex` via `spawn_blocking` so a
    /// worker thread is never parked on the network round-trip.
    pub fn new(
        base_url: impl AsRef<str>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        let endpoint = format!("{}/embeddings", base_url.as_ref().trim_end_matches('/'));
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|e| {
                // A misbuilt client (e.g. a broken TLS stack) falls back to the
                // default so memory still works via BM25; warn so it is debuggable.
                tracing::warn!(error = %e, "failed to build embedder HTTP client; using default");
                reqwest::blocking::Client::default()
            });
        Self {
            client,
            endpoint,
            model: model.into(),
            api_key,
        }
    }

    /// POST one input and return its vector, or `None` on any failure (the
    /// BM25-fallback guarantee). Empty input never hits the network.
    fn embed(&self, input: &str) -> Option<Vec<f32>> {
        if input.trim().is_empty() {
            return None;
        }
        let mut req = self.client.post(&self.endpoint).json(&EmbeddingRequest {
            model: &self.model,
            input,
        });
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: EmbeddingResponse = resp.json().ok()?;
        let vector = body.data.into_iter().next()?.embedding;
        if vector.is_empty() || vector.iter().all(|x| *x == 0.0) {
            None
        } else {
            Some(vector)
        }
    }
}

impl Embedder for OpenAiEmbedder {
    fn embed_query(&self, query: &str) -> Result<Option<Vec<f32>>> {
        Ok(self.embed(query))
    }
    fn embed_chunk(&self, text: &str) -> Result<Option<Vec<f32>>> {
        Ok(self.embed(text))
    }
}

#[derive(serde::Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: &'a str,
}

#[derive(serde::Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(serde::Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

#[cfg(test)]
mod tests;
