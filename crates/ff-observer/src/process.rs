//! `ProcessSource` — wakes the agent when a regex matches new bytes on the
//! stdout or stderr of a `ProcessSupervisor`-managed background process.
//! Phase 3 of the observer framework (#893).
//!
//! Construction wires a [`tokio::sync::broadcast::Receiver<ProcessChunk>`]
//! from the supervisor (which exposes only bytes appended *after* subscribe
//! — backfilling the ring buffer would re-fire old errors and is the wrong
//! default for a wake source). The source's `next_event` then selects
//! between that receiver and the supervisor's cancel signal.
//!
//! Per chunk: a single regex match yields exactly one `ObserverEvent` whose
//! `summary` is the matched text on one line, truncated to ~200 chars so a
//! multi-megabyte error log never blows the agent's context. Multiple
//! matches inside the same chunk (or chunks arriving while a previous
//! summary is still being dispatched) coalesce into a single event — fine
//! for a wake source, where one ring per change is the spec's contract.
//!
//! On process exit the supervisor's exit-watcher drops the broadcast
//! sender, so `rx.recv()` returns `RecvError::Closed`. Rather than vanish
//! silently, `next_event` emits ONE terminal `ObserverEvent` ("process
//! {pid} ended ...") to wake the agent — process end is the most
//! wake-worthy moment, and a watch that finishes without waking anyone is
//! the whole bug #1090 fixed. The next `next_event` call then returns
//! `None` (gated by `terminated`) so the supervisor reaps the entry. No
//! zombie observers. A user-initiated cancel still returns `None`
//! immediately and stays silent — only *process end* wakes.

use super::source::{ObserverContext, ObserverEvent, ObserverSource};
use async_trait::async_trait;
use ff_tools::process::{ProcessChunk, ProcessSupervisor};
use regex::Regex;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::Notify;

/// Truncation cap on the matched line in the event summary. A dev-server
/// stack trace or build error log is rarely useful past this; clamping
/// here keeps the wake text small enough to fold into a prompt without
/// dropping the matching signal.
const SUMMARY_MAX_CHARS: usize = 200;

/// Regex applied to each new chunk. The match is converted to a
/// single-line, char-clamped summary. Pre-compiled so a long-lived
/// observer pays no per-event setup cost.
pub struct ProcessSource {
    ctx: ObserverContext,
    /// Compiled with `(?m)` so the user can pass multi-line patterns.
    /// `None` means "match everything" (every chunk emits one event);
    /// not currently surfaced in the tool schema but kept for symmetry
    /// with the http/file sources.
    regex: Option<Regex>,
    rx: tokio::sync::broadcast::Receiver<ProcessChunk>,
    /// The observed `process_manager` id, used only to word the terminal
    /// wake ("process {pid} ended ...").
    pid: u64,
    /// The original (pre-`(?m)`) filter pattern, kept verbatim so the
    /// terminal wake can name it when the process ended without a match.
    pattern: Option<String>,
    /// Whether any chunk ever matched. Decides the terminal wake wording:
    /// a plain "ended" vs. "ended -- pattern never matched".
    matched_any: bool,
    /// Set once the observed process has ended (broadcast `Closed`) and we
    /// have emitted the single terminal wake. A second `Closed` then
    /// returns `None` so the supervisor reaps us instead of looping.
    terminated: bool,
}

impl std::fmt::Debug for ProcessSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessSource")
            .field("ctx", &self.ctx)
            .field("regex", &self.regex.as_ref().map(|r| r.as_str()))
            .finish_non_exhaustive()
    }
}

impl ProcessSource {
    /// Build a `ProcessSource` for `pid` (a `process_manager` id owned by
    /// `session_id`). `filter`, if present, is a regex applied to each new
    /// chunk; only matches produce events. `None` matches every chunk.
    ///
    /// Returns an error if:
    /// - the filter fails to compile as a regex (checked first, so a
    ///   bad pattern is reported even if the pid is also bogus);
    /// - the process is unknown, owned by a different session, or has
    ///   already exited (wording is `"no such process: {pid}"` to mirror
    ///   `process_manager poll` and hide cross-session ids).
    pub fn new(
        ctx: ObserverContext,
        pid: u64,
        filter: Option<&str>,
        supervisor: &ProcessSupervisor,
        session_id: &str,
    ) -> Result<Self, String> {
        // Compile the regex first so a bad pattern is reported even
        // when the pid is also bogus (e.g. tests that exercise the
        // regex-error path without a real process).
        let regex = match filter {
            Some(pat) => Some(
                Regex::new(&format!("(?m){pat}"))
                    .map_err(|e| format!("invalid filter regex '{pat}': {e}"))?,
            ),
            None => None,
        };
        if !supervisor.is_alive(pid, session_id) {
            return Err(format!("no such process: {pid}"));
        }
        let rx = supervisor
            .subscribe(pid, session_id)
            .ok_or_else(|| format!("no such process: {pid}"))?;
        Ok(Self {
            ctx,
            regex,
            rx,
            pid,
            pattern: filter.map(str::to_owned),
            matched_any: false,
            terminated: false,
        })
    }

