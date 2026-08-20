//! Interactive pseudo-terminals for the embedded terminal drawer (#1284).
//!
//! The app could already *run* commands -- the `bash` tool, `process_manager`,
//! the shell plugin -- but every one of those is one-shot and captured: no TTY,
//! so no line editing, no `clear`, no colors, no `vim`, no resize. This module
//! is the missing primitive: a real PTY per drawer tab, spawned with
//! `cwd = the session's working directory` (the same `session_root` the
//! composer's workspace selector shows), so the shell opens where the agent
//! works.
//!
//! ## Shape
//!
//! [`Terminals`] is a **map keyed by terminal id**, managed alongside `AppState`.
//! Every reference implementation of a Tauri PTY we looked at keeps one global
//! `PtyPair`; that cannot express this UI, where each pane has its own drawer
//! with its own tabs and each tab is rooted at *that pane's* cwd.
//!
//! ## Why a `Channel` and not `app.emit`
//!
//! Output streams over a [`tauri::ipc::Channel`], unlike `process:output`
//! (`lib.rs`), which is an `app.emit` event. Tauri's own docs say the event
//! system is "not designed for low latency or high throughput" and point at
//! channels for streaming -- and terminal output is the high-throughput case
//! (`yes`, a build log, `cat` on a large file). Bytes are sent as
//! [`InvokeResponseBody::Raw`], which reaches JavaScript as an `ArrayBuffer`
//! rather than a JSON number array, so a UTF-8 sequence split across two reads
//! is reassembled by xterm's decoder instead of being mangled by a lossy
//! `String::from_utf8_lossy` on this side.
//!
//! The *exit* of a shell is the opposite kind of signal -- one per terminal,
//! for the whole app -- so it stays an event (`terminal:exited`), exactly like
//! `process:exited`.
//!
//! ## Lifetime
//!
//! A shell is killed when its tab closes ([`close_terminal`]), and when its
//! session is deleted ([`Terminals::reap_session`], called from
//! `delete_session`). The reader thread owns teardown for the natural case (the
//! user types `exit`): on EOF it removes the entry, reaps the child, and emits
//! `terminal:exited`. No path leaves an orphaned shell behind.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ff_core::events::TerminalExitedEvent;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter, State};

use crate::state::AppState;

/// Read buffer for the PTY reader thread. Terminal output arrives in bursts far
/// smaller than this for interactive use; a full buffer just means one channel
/// message per 8KB during a flood (`cat` on a big file), which is the case the
/// channel exists for.
const READ_BUF: usize = 8192;

/// One live shell. The `master` is kept alive for its whole life: dropping it
/// closes the PTY, which is what makes the reader thread see EOF.
struct Terminal {
    /// The session whose working directory this shell was spawned in. Only used
    /// to scope [`Terminals::reap_session`] -- a terminal is otherwise addressed
    /// by its own id.
    session_id: String,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

/// Every open terminal, keyed by terminal id. Managed state, registered in
/// `lib.rs` next to `AppState`.
///
/// Deliberately *not* a field on `AppState`: a PTY handle is desktop-window
/// state with a thread attached, and keeping it here means `state.rs` stays free
/// of `portable-pty` types while `delete_session` can still reap through the
/// single `reap_session` entry point.
#[derive(Default)]
pub struct Terminals(Arc<Mutex<HashMap<String, Terminal>>>);

impl Terminals {
    /// Spawn a shell rooted at `cwd` and register it (#1284).
    ///
    /// Returns the new terminal's id and its output reader -- the reader is
    /// handed back rather than consumed here so the *pump* (which needs a Tauri
    /// `AppHandle` and a channel) stays out of this type, leaving the whole
    /// spawn/register/kill lifecycle testable without a running app.
    pub(crate) fn open(
        &self,
        session_id: &str,
        cwd: &Path,
        cols: u16,
        rows: u16,
        shell: &str,
    ) -> Result<(String, Box<dyn Read + Send>), String> {
        let pair = native_pty_system()
            .openpty(pty_size(cols, rows))
            .map_err(|e| format!("open pty: {e}"))?;

        let mut cmd = CommandBuilder::new(shell);
        cmd.cwd(cwd);
        // Without a `TERM` the shell assumes a dumb terminal: no colors, and
        // `clear`/`vim`/`htop` refuse to draw. xterm.js speaks xterm-256color.
        cmd.env("TERM", "xterm-256color");
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawn {shell}: {e}"))?;
        // The slave handle has done its job. Holding it keeps the PTY open after
        // the shell exits, so the reader would never see EOF and a dead tab
        // would sit there looking alive.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("pty reader: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("pty writer: {e}"))?;

        let terminal_id = uuid::Uuid::new_v4().to_string();
        self.0.lock().unwrap().insert(
            terminal_id.clone(),
            Terminal {
                session_id: session_id.to_string(),
                writer,
                master: pair.master,
                child,
            },
        );
        Ok((terminal_id, reader))
    }

