//! Read-only file-browser IPC contract for the workspace Files panel (Issue #872).
//!
//! These types ARE the IPC surface the frontend renders. `list_directory` returns
//! a single directory level (`DirEntry`); `read_file` returns a single file's body
//! (`FileContent`). Both are jailed to the session workspace root on the backend —
//! browsing and viewing only, no writes (editing is a later phase).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One entry in a directory listing returned by `list_directory`. Directories
/// report `size: 0`; the frontend sorts directories first, then alphabetically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct DirEntry {
    /// File or directory name without any directory prefix, e.g. `main.rs`.
    pub name: String,
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// Size on disk in bytes; `0` for directories.
    #[ts(type = "number")]
    pub size: u64,
}

/// A file's body returned by `read_file`. `text` is `None` for binary (non-UTF-8)
/// files, in which case `is_binary` is `true`; `truncated` is set when the file is
/// larger than the requested byte cap and only a prefix is returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct FileContent {
    /// UTF-8 decoded contents (a prefix when `truncated`); `None` when `is_binary`.
    pub text: Option<String>,
    /// Whether the file was detected as binary (non-UTF-8) and so not decoded.
    pub is_binary: bool,
    /// Whether `text` holds only the first `max_bytes` of a larger file.
    pub truncated: bool,
    /// Full size of the file on disk in bytes, regardless of truncation.
    #[ts(type = "number")]
    pub size: u64,
}
