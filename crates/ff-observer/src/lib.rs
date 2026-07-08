//! Public Observer framework API.
//!
//! Observers are session-scoped background monitors that watch a source (file,
//! HTTP URL, or process stdout/stderr) and surface events to the agent. The
//! framework owns the lifecycle; the host (desktop `AppState`) holds the
//! [`supervisor::ObserverSupervisor`] and reaps observers on session close.
//!
//! Phase 1 ships [`file::FileSource`] (kqueue/inotify, no `notify` crate).
//! Phase 2 adds [`http::HttpSource`] and Phase 3 adds [`process::ProcessSource`]
//! wired to the [`ff_tools::process::ProcessSupervisor`].

pub mod event;
pub mod file;
pub mod http;
pub mod process;
pub mod source;
pub mod supervisor;
pub mod tool;

pub use event::{ObserverEvent, ObserverId, ObserverInfo, ObserverKind, ObserverSpec};
pub use source::ObserverSource;
pub use supervisor::ObserverSupervisor;
pub use tool::ObserverTool;
