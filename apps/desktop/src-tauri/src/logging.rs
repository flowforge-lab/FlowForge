//! Tracing subscriber installation (#1117).
//!
//! The workspace is thoroughly instrumented with `tracing` macros — the
//! observer wake path alone emits "observer wake spawning turn", "observer
//! event deferred (turn in flight)", and "start_observer_pump called twice;
//! ignoring" — but until now **no subscriber was ever installed**, so every
//! one of those was a silent no-op. A whole class of background-task bugs
//! (observer wakes that never surface, pumps started twice, drain turns that
//! don't spawn) was undiagnosable from a user report: the evidence was being
//! generated and then dropped on the floor.
//!
//! Design mirrors [`crate::boot_trace`]: opt-in via env var, off by default, so
//! a normal launch pays nothing and no user gets surprise disk writes.
//!
//! - `FF_LOG=<filter>` — enable logging at the given `EnvFilter` directive
//!   (e.g. `FF_LOG=info`, `FF_LOG=flowforge_desktop=debug,ff_observer=trace`).
//!   Unset means no subscriber at all, exactly as before this module existed.
//! - `FF_LOG_STDERR=1` — also mirror to stderr, for `cargo tauri dev`.
//!
//! Logs are written to `<data_dir>/logs/flowforge.log`, rotated daily, beside
//! the existing `sessions.db` so a bug report can include both.

use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Env var holding the filter directive. Unset = logging disabled entirely.
const FILTER_VAR: &str = "FF_LOG";
/// Env var to additionally mirror output to stderr.
const STDERR_VAR: &str = "FF_LOG_STDERR";

/// Directory that receives rotated log files, under the app data dir.
pub fn log_dir(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("logs")
}

/// Install the process-wide tracing subscriber.
///
/// Returns the appender's [`WorkerGuard`], which **must be held for the life of
/// the process** — dropping it flushes and stops the writer thread, silently
/// ending all file logging. Returns `None` when `FF_LOG` is unset (logging
/// disabled) or when the log directory can't be created.
///
/// Safe to call once. A second call is ignored by `tracing`'s global-default
/// machinery (`try_init` errors), which we swallow: a duplicate install is a
/// caller bug, not a reason to fail a launch that is otherwise fine.
#[must_use = "dropping the guard stops file logging"]
pub fn init(data_dir: &std::path::Path) -> Option<WorkerGuard> {
    let directive = std::env::var(FILTER_VAR).ok()?;
    if directive.trim().is_empty() {
        return None;
    }

    let dir = log_dir(data_dir);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        // No subscriber yet, so `tracing::warn!` would go nowhere — stderr is
        // the only channel that can report a logging failure.
        eprintln!(
            "flowforge: cannot create log dir {}: {error}",
            dir.display()
        );
        return None;
    }

    let appender = tracing_appender::rolling::daily(&dir, "flowforge.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    // Built from the same directive twice: `EnvFilter` isn't `Clone`, and each
    // layer needs its own.
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true)
        .with_filter(EnvFilter::new(&directive));

    let stderr_layer = stderr_enabled().then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_target(true)
            .with_filter(EnvFilter::new(&directive))
    });

    if tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .is_err()
    {
        eprintln!("flowforge: tracing subscriber already installed; ignoring");
        return None;
    }

    tracing::info!(
        directive = %directive,
        log_dir = %dir.display(),
        "tracing subscriber installed (#1117)"
    );
    Some(guard)
}

/// Whether to mirror logs to stderr, per [`STDERR_VAR`]. Any value other than
/// `0`/`false`/empty enables it, matching the loose truthiness of `boot_trace`.
fn stderr_enabled() -> bool {
    match std::env::var(STDERR_VAR) {
        Ok(v) => {
            let v = v.trim();
            !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("false"))
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests;
