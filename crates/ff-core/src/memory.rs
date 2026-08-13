//! Read-only memory IPC contract for the Settings memory pane (Issue #131).
//!
//! These types ARE the IPC surface that the frontend renders — they expose the
//! curated/daily Markdown layers described in RFC 0006 so users can inspect what
//! FlowForge remembers. They are deliberately read-only: writes, the enable/disable
//! toggle, and any host-side `search_memory` are deferred to the index/settings work
//! (Issues #166 / M5.3), so this contract can freeze now without churning when
//! ranking lands.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Which memory layer a [`MemoryFileInfo`] belongs to. Mirrors the ff-memory
/// `MemoryFileKind`, kept separate so the wire contract owns its own derives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum MemoryFileKind {
    /// The hand-curated `MEMORY.md` layer.
    Curated,
    /// An auto-captured `daily/<date>.md` layer.
    Daily,
}

/// A curated-memory stratum (RFC 0008 §4) — the editable sections of `MEMORY.md`.
/// Wire contract for the Settings → Memory editor (#868/#969); mirrors the ff-memory
/// domain `Stratum`, kept separate so the wire type owns its own derives (same
/// pattern as [`MemoryFileKind`]). The host maps this to the domain enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum Stratum {
    /// Who the user is — `## Identity`.
    Identity,
    /// How the user works — `## Patterns`.
    Patterns,
    /// What the user is focused on now — `## Focus`.
    Focus,
}

/// Metadata for a single memory file, listed by `list_memory_files`. The body is
/// fetched separately via `read_memory_file` so listings stay cheap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct MemoryFileInfo {
    /// File name without directory, e.g. `MEMORY.md` or `2026-06-18.md`.
    pub name: String,
    /// Path relative to the memory root, with forward slashes, e.g.
    /// `daily/2026-06-18.md`. Pass this back to `read_memory_file`.
    pub rel_path: String,
    /// Which layer this file belongs to.
    pub kind: MemoryFileKind,
    /// Size on disk in bytes.
    #[ts(type = "number")]
    pub size_bytes: i64,
    /// Last-modified time as Unix epoch milliseconds.
    #[ts(type = "number")]
    pub modified_ms: i64,
}

/// Summary of the memory store, returned by `memory_overview` to drive the Settings
/// pane header (file/byte counts, root location, and whether capture is enabled).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct MemoryOverview {
    /// Whether memory capture is enabled. Read-only here; the toggle lands with the
    /// settings work (Issue #166).
    pub enabled: bool,
    /// Number of memory files across all layers.
    #[ts(type = "number")]
    pub file_count: i64,
    /// Total size of all memory files in bytes.
    #[ts(type = "number")]
    pub total_bytes: i64,
    /// Absolute path to the memory root directory.
    pub root_path: String,
    /// Whether usage decay is active (RFC 0007 §5 `decay.enabled`). When `false`,
    /// stats are still recorded but `weight` never decays and **no chunk is ever
    /// dormant**, so the Salience controls that exist to move a chunk across the
    /// dormancy threshold — Sleep in particular (#1239) — have nothing to act on.
    /// Surfaced so the panel can disable them and say why, instead of offering a
    /// button that silently does nothing.
    pub decay_enabled: bool,
}

/// Per-chunk salience stats for the Settings "Salience" surface (Issue #293,
/// RFC 0007 §7). One row per indexed memory chunk, joining the chunk's identity
/// with its `chunk_stats` usage. The backend computes `weight` (effective,
/// pin-aware) and `dormant` authoritatively so the frontend never re-derives the
/// dormancy threshold.
///
/// **No-row (never recalled) case:** a chunk that has been indexed but never
/// surfaced by `memory_search` has no `chunk_stats` row, so it reads
/// `weight = 1.0`, `access_count = 0`, `last_accessed_ms = None`,
/// `dormant = false`, `pinned = false`. The age clock starts at first *recall*,
/// not creation — the UI should render "never recalled" rather than an epoch
/// timestamp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct MemoryChunkStat {
    /// Stable chunk identity (`ff_memory::chunk_key`) — the mutation handle for
    /// `resetMemoryChunk` / `setMemoryChunkPinned`.
    pub chunk_key: String,
    /// Path relative to the memory root, forward slashes, e.g. `MEMORY.md` or
    /// `daily/2026-06-25.md`.
    pub rel_path: String,
    /// Nearest Markdown heading, or `None` for pre-heading preamble.
    pub heading: Option<String>,
    /// First non-empty content line of the chunk, trimmed and length-capped — a
    /// human-readable summary for the list row.
    pub preview: String,
    /// Effective (decayed, pin-aware) weight — NOT the raw stored
    /// `chunk_stats.weight`. A pinned chunk reads `1.0`.
    pub weight: f32,
    /// Times this chunk has been reinforced by a recall.
    #[ts(type = "number")]
    pub access_count: u32,
    /// Last-recalled time as Unix epoch milliseconds, or `None` if never recalled.
    #[ts(type = "number | null")]
    pub last_accessed_ms: Option<i64>,
    /// Server-computed: `decay.enabled && weight < threshold && !pinned`.
    pub dormant: bool,
    /// Whether the chunk is pinned (weight held at `1.0`, decay skipped).
    pub pinned: bool,
}
