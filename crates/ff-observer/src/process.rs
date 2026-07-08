//! `ProcessSource` — pattern-match on stdout/stderr of a running
//! [`ff_tools::process::ProcessSupervisor`] child. Phase 3 of #709.
//!
//! The supervisor was widened with a per-process line broadcast sink so
//! `ProcessSource` can subscribe once and react to new lines without
//! re-reading the ring buffer. The wiring is best-effort: a source that
//! outlives its target process returns `Ok(None)` and exits.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::event::{ObserverError, ObserverEvent, ObserverSpec};
use crate::source::ObserverSource;

/// What the broadcast sink carries. Mirrors the public type on
/// `ff_tools::process`; duplicated here as a one-field newtype so the
/// observer crate doesn't pull a new dep just to deserialize the line.
#[derive(Debug, Clone)]
pub struct LineEvent {
    pub line: String,
    pub is_stderr: bool,
}

#[derive(Debug)]
pub struct ProcessSource {
    process_id: u64,
    key: String,
    filter: Option<Regex>,
    supervisor: Arc<ff_tools::process::ProcessSupervisor>,
}

impl ProcessSource {
    pub fn from_spec(spec: ObserverSpec) -> Result<Self, ObserverError> {
        let process_id: u64 =
            spec.target
                .trim()
                .parse()
                .map_err(|_| ObserverError::InvalidTarget {
                    kind: "process",
                    reason: format!("target must be a numeric process id, got '{}'", spec.target),
                })?;
        let key = format!("pid {process_id}");
        let filter = spec
            .filter
            .as_deref()
            .map(Regex::new)
            .transpose()
            .map_err(|e| ObserverError::InvalidFilter(e.to_string()))?;
        // The supervisor is injected by the host at tool-construction time
        // via [`with_supervisor`]. We can't acquire it from the spec, so
        // this constructor leaves it `None`-ish — the host wraps the call
        // and fills it in. To keep the trait flow simple, the supervisor
        // lives behind a static set-by-host OnceLock.
        let supervisor = current_supervisor().ok_or_else(|| {
            ObserverError::Other(
                "process observer: no ProcessSupervisor registered; call \
                 `ff_observer::process::set_supervisor` at app boot"
                    .into(),
            )
        })?;
        Ok(Self {
            process_id,
            key,
            filter,
            supervisor,
        })
    }

    /// Build with an explicit supervisor (used by tests + host wiring).
    /// This is the construction path the host uses after calling
    /// `set_supervisor`; tests can use it directly to inject a custom
    /// supervisor without touching the global.
    pub fn with_supervisor(
        process_id: u64,
        filter: Option<Regex>,
        supervisor: Arc<ff_tools::process::ProcessSupervisor>,
    ) -> Self {
        Self {
            process_id,
            key: format!("pid {process_id}"),
            filter,
            supervisor,
        }
    }
}

/// Construct a `ProcessSource` against a specific supervisor. Used by the
/// host (which holds its own `Arc<ProcessSupervisor>` in `AppState`) and by
/// tests; the global `set_supervisor` path is kept for callers that prefer
/// the static injection.
pub fn from_supervisor(
    process_id: u64,
    filter: Option<Regex>,
    supervisor: Arc<ff_tools::process::ProcessSupervisor>,
) -> ProcessSource {
    ProcessSource::with_supervisor(process_id, filter, supervisor)
}

#[async_trait]
impl ObserverSource for ProcessSource {
    fn key(&self) -> &str {
        &self.key
    }

    async fn next_event(
        &mut self,
        id: crate::event::ObserverId,
        cancel: &CancellationToken,
    ) -> Result<Option<ObserverEvent>, ObserverError> {
        // Subscribe to the per-process line stream. If the process is unknown
        // or already finished, return `None` to terminate the observer.
        let mut rx = match self.supervisor.subscribe_lines(self.process_id) {
            Some(rx) => rx,
            None => return Ok(None),
        };
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Ok(None),
                line = rx.recv() => {
                    let ev = match line {
                        Ok(ev) => ev,
                        // Closed = process gone or supervisor dropped the sender.
                        Err(_) => return Ok(None),
                    };
                    let text = ev.line;
                    if let Some(re) = &self.filter {
                        if !re.is_match(&text) {
                            continue;
                        }
                    }
                    let prefix = if ev.is_stderr { "stderr" } else { "stdout" };
                    return Ok(Some(ObserverEvent {
                        id,
                        key: self.key.clone(),
                        summary: format!("{prefix} matched: {text}"),
                        occurred_at: Utc::now(),
                    }));
                }
            }
        }
    }
}

// --- Supervisor injection --------------------------------------------------

use std::sync::OnceLock;

static CURRENT_SUPERVISOR: OnceLock<Arc<ff_tools::process::ProcessSupervisor>> = OnceLock::new();

/// Install the host's process supervisor as the source of stdout/stderr lines
/// for `ProcessSource`. Must be called once at app boot, before the first
/// `observer start --source process` tool call.
pub fn set_supervisor(sup: Arc<ff_tools::process::ProcessSupervisor>) {
    let _ = CURRENT_SUPERVISOR.set(sup);
}

fn current_supervisor() -> Option<Arc<ff_tools::process::ProcessSupervisor>> {
    CURRENT_SUPERVISOR.get().cloned()
}
