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
}
