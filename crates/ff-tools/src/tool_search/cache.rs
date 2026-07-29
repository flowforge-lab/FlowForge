//! Durable form of the embedding cache (#1138 step 5).
//!
//! The corpus cache is in-process: #1140 moved it to the shared
//! [`ToolSearchState`](super::ToolSearchState) so it survives the per-turn
//! registry rebuild, but it still dies with the app, so every launch re-embedded
//! the whole deferred corpus before semantic recall could contribute anything.
//!
//! # Why a file, not SQLite
//!
//! ~32 tools x 768 dims x 4 bytes is about 98KB — one small map, not a queryable
//! dataset. `ff-tools` uses only `ff-memory`'s traits and types and has no
//! `rusqlite` dependency, so a table would mean a new direct dependency, a
//! migration, and `Connection` lifetime/locking to gain `WHERE` clauses over
//! 98KB that nothing needs.
//!
//! # The cache is derived data
//!
//! The source of truth is the tool registry, recomputed every turn
//! ([`corpus_texts`](super::ToolSearchTool::corpus_texts)). This file only records
//! "this text's vector has already been computed", which is what makes every
//! failure mode here benign: a missing, truncated, or unreadable file is
//! indistinguishable from a cold start, and deleting it *is* the cache-clear
//! operation.
//!
//! Consequently nothing in this module returns an error to its caller. Recall
//! must never come out worse than BM25F because a cache file was unwritable
//! (RFC 0024 §6), so every failure logs and degrades instead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::semantic::CorpusVectors;

/// On-disk shape. `model` sits at the top level rather than per entry so that a
/// snapshot from another model cannot be partially adopted: see [`load`].
#[derive(Debug, Serialize, Deserialize)]
struct Snapshot {
    model: String,
    vectors: HashMap<String, Vec<f32>>,
}

/// Read the cache for `model`, or an empty corpus.
///
/// A snapshot written by a *different* model is discarded wholesale rather than
/// filtered, and that is load-bearing. [`CorpusVectors::len`] counts entries
/// regardless of model while [`CorpusVectors::get`] filters by it, so a corpus
/// holding another model's vectors would satisfy the warm gate — "complete, do
/// not re-embed" — while every lookup missed. Semantic ranking would then return
/// nothing, silently, for the rest of the process, and look identical to a
/// healthy setup from the outside. Loading nothing is a clean cold start; that is
/// strictly better than a corpus that lies about being complete.
///
/// Dimension changes are covered by the same check: a different dimension implies
/// a different model name.
pub(crate) fn load(path: &Path, model: &str) -> CorpusVectors {
    let empty = CorpusVectors::new(model);
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return empty,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "tool_search embedding cache unreadable; starting cold");
            return empty;
        }
    };
    let snapshot: Snapshot = match serde_json::from_str(&raw) {
        Ok(s) => s,
        Err(e) => {
            // Warn, not debug: a corrupt file is silent data loss that costs a
            // full re-embed on every launch until someone deletes it.
            tracing::warn!(path = %path.display(), error = %e, "tool_search embedding cache is corrupt; starting cold");
            return empty;
        }
    };
    if snapshot.model != model {
        tracing::debug!(
            cached = %snapshot.model,
            wanted = %model,
            "tool_search embedding cache was built by another model; discarding"
        );
        return empty;
    }
    tracing::debug!(
        path = %path.display(),
        model = %model,
        entries = snapshot.vectors.len(),
        "tool_search embedding cache loaded"
    );
    CorpusVectors::from_parts(snapshot.model, snapshot.vectors)
}

/// Write `vectors` to `path`, replacing any previous snapshot.
///
/// Called once at the end of a warm rather than per insert: the whole map is ~98KB
/// and warm is already a batch.
///
/// Writes to a sibling temp file and renames, so a crash mid-write cannot leave a
/// half-written file that would then be reported as corrupt on next launch. The
/// temp file is a sibling rather than in `TMPDIR` because `rename` is only atomic
/// within a filesystem.
pub(crate) fn store(path: &Path, vectors: &CorpusVectors) {
    let (model, by_hash) = vectors.parts();
    if by_hash.is_empty() {
        return;
    }
    let snapshot = Snapshot {
        model: model.to_string(),
        vectors: by_hash.clone(),
    };
    let json = match serde_json::to_vec(&snapshot) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(error = %e, "could not serialise tool_search embedding cache");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(path = %parent.display(), error = %e, "could not create tool_search cache directory");
            return;
        }
    }
    let tmp = tmp_path(path);
    if let Err(e) = std::fs::write(&tmp, &json) {
        tracing::warn!(path = %tmp.display(), error = %e, "could not write tool_search embedding cache");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        tracing::warn!(path = %path.display(), error = %e, "could not replace tool_search embedding cache");
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    tracing::debug!(
        path = %path.display(),
        model = %model,
        entries = by_hash.len(),
        "tool_search embedding cache written"
    );
}

/// Sibling temp path, distinguished by pid so two processes warming at once
/// cannot write over each other's partial file before either renames.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    path.with_file_name(name)
}
