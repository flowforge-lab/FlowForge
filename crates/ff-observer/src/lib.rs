//! Session-scoped Observer framework (#891 Phase 1).
//!
//! Public surface: a [`supervisor::ObserverSupervisor`] that owns
//! per-session background observers, the [`source::ObserverSource`]
//! trait each source implements, and the [`tool::ObserverTool`] the
//! agent uses to start / stop / list them. The host (desktop) wires
//! the supervisor into [`AppState`](https://example.invalid) and
//! pumps events into the agent loop.
//!
//! Out of scope for Phase 1: the `http` and `process` source
//! implementations (Phase 2 / Phase 3 issues). The kind variants
//! exist; the supervisor rejects them at `start` time with an
//! actionable error.

pub mod cancel;
pub mod file;
pub mod http;
pub mod process;
pub mod source;
pub mod supervisor;
pub mod tool;

/// Sentinel passed by [`tool::ObserverTool::run`] when there is no
/// real session. The supervisor treats this exactly like any other
/// id (it still owns a quota), but no agent loop will ever look up
/// events routed to it.
pub const NO_SESSION_TOOL: &str = "tool-no-session";

pub use source::{
    ObserverEvent, ObserverId, ObserverInfo, ObserverKind, ObserverSource, ObserverSpec,
};
pub use supervisor::{ObserverSupervisor, MAX_PER_SESSION};
pub use tool::ObserverTool;
