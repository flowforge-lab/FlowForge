//! Tracing subscriber installation (#1118).
//!
//! Extracted from the desktop crate to a shared crate in #1060, because the CLI
//! had the identical problem this module was written to solve: `flowforge serve`
//! installed no subscriber, so `RUST_LOG` did nothing and every `info!`/`warn!`
//! on the Slack path was discarded — including "router started" and the
//! allowlist-rejection warning. A running `serve` and a hung one were
//! indistinguishable from its output, which cost a full round of wrong theories
//! during the #1060 Slack acceptance run before anyone thought to check whether
//! a subscriber existed at all. The fix belongs in one place: both binaries now
//! call [`init`], and the env-var contract (`FF_LOG`, `FF_LOG_STDERR`) is the
//! same wherever you meet it.
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
//! Originally opt-in via env var and off by default. That was the wrong default
//! (#1118 problem 3): it asks the user to have predicted the failure *before* it
//! happened. The failures this module exists for — a background wake that never
//! surfaces, a scheduled turn that dies quietly — are exactly the ones you cannot
//! reproduce on demand: the observer has already fired, the schedule window has
//! passed. By the time anyone thinks to set `FF_LOG` and retry, the evidence is
//! gone. #1117 cost a day of wrong theories for precisely this reason.
//!
//! So there is now a floor: warnings and errors are always recorded.
//!
//! - Unset `FF_LOG` — floor of [`DEFAULT_DIRECTIVE`]. Quiet in normal operation;
//!   a real failure leaves a durable trace.
//! - `FF_LOG=<filter>` — override the floor entirely, in either direction
//!   (`FF_LOG=off` to silence, `FF_LOG=ff_observer=trace` to dig).
//! - `FF_LOG_STDERR=1` — also mirror to stderr, for `cargo tauri dev`.
//!
//! Logs are written to `<data_dir>/logs/flowforge.log`, rotated daily, beside
//! the existing `sessions.db` so a bug report can include both.
//!
//! # What always-on obliges us to get right
//!
//! Three things were harmless while this was opt-in and are not harmless as a
//! default, because the user did not ask for any of them:
//!
//! 1. **Unbounded growth.** Daily rotation with no cap is a slow disk leak on a
//!    desktop app nobody prunes by hand. Capped at [`MAX_LOG_FILES`].
//! 2. **World-readable files.** Logs name real paths and can name real hosts, so
//!    they are created `0o600` on unix (see [`restrict_permissions`]).
//! 3. **Provider error bodies.** A non-2xx body is read into `LlmError::Api`
//!    (`ff_llm::error_for_status_with_body`, 2 KB of it) and some providers echo
//!    request fragments back in a 400. Logging that verbatim by default would put
//!    slices of user conversations on disk, so `ff-transport`'s turn-failure site
//!    logs a classified summary and keeps the body at `debug`. That split is a
//!    contract, tested in `ff-transport`, not a convention.

use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Env var holding the filter directive. Unset = [`DEFAULT_DIRECTIVE`].
const FILTER_VAR: &str = "FF_LOG";
/// Env var to additionally mirror output to stderr.
const STDERR_VAR: &str = "FF_LOG_STDERR";

/// Filter applied when `FF_LOG` is unset: record failures, nothing else.
///
/// `warn` rather than `info` because this runs for every user on every launch.
/// `info` is where the workspace puts routine progress ("observer wake spawning
/// turn"), which is noise until you are already debugging — and noise that has to
/// be paid for in disk on someone else's machine. `warn` and above is the set
/// that means *something went wrong*, which is the set worth keeping without
/// being asked.
const DEFAULT_DIRECTIVE: &str = "warn";

/// Rotated files retained by the appender.
///
/// Seven days spans "it happened over the weekend" — the realistic gap between a
/// background failure and someone noticing — while bounding the footprint. At
/// the floor a quiet day writes nothing at all, so this is a ceiling, not a
/// budget.
const MAX_LOG_FILES: usize = 7;

/// Directory that receives rotated log files, under the app data dir.
pub fn log_dir(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("logs")
}