    /// Write keystrokes to a terminal's shell. `Err` for an unknown id.
    pub(crate) fn write(&self, terminal_id: &str, data: &[u8]) -> Result<(), String> {
        let mut map = self.0.lock().unwrap();
        let terminal = map
            .get_mut(terminal_id)
            .ok_or_else(|| format!("no such terminal: {terminal_id}"))?;
        terminal
            .writer
            .write_all(data)
            .and_then(|()| terminal.writer.flush())
            .map_err(|e| format!("write: {e}"))
    }

    /// Tell a shell its window changed size. `Err` for an unknown id.
    pub(crate) fn resize(&self, terminal_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let map = self.0.lock().unwrap();
        let terminal = map
            .get(terminal_id)
            .ok_or_else(|| format!("no such terminal: {terminal_id}"))?;
        terminal
            .master
            .resize(pty_size(cols, rows))
            .map_err(|e| format!("resize: {e}"))
    }

    /// Kill one terminal's shell and forget it. `false` when the id was already
    /// gone, which is not an error -- see [`terminal_close`].
    pub(crate) fn close(&self, terminal_id: &str) -> bool {
        // Take the entry out under the lock, then tear it down with the lock
        // released: the reader thread needs the same lock to clean up after the
        // EOF our kill causes, and holding it here would deadlock the two.
        let taken = self.0.lock().unwrap().remove(terminal_id);
        match taken {
            Some(terminal) => {
                let Terminal {
                    writer,
                    master,
                    mut child,
                    ..
                } = terminal;
                // Close our side of the PTY *before* signalling. A shell that is
                // exiting while we still hold its terminal open can sit in the
                // kernel's exit path indefinitely -- keeping the master alive
                // across the reap below is what makes `wait()` block forever
                // (measured, not theorized: it is why this is spelled out).
                drop(writer);
                drop(master);
                let _ = child.kill();
                // Reap on a detached thread rather than here. The caller is a UI
                // command; how fast a shell winds down is up to the shell (a
                // SIGHUP handler, an `atexit`, a `trap`), and none of that may
                // stall the window. The thread is short-lived and its only job is
                // to keep the child from lingering as a zombie.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                true
            }
            None => false,
        }
    }

    /// A second handle on the same map, for the reader threads. `Terminals`
    /// itself is managed state and can only be borrowed from `State`, so a
    /// thread that must outlive the command borrows this instead.
    fn handle(&self) -> Terminals {
        Terminals(self.0.clone())
    }

    /// How many terminals are open. Test-facing; the UI tracks its own tabs.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    /// Kill and drop every terminal belonging to `session_id` (#1284).
    ///
    /// Called from `delete_session` alongside `reap_session_processes` /
    /// `reap_session_kernels`. Synchronous and safe to call from a sync Tauri
    /// command: killing is a signal, not an await, so unlike the process/kernel
    /// reaps there is no runtime to enter (a bare `tokio::spawn` in a sync
    /// command panics off-reactor on macOS -- #117/#471). Returns how many were
    /// reaped, for the log line.
    pub fn reap_session(&self, session_id: &str) -> usize {
        let ids: Vec<String> = self
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, t)| t.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect();
        ids.iter().filter(|id| self.close(id)).count()
    }
}

/// A [`PtySize`] from a cols/rows pair, flooring both at 1: the frontend can
/// legitimately measure 0 columns for a drawer that is laid out but not yet
/// painted, and a zero-sized PTY makes shells misbehave.
fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Open an interactive shell rooted at `session_id`'s working directory (#1284).
///
/// Returns the terminal id the other three commands address. `on_output` is the
/// frontend's channel; raw PTY bytes are pushed to it by a dedicated reader
/// thread until the shell exits.
#[tauri::command]
pub fn terminal_open(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    terminals: State<'_, Terminals>,
    session_id: String,
    cols: u16,
    rows: u16,
    on_output: Channel<InvokeResponseBody>,
) -> Result<String, String> {
    // The single source of truth for "where this session works" (#200/#211) --
    // absolute and symlink-resolved, the same value `get_session_workspace`
    // returns and the workspace selector displays. Never re-derived here.
    let cwd = state.session_root(&session_id);
    let shell = resolve_shell(std::env::var("SHELL").ok(), which);
    let (terminal_id, reader) = terminals.open(&session_id, &cwd, cols, rows, &shell)?;

    spawn_reader(
        app,
        terminals.handle(),
        terminal_id.clone(),
        session_id,
        reader,
        on_output,
    );

    tracing::info!(terminal_id = %terminal_id, cwd = %cwd.display(), shell = %shell, "terminal opened");
    Ok(terminal_id)
}

