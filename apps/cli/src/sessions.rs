//! Pure helpers for the CLI session commands (`sessions list`, `fork`, `chat
//! --resume`), mirroring `apps/desktop/src/lib/sessions.ts` so the two surfaces
//! stay at label-resolution and fork-naming parity (#1080). Kept free of IO and
//! clap so the helpers are unit-testable in isolation — the command layer in
//! `main.rs` owns the store + dispatch, these own the rendering text and the
//! `(Fork N)` numbering algorithm.

use ff_core::Session;

/// The label a user sees for a session: persisted title > goal > fallback.
/// Mirrors `resolveLabel` in `apps/desktop/src/lib/sessions.ts` so the CLI's
/// `sessions list` shows the same text the desktop sidebar would.
pub fn resolve_label(session: &Session) -> &str {
    if let Some(title) = &session.title {
        if !title.is_empty() {
            return title;
        }
    }
    if let Some(goal) = &session.goal {
        if !goal.is_empty() {
            return goal;
        }
    }
    "New session"
}

/// Strip a trailing " (Fork <k>)" suffix (#1069) so forking a fork renumbers
/// from the original base title instead of stacking suffixes. Mirrors
/// `stripForkSuffix` in `apps/desktop/src/lib/sessions.ts`. Hand-rolled (no
/// `regex` dependency) to match the codebase's YAGNI stance — the pattern is
/// simple enough for literal string ops.
pub fn strip_fork_suffix(title: &str) -> &str {
    // Find the last " (Fork " and check it's followed by digits + ")" at the end.
    if let Some(pos) = title.rfind(" (Fork ") {
        let after = &title[pos + " (Fork ".len()..];
        if let Some(num) = after.strip_suffix(')') {
            if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
                return &title[..pos];
            }
        }
    }
    title
}

/// If `title` is exactly `<base> (Fork <N>)`, return `N`; otherwise `None`.
/// Used by [`next_fork_title`] to find the highest existing fork number
/// sharing a base. Literal string matching (no regex escaping needed, unlike
/// the desktop's TS port — `starts_with` + suffix check is exact).
fn match_fork_number(title: &str, base: &str) -> Option<u32> {
    if !title.starts_with(base) {
        return None;
    }
    let suffix = &title[base.len()..];
    let rest = suffix.strip_prefix(" (Fork ")?;
    let num_str = rest.strip_suffix(')')?;
    num_str.parse::<u32>().ok()
}

/// Compute the next "<base> (Fork N)" title for a session forked from
/// `source_title` (#1069). `base` strips any existing "(Fork k)" suffix from
/// the source so forking a fork stays on the same base and keeps numbering
/// contiguous; `N` is one past the highest existing "(Fork N)" sharing that
/// base among `existing_titles` (the in-memory session list), or 1 if none.
/// Mirrors `nextForkTitle` in `apps/desktop/src/lib/sessions.ts`.
pub fn next_fork_title(source_title: &str, existing_titles: &[Option<&str>]) -> String {
    let base = strip_fork_suffix(source_title);
    let mut max = 0u32;
    for t in existing_titles.iter().flatten() {
        if let Some(n) = match_fork_number(t, base) {
            max = max.max(n);
        }
    }
    format!("{base} (Fork {})", max + 1)
}

/// Format an epoch-millis timestamp as a short local-time string for `sessions
/// list`. Falls back to the raw millis if the timestamp is out of chrono's
/// representable range (same resilience as `fmt_ts` in `ff-session/src/lib.rs`).
pub fn format_ts(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| ms.to_string())
}

/// Render a list of sessions as TSV (id, label, status, updated-at) to `out`,
/// most-recently-updated first (the store's `list_sessions` order). One header
/// row, then one row per session. Mirrors `ff config list`'s machine-parseable
/// convention. Pure over `(sessions, writer)` so the rendering is unit-testable
/// without a real store or stdout capture.
pub fn render_list(sessions: &[Session], out: &mut impl std::io::Write) -> std::io::Result<()> {
    writeln!(out, "id\tlabel\tstatus\tupdated")?;
    for s in sessions {
        writeln!(
            out,
            "{}\t{}\t{}\t{}",
            s.id,
            resolve_label(s),
            status_label(s.status),
            format_ts(s.updated_at),
        )?;
    }
    Ok(())
}

/// Map a `SessionStatus` to the short lowercase label used in `sessions list`,
/// mirroring the serde `rename_all = "lowercase"` on the enum so the wire form
/// and the CLI text agree.
fn status_label(status: ff_core::SessionStatus) -> &'static str {
    match status {
        ff_core::SessionStatus::Active => "active",
        ff_core::SessionStatus::Done => "done",
        ff_core::SessionStatus::Abandoned => "abandoned",
    }
}

#[cfg(test)]
mod tests;