/// Install the process-wide tracing subscriber.
///
/// Returns the appender's [`WorkerGuard`], which **must be held for the life of
/// the process** — dropping it flushes and stops the writer thread, silently
/// ending all file logging. Returns `None` when logging is off — either because
/// the resolved filter is `off` (`FF_LOG=off`, the deliberate way to ask for
/// silence) or because the log directory can't be created. An unset `FF_LOG` does
/// *not* disable logging: it resolves to the [`FLOOR`] directive, which is the
/// point of #1118.
///
/// Safe to call once. A second call is ignored by `tracing`'s global-default
/// machinery (`try_init` errors), which we swallow: a duplicate install is a
/// caller bug, not a reason to fail a launch that is otherwise fine.
#[must_use = "dropping the guard stops file logging"]
pub fn init(data_dir: &std::path::Path) -> Option<WorkerGuard> {
    let directive = resolve_directive();

    // Make "off" mean off. Everything below has side effects a silenced app should
    // not pay: creating the log dir, opening (and so creating) today's file, and
    // spawning the non-blocking writer thread. Checked here rather than by letting
    // the filter drop events, because the empty file per day is the visible part.
    if directive_is_off(&directive) {
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

    let appender = match open_appender(&dir) {
        Ok(appender) => appender,
        Err(error) => {
            eprintln!(
                "flowforge: cannot open log file in {}: {error}",
                dir.display()
            );
            return None;
        }
    };
    // Wrapped so that each daily rotation re-applies the mode: the appender opens
    // new files itself with a hardcoded `create(true)` and no mode hook, so a
    // one-shot pass at startup would leave every later day's file at the umask
    // default. See [`ModeEnforcingAppender`].
    let appender = ModeEnforcingAppender::new(appender, dir.clone());
    let (writer, guard) = tracing_appender::non_blocking(appender);

    // After the appender has created today's file, before anything is written to
    // it. Best-effort: a failure here must not cost us the logs.
    restrict_permissions(&dir);

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
        "tracing subscriber installed (#1118)"
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

/// The filter directive to install: `FF_LOG` when set to something non-blank,
/// otherwise [`DEFAULT_DIRECTIVE`].
///
/// A blank `FF_LOG=` falls back to the floor rather than disabling logging, since
/// an empty string is far more likely to be an unset variable expanding in a shell
/// script than a deliberate request for silence. `FF_LOG=off` is the deliberate
/// way to ask for that, and it still works — `EnvFilter` understands it.
fn resolve_directive() -> String {
    match std::env::var(FILTER_VAR) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => DEFAULT_DIRECTIVE.to_string(),
    }
}

/// Whether `directive` silences everything, so [`init`] can skip its side effects.
///
/// Deliberately conservative: only a directive whose every comma-separated clause
/// is a bare `off` counts. `off,ff_agent=debug` is a real and useful shape — "quiet
/// except this one target" — and must still open a file, so anything with a
/// target-scoped clause is not global silence. Erring the other way would silently
/// discard logs someone asked for, which is far worse than an unused empty file.
fn directive_is_off(directive: &str) -> bool {
    let mut clauses = directive
        .split(',')
        .map(str::trim)
        .filter(|clause| !clause.is_empty())
        .peekable();
    // An empty directive is not silence. `resolve_directive` cannot produce one, but
    // defaulting to "off" for no input would make this a trap for any later caller:
    // `all()` over an empty iterator is vacuously true.
    clauses.peek().is_some() && clauses.all(|clause| clause.eq_ignore_ascii_case("off"))
}

/// Build the rolling file appender for `dir`.
///
/// Split out of [`init`] so the retention cap is observable. `init` installs a
/// process-global subscriber, which a test cannot do without fighting every other
/// test in the binary — leaving the cap unreachable and therefore untested, which
/// for a disk-growth guard is the same as not having one.
fn open_appender(
    dir: &std::path::Path,
) -> Result<tracing_appender::rolling::RollingFileAppender, tracing_appender::rolling::InitError> {
    tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("flowforge.log")
        .max_log_files(MAX_LOG_FILES)
        .build(dir)
}

/// Re-applies owner-only permissions when the appender rotates to a new file.
///
/// [`restrict_permissions`] runs once, over the files that exist at startup.
/// `tracing_appender` opens each new day's file itself, with a hardcoded
/// `OpenOptions::append(true).create(true)` and no mode hook or callback — so
/// without this, every file after the first day lands at the umask default
/// (commonly `0o644`) and the `0o600` guarantee quietly expires overnight. The
/// enclosing directory is `0o700`, so this is a defence-in-depth layer rather than
/// the only thing standing between the logs and another account, but a layer that
/// silently stops working is worse than one that was never claimed.
///
/// The write path is the hook: rotation is only observable as "the file under the
/// current name changed", and every rotation is necessarily preceded by a write.
/// Checking the date rather than stat-ing on every call keeps this to one
/// comparison per line; `restrict_permissions` is only invoked when the date turns
/// over, i.e. at most once a day.
#[cfg(unix)]
struct ModeEnforcingAppender<W> {
    inner: W,
    dir: std::path::PathBuf,
    current_day: Option<u64>,
}

#[cfg(unix)]
impl<W> ModeEnforcingAppender<W> {
    fn new(inner: W, dir: std::path::PathBuf) -> Self {
        // Seeded from now, since `init` has just applied the mode to today's file.
        Self {
            inner,
            dir,
            current_day: Self::today(),
        }
    }

    fn today() -> Option<u64> {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() / 86_400)
    }
}

#[cfg(unix)]
impl<W: std::io::Write> std::io::Write for ModeEnforcingAppender<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let today = Self::today();
        if today != self.current_day {
            self.current_day = today;
            // Write first, so the file the appender is about to roll to exists.
            let n = self.inner.write(buf)?;
            restrict_permissions(&self.dir);
            return Ok(n);
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Passthrough on Windows, where unix mode bits do not apply and the inherited
/// per-user app-data ACL already scopes the directory to the owner.
#[cfg(not(unix))]
struct ModeEnforcingAppender<W> {
    inner: W,
}

#[cfg(not(unix))]
impl<W> ModeEnforcingAppender<W> {
    fn new(inner: W, _dir: std::path::PathBuf) -> Self {
        Self { inner }
    }
}

#[cfg(not(unix))]
impl<W: std::io::Write> std::io::Write for ModeEnforcingAppender<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Tighten the log directory and its files to owner-only on unix.
///
/// Logs name filesystem paths (so, usernames and project names) and observer
/// targets (so, hosts). On a shared or multi-account machine the default umask
/// can leave that group- or world-readable. Best-effort by design: losing the
/// logs to a permissions failure would defeat the point of always-on logging, so
/// errors are reported to stderr and otherwise ignored.
///
/// No-op on Windows, where the ACL inherited from the per-user app-data directory
/// is already owner-scoped and unix mode bits do not apply.
#[cfg(unix)]
fn restrict_permissions(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let set = |path: &std::path::Path, mode: u32| {
        if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
            eprintln!("flowforge: cannot restrict {}: {error}", path.display());
        }
    };

    set(dir, 0o700);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.path().is_file() {
            set(&entry.path(), 0o600);
        }
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_dir: &std::path::Path) {}

#[cfg(test)]
mod tests;