/// Send keystrokes to a terminal's shell (#1284). `data` is what xterm's
/// `onData` produced -- printable text, control bytes, escape sequences.
#[tauri::command]
pub fn terminal_write(
    terminals: State<'_, Terminals>,
    terminal_id: String,
    data: String,
) -> Result<(), String> {
    terminals.write(&terminal_id, data.as_bytes())
}

/// Tell the shell its window changed size (#1284), so it re-wraps and full-screen
/// programs redraw. Driven by the frontend's `ResizeObserver` -> `fit()`.
#[tauri::command]
pub fn terminal_resize(
    terminals: State<'_, Terminals>,
    terminal_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    terminals.resize(&terminal_id, cols, rows)
}

/// Kill a terminal's shell and forget it (#1284) -- the tab was closed.
///
/// Idempotent: closing an already-gone terminal (its shell exited on its own and
/// the reader thread removed it) is a no-op, not an error, because the frontend
/// legitimately races `terminal:exited` against the user clicking `x`.
#[tauri::command]
pub fn terminal_close(terminals: State<'_, Terminals>, terminal_id: String) -> Result<(), String> {
    if terminals.close(&terminal_id) {
        tracing::debug!(terminal_id = %terminal_id, "terminal closed");
    }
    Ok(())
}

/// Pump one PTY's output to the frontend until the shell exits.
///
/// A plain OS thread, not a tokio task: reading a PTY master is a *blocking*
/// `Read` with no async equivalent, so parking a reactor thread on it is exactly
/// what we must not do.
fn spawn_reader(
    app: AppHandle,
    terminals: Terminals,
    terminal_id: String,
    session_id: String,
    mut reader: Box<dyn Read + Send>,
    on_output: Channel<InvokeResponseBody>,
) {
    std::thread::spawn(move || {
        let mut buf = [0u8; READ_BUF];
        loop {
            match reader.read(&mut buf) {
                // EOF: the shell exited (`exit`, or we killed it).
                Ok(0) => break,
                Ok(n) => {
                    // A dead channel means the webview dropped it (page reload,
                    // window closed) -- stop reading, and let the teardown below
                    // kill the now-unobservable shell.
                    if on_output
                        .send(InvokeResponseBody::Raw(buf[..n].to_vec()))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    tracing::debug!(terminal_id = %terminal_id, error = %e, "terminal read ended");
                    break;
                }
            }
        }

        // Teardown, whichever way we got here. `terminal_close` may have removed
        // the entry already (it killed the shell, which is why we saw EOF); then
        // this is a no-op and the child was reaped there.
        terminals.close(&terminal_id);
        // One event per terminal for its whole life -- the low-frequency signal
        // the event system *is* built for, unlike the byte stream above.
        let _ = app.emit(
            "terminal:exited",
            TerminalExitedEvent {
                session_id,
                terminal_id,
            },
        );
    });
}

/// The shell to spawn, given `$SHELL` and a PATH lookup.
///
/// `$SHELL` is the user's own choice and wins whenever it is set and non-empty.
/// The fallbacks below it are ordered by what a user of a given platform expects
/// to get, not by what is likeliest to exist: `sh` and `cmd.exe` are the
/// last-resort rungs precisely because landing there is a worse experience.
///
/// Takes both inputs as parameters rather than reading the environment itself so
/// the ordering is unit-testable without mutating process-global state (which
/// races every other test in the binary).
pub(crate) fn resolve_shell(
    env_shell: Option<String>,
    lookup: impl Fn(&str) -> Option<PathBuf>,
) -> String {
    if let Some(shell) = env_shell.filter(|s| usable_env_shell(s)) {
        return shell;
    }
    let candidates: &[&str] = if cfg!(windows) {
        &["pwsh.exe", "powershell.exe"]
    } else {
        &["zsh", "bash"]
    };
    for name in candidates {
        if let Some(path) = lookup(name) {
            return path.display().to_string();
        }
    }
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    } else {
        "/bin/sh".to_string()
    }
}

/// Whether `$SHELL` names something this platform can actually spawn.
///
/// Empty is out everywhere. On Windows a POSIX-looking path is out too (#1286
/// review): Git-for-Windows exports `SHELL=/usr/bin/bash` into environments that
/// have no such program, and handing that to ConPTY fails to open a terminal at
/// all. Falling through to `pwsh`/`powershell` gives the user a working shell
/// instead of an error, which is the point of honouring `$SHELL` in the first
/// place. A Windows-shaped value (`C:\...`, or a bare `bash.exe` on PATH) is
/// still honoured.
fn usable_env_shell(shell: &str) -> bool {
    let shell = shell.trim();
    if shell.is_empty() {
        return false;
    }
    !cfg!(windows) || !shell.starts_with('/')
}

/// First executable named `name` on `PATH`, or `None`. A three-line stand-in for
/// the `which` crate -- not worth a dependency for one call site.
fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
