//! Wire types: the spec the model/host hands the supervisor, the public
//! observer record, and the event surfaced to subscribers.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// What kind of observer the user asked for. Maps 1:1 to the
/// [`ObserverSource`](crate::source::ObserverSource) implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObserverKind {
    File,
    Http,
    Process,
}

impl ObserverKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ObserverKind::File => "file",
            ObserverKind::Http => "http",
            ObserverKind::Process => "process",
        }
    }
}

/// The user-supplied spec for an observer, parsed from tool args. Per-source
/// validation lives in each `ObserverSource` constructor — generic fields
/// (`target`, `filter`) are common; interval is HTTP-only.
#[derive(Debug, Clone)]
pub struct ObserverSpec {
    pub kind: ObserverKind,
    /// What to watch: a path for file, a URL for http, a numeric process id
    /// string for process. Stored as a string so the supervisor can be
    /// source-agnostic.
    pub target: String,
    /// Optional regex filter. For `file`, paths must match (matched against
    /// the watched file basename by default). For `http`, the response body
    /// must contain a match. For `process`, the per-line stream must match.
    pub filter: Option<String>,
    /// Poll interval for `http`. Ignored by file/process. Clamped server-side
    /// to a minimum of 30s (#709 Phase 2).
    pub interval: Option<Duration>,
}

#[derive(Debug, thiserror::Error)]
pub enum ObserverError {
    #[error("unknown observer kind: {0}")]
    UnknownKind(String),
    #[error("invalid target for {kind}: {reason}")]
    InvalidTarget { kind: &'static str, reason: String },
    #[error("invalid filter regex: {0}")]
    InvalidFilter(String),
    #[error("too many observers for this session (max {0})")]
    SessionCapReached(usize),
    #[error("observer {0} not found")]
    NotFound(u64),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObserverId(pub u64);

impl std::fmt::Display for ObserverId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "obs#{}", self.0)
    }
}

/// The state the host reads from `list()`. Per-observer counters (`fires`)
/// and a stable `key` (the original target) make it easy to render a sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserverInfo {
    pub id: ObserverId,
    pub key: String,
    pub kind: ObserverKind,
    pub target: String,
    pub filter: Option<String>,
    pub started_at: DateTime<Utc>,
    pub fires: u64,
}

/// The payload that flows from a fired observer to the host subscriber.
/// `summary` is the human-readable line we render into the synthetic
/// `[Observer "key"]: summary` user message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserverEvent {
    pub id: ObserverId,
    pub key: String,
    pub summary: String,
    pub occurred_at: DateTime<Utc>,
}
