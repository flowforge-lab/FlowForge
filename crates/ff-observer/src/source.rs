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
use ts_rs::TS;

/// A monotonic, session-global observer id. Allocated by
/// [`crate::supervisor::ObserverSupervisor::start`] and used by the agent to
/// later `stop` an observer.
pub type ObserverId = u64;

/// The concrete source a `start` call instantiates. Phase 1 only ships
/// [`ObserverKind::File`]; the other variants are placeholders so
/// `ObserverSpec` already has the right shape and Phase 2/3 can fill them in
/// without an API break.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
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
    /// relative to the session root, resolved by the tool). For `http`
    /// this is a URL. For `process` (Phase 3) the interpretation is
    /// theirs.
    pub target: String,
    /// Source-specific filter. For `file` it's a glob that a directory
    /// watcher uses to limit which children trigger an event. For
    /// `http` it's a plain substring the body must contain to fire.
    /// `None` means "match everything" (for http: always fire on
    /// change). Reserved / ignored for other kinds.
    pub filter: Option<String>,
    /// Source-specific cadence. Currently only `http` consumes it:
    /// `None` means the source picks its own default (60s for http);
    /// `Some(n)` is clamped to the source's minimum.
    pub interval_secs: Option<u64>,
}

/// What the supervisor returns from `list` — the durable, human-readable
/// record of an observer. Distinct from the `ManagedObserver` internals
/// (which hold the live `JoinHandle` + cancel signal + event sender).
#[derive(Clone, Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct ObserverInfo {
    /// `number` on the FE, not `bigint`: observer ids are small monotonic
    /// counters, so a JS `number` is exact (mirrors the `u32` process-event
    /// ids). Avoids `bigint` friction at the `stop_observer` invoke boundary.
    #[ts(type = "number")]
    pub id: ObserverId,
    pub label: String,
    pub kind: ObserverKind,
    pub target: String,
    /// RFC 3339 timestamp (chrono serializes to a string); typed as `string` on
    /// the FE so we don't need the ts-rs `chrono-impl` feature.
    #[ts(type = "string")]
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // The `list_observers` command returns `ObserverInfo` straight to the FE,
    // so its JSON is a contract the ts-rs bindings mirror (#1038): camelCase
    // keys, a lowercased `kind`, and an RFC 3339 string `startedAt`.
    #[test]
    fn observer_info_serializes_to_the_fe_wire_shape() {
        let info = ObserverInfo {
            id: 7,
            label: "lib.rs".into(),
            kind: ObserverKind::Http,
            target: "localhost:3000/health".into(),
            started_at: Utc.with_ymd_and_hms(2026, 7, 23, 1, 2, 3).unwrap(),
        };
        let v = serde_json::to_value(&info).unwrap();
        assert_eq!(v["id"], 7);
        assert_eq!(v["label"], "lib.rs");
        assert_eq!(v["kind"], "http");
        assert_eq!(v["target"], "localhost:3000/health");
        assert_eq!(v["startedAt"], "2026-07-23T01:02:03Z");
        // No snake_case leak from the rename.
        assert!(v.get("started_at").is_none());
    }
}
