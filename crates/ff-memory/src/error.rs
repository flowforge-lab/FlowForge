//! Memory error type. Recall degrades gracefully wherever it can (a missing file
//! is empty, never an error); these are the cases that genuinely cannot continue —
//! a corrupt index or an I/O failure writing the user's memory.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("memory index: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("memory io {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, MemoryError>;
