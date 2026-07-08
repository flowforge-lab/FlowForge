//! The [`ObserverSource`] trait — the contract every backend (file, http,
//! process) implements. The supervisor owns the lifecycle; the source owns
//! the OS/IO details.
//!
//! `next_event` is the long-poll: it resolves with the next fired event, or
//! `None` if the cooperative `cancel` token trips (shutdown, stop, reap). The
//! supervisor drives one task per observer and reaps on `None`.

use std::fmt;
use std::path::PathBuf;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::event::{ObserverError, ObserverEvent, ObserverId, ObserverKind, ObserverSpec};

#[async_trait]
pub trait ObserverSource: Send {
    /// A short, human-readable key for the watcher (e.g. a path or URL).
    /// Surfaced as the "key" in [`ObserverEvent`] and rendered into the
    /// synthetic user message.
    fn key(&self) -> &str;

    /// The first event the source would emit *if it were already in the
    /// "changed" state*. For example, the HTTP source returns a "content
    /// changed" event if the URL has drifted since last poll; the file source
    /// returns a "file already changed" event when the watch is set up after a
    /// write has already happened. This avoids a silent boot where the agent
    /// is "watching" a file that has already changed (#709 §"Zero-cost idle").
    ///
    /// `id` is the supervisor-assigned id; sources should stamp it onto the
    /// returned event so the host's subscriber sees the real id, not a
    /// placeholder.
    ///
    /// Default: `None` (no bootstrap event).
    async fn prime(&mut self, id: ObserverId) -> Result<Option<ObserverEvent>, ObserverError> {
        let _ = id;
        Ok(None)
    }

    /// Block until the next event fires or `cancel` is tripped. Returning
    /// `Ok(None)` signals the supervisor that the source has terminated
    /// cleanly (e.g. process exited, watched file removed). `Err` is treated
    /// as a recoverable error and surfaced to the host, not the model.
    /// `id` is the supervisor-assigned id; sources stamp it onto the
    /// returned event.
    async fn next_event(
        &mut self,
        id: ObserverId,
        cancel: &CancellationToken,
    ) -> Result<Option<ObserverEvent>, ObserverError>;
}

impl fmt::Debug for dyn ObserverSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("dyn ObserverSource")
            .field("key", &self.key())
            .finish()
    }
}

/// Reject an empty target, normalize an absolute path, and return the parent
/// directory + file basename. Used by [`crate::file::FileSource`] so the OS
/// watch attaches to a real directory and a basename filter has a single
/// representation.
pub(crate) fn split_target_path(target: &str) -> Result<(PathBuf, Option<String>), ObserverError> {
    if target.trim().is_empty() {
        return Err(ObserverError::InvalidTarget {
            kind: "file",
            reason: "target path must not be empty".into(),
        });
    }
    let p = PathBuf::from(target);
    if !p.exists() {
        return Err(ObserverError::InvalidTarget {
            kind: "file",
            reason: format!("path does not exist: {target}"),
        });
    }
    let (dir, name) = if p.is_dir() {
        (p.clone(), None)
    } else {
        let parent = p.parent().map(|x| x.to_path_buf()).unwrap_or(p.clone());
        let stem = p.file_name().map(|n| n.to_string_lossy().into_owned());
        (parent, stem)
    };
    if !dir.is_dir() {
        return Err(ObserverError::InvalidTarget {
            kind: "file",
            reason: format!("not a directory: {}", dir.display()),
        });
    }
    Ok((dir, name))
}

/// Build a source from a spec. Each backend parses the parts of `spec` it
/// cares about. Returns a boxed trait object so the supervisor can hold a
/// heterogeneous map of observers.
pub async fn build_source(spec: ObserverSpec) -> Result<Box<dyn ObserverSource>, ObserverError> {
    match spec.kind {
        ObserverKind::File => Ok(Box::new(crate::file::FileSource::from_spec(spec).await?)),
        ObserverKind::Http => Ok(Box::new(crate::http::HttpSource::from_spec(spec).await?)),
        ObserverKind::Process => Ok(Box::new(crate::process::ProcessSource::from_spec(spec)?)),
    }
}