    /// One-line, char-clamped summary of the regex match. Internal
    /// newlines become spaces so a multi-line match folds into a single
    /// readable line; trailing dots flag truncation.
    fn summarize(&self, text: &str) -> String {
        // Find the first match. We deliberately don't use `find_iter` —
        // one event per chunk is enough for a wake source, and the
        // contract for the http/file sources is also "one event per
        // detected change".
        let mat = match self.regex.as_ref() {
            Some(re) => match re.find(text) {
                Some(m) => m,
                None => return String::new(),
            },
            None => return text.to_string(),
        };
        let line = mat.as_str();
        // Collapse internal newlines to spaces so a multi-line match
        // never produces a multi-line summary (which would render
        // oddly in the wake block).
        let flat: String = line
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();
        if flat.chars().count() <= SUMMARY_MAX_CHARS {
            return flat;
        }
        let mut out: String = flat.chars().take(SUMMARY_MAX_CHARS).collect();
        out.push_str("...");
        out
    }
}

#[async_trait]
impl ObserverSource for ProcessSource {
    fn ctx(&self) -> &ObserverContext {
        &self.ctx
    }

    async fn next_event(&mut self, cancel: Arc<Notify>) -> Option<ObserverEvent> {
        // The observed process already ended and we emitted its single
        // terminal wake on the previous call; retire now so the supervisor
        // reaps us instead of looping on a closed channel.
        if self.terminated {
            return None;
        }
        loop {
            // `biased` so a cancel that arrives during a chunk-recv
            // is observed before the next chunk; same shape as the
            // http source.
            tokio::select! {
                biased;
                // User/agent stopped this observer: stay silent (no wake),
                // matching the http/file cancel contract.
                _ = cancel.notified() => return None,
                res = self.rx.recv() => match res {
                    // The observed process ended (broadcast sender dropped).
                    // This is the most wake-worthy moment, so emit ONE
                    // terminal event before retiring (#1090) rather than
                    // vanishing silently. `terminated` gates the next call
                    // to `None` so the supervisor reaps us.
                    Err(RecvError::Closed) => {
                        self.terminated = true;
                        let pid = self.pid;
                        let summary = match (&self.pattern, self.matched_any) {
                            // Ended after at least one match: neutral close.
                            (_, true) => format!("process {pid} ended"),
                            // Had a filter but it never matched before the
                            // process ended (killed, crashed, or the match
                            // was lost to a lagged broadcast): say so and
                            // point at `poll` for the full output.
                            (Some(pat), false) => format!(
                                "process {pid} ended -- wake pattern \"{pat}\" never matched; poll for full output"
                            ),
                            // No filter: every chunk would have matched, so
                            // "no match" is meaningless; just report the end.
                            (None, false) => format!("process {pid} ended"),
                        };
                        return Some(ObserverEvent {
                            session_id: self.ctx.session_id.clone(),
                            id: self.ctx.id,
                            label: self.ctx.label.clone(),
                            summary,
                        });
                    }
                    // A lagged receiver means the broadcast dropped
                    // chunks on overflow; loop and try the next one
                    // (the agent will see a single event for the
                    // newest surviving match — fine for a wake).
                    Err(RecvError::Lagged(_)) => continue,
                    Ok(chunk) => {
                        let text = String::from_utf8_lossy(&chunk.bytes);
                        let line = self.summarize(&text);
                        if line.is_empty() {
                            // No match in this chunk (and the source
                            // has a filter set). Keep draining.
                            continue;
                        }
                        self.matched_any = true;
                        return Some(ObserverEvent {
                            session_id: self.ctx.session_id.clone(),
                            id: self.ctx.id,
                            label: self.ctx.label.clone(),
                            summary: format!("matched: \"{line}\""),
                        });
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ObserverId;
    use ff_tools::process::ProcessSupervisor;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::Notify;
    fn ctx(id: ObserverId) -> ObserverContext {
        ObserverContext {
            session_id: "s1".into(),
            id,
            label: "proc-observer".into(),
        }
    }

    /// Start a long-running process under `cmd`, returning (supervisor, pid).
    /// Callers that need output after subscribe must gate the process themselves
    /// (file-gate) so CI load can't race bytes past the broadcast receiver.
    async fn live_proc(dir: &TempDir, cmd: &str) -> (Arc<ProcessSupervisor>, u64) {
        let sup = Arc::new(ProcessSupervisor::new());
        let id = sup.start(cmd, dir.path(), "s1").expect("start process");
        (sup, id)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn match_emits_with_summary() {
        let dir = TempDir::new().unwrap();
        // File-gate (same idea as #958): block the process until after we
        // subscribe, so CI load can't race the echo past the broadcast
        // receiver and leave `next_event` with a Closed channel.
        let gate = dir.path().join("gate");
        #[cfg(not(windows))]
        let cmd = format!(
            "while [ ! -f '{}' ]; do sleep 0.01; done; echo error detected",
            gate.display()
        );
        #[cfg(windows)]
        let cmd = format!(
            "while (-not (Test-Path '{}')) {{ Start-Sleep -Milliseconds 10 }}; Write-Output 'error detected'",
            gate.display().to_string().replace('\\', "/")
        );
        let (sup, id) = live_proc(&dir, &cmd).await;
        let mut src = ProcessSource::new(ctx(1), id, Some("error detected"), &sup, "s1")
            .expect("source builds");
        std::fs::write(&gate, "go").unwrap();
        let cancel = Arc::new(Notify::new());
        let ev = tokio::time::timeout(Duration::from_secs(3), src.next_event(cancel.clone()))
            .await
            .expect("event arrives within budget")
            .expect("source returns an event");
        assert_eq!(ev.session_id, "s1");
        assert_eq!(ev.id, 1);
        assert!(ev.summary.contains("error detected"), "{}", ev.summary);
        assert!(ev.summary.starts_with("matched:"), "{}", ev.summary);
        // Tidy up: stop the sleeper so the test can drop without
        // leaving a SIGKILL race.
        let _ = sup.stop(id, "s1").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ends_without_match_wakes_then_reaps() {
        // #1090: a process that exits WITHOUT ever matching the filter
        // must still wake the agent exactly once (a terminal event naming
        // the unmatched pattern), then return `None` so the supervisor
        // reaps it. This is the killed-mid-release scenario.
        let dir = TempDir::new().unwrap();
        let (sup, id) = live_proc(&dir, "sleep 0.3; echo all good").await;
        let mut src = ProcessSource::new(ctx(1), id, Some("error detected"), &sup, "s1")
            .expect("source builds");
        let cancel = Arc::new(Notify::new());
        // First call: the regex never matches; the process exits, the
        // sender drops, and we get the single terminal wake.
        let ev = tokio::time::timeout(Duration::from_secs(3), src.next_event(cancel.clone()))
            .await
            .expect("terminal event arrives within budget")
            .expect("process end must wake, not vanish");
        assert_eq!(ev.session_id, "s1");
        assert_eq!(ev.id, 1);
        assert!(ev.summary.contains("ended"), "{}", ev.summary);
        assert!(
            ev.summary.contains("never matched"),
            "must name the unmatched pattern: {}",
            ev.summary
        );
        assert!(
            ev.summary.contains("error detected"),
            "must quote the pattern: {}",
            ev.summary
        );
        // Second call: nothing left to say, so the source retires.
        let second = tokio::time::timeout(Duration::from_secs(1), src.next_event(cancel.clone()))
            .await
            .expect("second call returns within budget");
        assert!(second.is_none(), "expected reap after terminal wake");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ends_after_match_wakes_neutrally_then_reaps() {
        // #1090: a process that matched at least once, then ends, still
        // emits a terminal wake — but a neutral "ended" (no "never
        // matched" wording, since it did match).
        let dir = TempDir::new().unwrap();
        let gate = dir.path().join("gate");
        #[cfg(not(windows))]
        let cmd = format!(
            "while [ ! -f '{}' ]; do sleep 0.01; done; echo error detected",
            gate.display()
        );
        #[cfg(windows)]
        let cmd = format!(
            "while (-not (Test-Path '{}')) {{ Start-Sleep -Milliseconds 10 }}; Write-Output 'error detected'",
            gate.display().to_string().replace('\\', "/")
        );
        let (sup, id) = live_proc(&dir, &cmd).await;
        let mut src = ProcessSource::new(ctx(1), id, Some("error detected"), &sup, "s1")
            .expect("source builds");
        std::fs::write(&gate, "go").unwrap();
        let cancel = Arc::new(Notify::new());
        // First: the match.
        let matched = tokio::time::timeout(Duration::from_secs(3), src.next_event(cancel.clone()))
            .await
            .expect("match arrives")
            .expect("match event");
        assert!(
            matched.summary.starts_with("matched:"),
            "{}",
            matched.summary
        );
        // Then: the process ends -> terminal wake, neutral wording.
        let terminal = tokio::time::timeout(Duration::from_secs(3), src.next_event(cancel.clone()))
            .await
            .expect("terminal event arrives")
            .expect("process end must wake");
        assert!(terminal.summary.contains("ended"), "{}", terminal.summary);
        assert!(
            !terminal.summary.contains("never matched"),
            "matched run must not claim 'never matched': {}",
            terminal.summary
        );
        // Then: reaped.
        let third = tokio::time::timeout(Duration::from_secs(1), src.next_event(cancel.clone()))
            .await
            .expect("third call returns within budget");
        assert!(third.is_none(), "expected reap after terminal wake");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_stops_loop() {
        let dir = TempDir::new().unwrap();
        let (sup, id) = live_proc(&dir, "sleep 5").await;
        let mut src =
            ProcessSource::new(ctx(1), id, Some("never"), &sup, "s1").expect("source builds");
        let cancel = Arc::new(Notify::new());
        // Cancel almost immediately; `next_event` should return `None`
        // before the 5 s sleep elapses.
        let cancel_for_signal = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_for_signal.notify_waiters();
        });
        let res =
            tokio::time::timeout(Duration::from_secs(1), src.next_event(cancel.clone())).await;
        assert!(
            matches!(res, Ok(None)),
            "expected None after cancel, got {res:?}"
        );
        let _ = sup.stop(id, "s1").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_process_id_returns_none_at_source_new() {
        // The constructor must reject an unknown pid before any task
        // is spawned — i.e. a clean `Err`, not a panic.
        let sup = ProcessSupervisor::new();
        let err = ProcessSource::new(ctx(1), 999, None, &sup, "s1")
            .expect_err("unknown pid must error at construction");
        assert!(err.contains("no such process"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cross_session_pid_returns_error() {
        // The supervisor's `is_alive` / `subscribe` hide foreign
        // sessions; the source must surface the same wording.
        let dir = TempDir::new().unwrap();
        let sup = Arc::new(ProcessSupervisor::new());
        let id = sup.start("sleep 30", dir.path(), "session-a").unwrap();
        let err = ProcessSource::new(ctx(1), id, None, &sup, "session-b")
            .expect_err("foreign session must error");
        assert!(err.contains("no such process"), "{err}");
        let _ = sup.stop(id, "session-a").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invalid_filter_regex_rejected() {
        let sup = ProcessSupervisor::new();
        // Doesn't need a real process — regex compile happens before
        // any pid lookup.
        let err = ProcessSource::new(ctx(1), 1, Some("(unclosed"), &sup, "s1")
            .expect_err("bad regex must error");
        assert!(err.contains("invalid filter regex"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn multiline_match_across_chunks_emits() {
        // The regex with `(?m)` (set by the source) matches any line;
        // two separate `echo`s produce two chunks. Each chunk is its
        // own event because each call to `next_event` only consumes
        // one chunk before returning. Two file-gates keep the echoes
        // in separate chunks without relying on wall-clock sleeps (#958).
        let dir = TempDir::new().unwrap();
        let gate1 = dir.path().join("gate1");
        let gate2 = dir.path().join("gate2");
        #[cfg(not(windows))]
        let cmd = format!(
            "while [ ! -f '{g1}' ]; do sleep 0.01; done; echo first-error; \
             while [ ! -f '{g2}' ]; do sleep 0.01; done; echo second-error",
            g1 = gate1.display(),
            g2 = gate2.display()
        );
        #[cfg(windows)]
        let cmd = format!(
            "while (-not (Test-Path '{g1}')) {{ Start-Sleep -Milliseconds 10 }}; Write-Output 'first-error'; \
             while (-not (Test-Path '{g2}')) {{ Start-Sleep -Milliseconds 10 }}; Write-Output 'second-error'",
            g1 = gate1.display().to_string().replace('\\', "/"),
            g2 = gate2.display().to_string().replace('\\', "/")
        );
        let (sup, id) = live_proc(&dir, &cmd).await;
        let mut src =
            ProcessSource::new(ctx(1), id, Some(".*-error"), &sup, "s1").expect("source builds");
        let cancel = Arc::new(Notify::new());
        std::fs::write(&gate1, "go").unwrap();
        let ev1 = tokio::time::timeout(Duration::from_secs(3), src.next_event(cancel.clone()))
            .await
            .expect("first event arrives within budget")
            .expect("source returns an event");
        std::fs::write(&gate2, "go").unwrap();
        let ev2 = tokio::time::timeout(Duration::from_secs(3), src.next_event(cancel.clone()))
            .await
            .expect("second event arrives within budget")
            .expect("source returns a second event");
        assert!(ev1.summary.contains("first-error"), "{}", ev1.summary);
        assert!(ev2.summary.contains("second-error"), "{}", ev2.summary);
        let _ = sup.stop(id, "s1").await;
    }
}
