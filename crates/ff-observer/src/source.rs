//! Cross-cutting data shapes for the observer framework. The trait, the spec
//! that the agent supplies to start an observer, the event delivered to the
//! host, and the public info shown by `list`.
//!
//! Concrete sources ([`crate::file::FileSource`], plus the `http` and
//! `process` stubs) live in their own modules; the supervisor holds them
//! behind a uniform `ObserverSource` trait so the wake-pump doesn't need to
//! know which kind fired.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Notify;

/// A monotonic, session-global observer id. Allocated by
/// [`crate::supervisor::ObserverSupervisor::start`] and used by the agent to
/// later `stop` an observer.
pub type ObserverId = u64;

/// The concrete source a `start` call instantiates. Phase 1 only ships
/// [`ObserverKind::File`]; the other variants are placeholders so
/// `ObserverSpec` already has the right shape and Phase 2/3 can fill them in
/// without an API break.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObserverKind {
    File,
    Http,
    Process,
}

/// The user-facing description of an observer. Constructed by the
/// `observer` tool from its `start` action's args and passed to
/// [`crate::supervisor::ObserverSupervisor::start`].
#[derive(Clone, Debug)]
pub struct ObserverSpec {
    /// Human-readable name. Echoed back in wake messages (`[Observer
    /// "<label>"]: ...`) and shown by `list`.
    pub label: String,
    pub kind: ObserverKind,
    /// Source-specific target. For `file` this is a path (absolute or
    /// relative to the session root, resolved by the tool). For `http` /
    /// `process` (Phase 2/3) the interpretation is theirs.
    pub target: String,
    /// Source-specific filter. For `file` it's a glob that a directory
    /// watcher uses to limit which children trigger an event. `None`
    /// means "match everything". Reserved / ignored for other kinds.
    pub filter: Option<String>,
}

/// What the supervisor returns from `list` — the durable, human-readable
/// record of an observer. Distinct from the `ManagedObserver` internals
/// (which hold the live `JoinHandle` + cancel signal + event sender).
#[derive(Clone, Debug)]
pub struct ObserverInfo {
    pub id: ObserverId,
    pub label: String,
    pub kind: ObserverKind,
    pub target: String,
    pub started_at: DateTime<Utc>,
}

/// One wake the host needs to react to. Carries enough context to render
/// the wake text and to route the event back to its owning session
/// (the supervisor stamps `session_id` on every event so the pump doesn't
/// need a reverse lookup).
#[derive(Clone, Debug)]
pub struct ObserverEvent {
    pub session_id: String,
    pub id: ObserverId,
    pub label: String,
    pub summary: String,
}

/// Identity a source receives at construction time, so it can stamp every
/// emitted event with the right id/label without holding a back-reference
/// to its [`ObserverSpec`]. Cheap to clone.
#[derive(Clone, Debug)]
pub struct ObserverContext {
    pub session_id: String,
    pub id: ObserverId,
    pub label: String,
}

/// One live observer. The supervisor owns one of these per `start` call,
/// and forwards each emitted `ObserverEvent` to the shared event channel
/// the desktop pump drains.
///
/// Contract on `next_event`:
/// - Return `Some(event)` when the source has something to report.
/// - Return `None` when the source has terminated naturally (cancelled
///   by the supervisor, the underlying fd was closed, etc.) — the
///   supervisor's task observes this and removes the managed observer.
/// - The implementation MUST select on `cancel.notified()` so the
///   supervisor can stop a source promptly. A source that blocks
///   indefinitely in I/O without honoring `cancel` is a bug.
#[async_trait]
pub trait ObserverSource: Send {
    fn ctx(&self) -> &ObserverContext;

    /// Wait for the next event. The supervisor calls this in a loop; each
    /// call is a fresh await point so the source can be cancelled between
    /// events.
    async fn next_event(&mut self, cancel: Arc<Notify>) -> Option<ObserverEvent>;
}
