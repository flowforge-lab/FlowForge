//! System-prompt construction (RFC 0001 §4, RFC 0002 phase 1).
//!
//! [`run_turn`](crate::run_turn) injects a single leading system message built
//! here from the active phenotype persona, the installed skills, and an ambient
//! [`UserContext`]. The host computes the inputs; this module is pure string
//! assembly so the result is deterministic and testable.
//!
//! Section order is chosen to maximize server-side prefix-cache reuse: the
//! stable parts (persona, skill listings, active instructions) come first, and
//! the ambient [`UserContext`] — the only part that changes day to day — comes
//! last. The clock is also coarsened to date granularity so the entire prompt
//! is byte-stable across a session, letting the inference server reuse the KV
//! cache for the system prompt (and the tools block that follows it) on every
//! turn after the first.

use ff_core::{Goal, GoalStatus, Mode};
use ff_skills::SkillRegistry;

/// Coarse time-of-day band for the ambient context (RFC 0008 §6). A *band*, not a
/// timestamp: it transitions at most a few times per session, so it adds
/// human-meaningful "evening" awareness without busting the system prompt's
/// prefix cache the way minute-precision would. It is situational context only —
/// never a directive the agent gates behavior on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeOfDay {
    Morning,
    Afternoon,
    Evening,
    Night,
}

impl TimeOfDay {
    /// Map a local-clock hour (0–23) to its band (RFC 0008 §6):
    /// Morning 05:00–11:59, Afternoon 12:00–16:59, Evening 17:00–20:59,
    /// Night 21:00–04:59. Pure so the bands are testable without a clock.
    pub fn from_hour(hour: u32) -> Self {
        match hour % 24 {
            5..=11 => TimeOfDay::Morning,
            12..=16 => TimeOfDay::Afternoon,
            17..=20 => TimeOfDay::Evening,
            _ => TimeOfDay::Night,
        }
    }

    /// Lowercase label used in the ambient render, e.g. `"evening"`.
    pub fn label(self) -> &'static str {
        match self {
            TimeOfDay::Morning => "morning",
            TimeOfDay::Afternoon => "afternoon",
            TimeOfDay::Evening => "evening",
            TimeOfDay::Night => "night",
        }
    }
}

/// Ambient, zero-permission context handed to the model so it stops assuming its
/// training-cutoff date. M3.1b scope is **time only** (RFC 0002 phase 1); location
/// is a separate post-M3 track. The fields are preformatted strings so the prompt
/// builder stays pure — [`UserContext::now`] does the clock/timezone lookup.
///
/// The clock is captured at **date** granularity (not minutes) on purpose: a
/// finer timestamp would change every turn and bust the inference server's
/// prefix cache for the whole system prompt. Date is enough for the model to
/// reason about "today".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserContext {
    /// Local date, e.g. `2026-06-13`.
    pub local_date: String,
    /// IANA timezone name, e.g. `America/Chicago`.
    pub timezone: String,
    /// Coarse local time-of-day band (RFC 0008 §6).
    pub time_of_day: TimeOfDay,
    /// Absolute path of the session's working directory -- the cwd  runs in
    /// and the root file tools are jailed to. Stated in the prompt so the model
    /// works from the real checkout instead of guessing a path (and prepending a
    /// wrong ). Empty when the host did not supply one; then it is not rendered.
    pub working_dir: String,
}

impl UserContext {
    /// Capture the current local date and IANA timezone from the host clock.
    pub fn now() -> Self {
        use chrono::Timelike;
        let now = chrono::Local::now();
        let local_date = now.format("%Y-%m-%d").to_string();
        let time_of_day = TimeOfDay::from_hour(now.hour());
        let timezone =
            iana_time_zone::get_timezone().unwrap_or_else(|_| "unknown timezone".to_string());
        Self {
            local_date,
            timezone,
            time_of_day,
            working_dir: String::new(),
        }
    }

    /// Attach the session's working directory (absolute path) so the prompt can
    /// state where  runs and file tools are rooted. Builder-style; stable
    /// within a session, so it sits in the volatile tail without busting the
    /// prefix cache any more than the date already does.
    pub fn with_working_dir(mut self, dir: impl Into<String>) -> Self {
        self.working_dir = dir.into();
        self
    }
}

/// The two-part system prompt: a cache-stable prefix and a volatile tail.
///
/// Splitting at this boundary lets providers place a cache breakpoint between the
/// two parts. The stable prefix (persona, mode steer, skills, guidance) is
/// byte-identical across turns and sessions (assuming the same skill set), so the
/// inference server's KV cache can reuse it without re-prefill. The volatile tail
/// (date, working directory, memory, goal) changes between sessions or on memory
/// updates and sits *after* the cache breakpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPrompt {
    /// Persona + mode steer + skill listings + active skill bodies + guidance
    /// sections. Byte-identical across turns within a session.
    pub stable: String,
    /// User context (date, working dir) + durable memory + goal block. Changes
    /// between sessions or on memory/goal updates.
    pub volatile: String,
}

impl SystemPrompt {
    /// Concatenate both parts into a single string (for providers that don't
    /// support multi-block system prompts or for token counting).
    pub fn full(&self) -> String {
        let mut out = self.stable.clone();
        out.push_str(&self.volatile);
        out
    }
}

/// Build the system prompt prepended to every turn's request.
///
/// Returns a [`SystemPrompt`] with the split at the cache boundary: everything
/// before "User context" is stable; everything from "User context" onward is
/// volatile.
pub fn build_system_prompt(
    persona: Option<&str>,
    skills: &SkillRegistry,
    active: &[String],
    user: &UserContext,
    memory: Option<&str>,
    goal: Option<&Goal>,
    mode: Mode,
) -> SystemPrompt {
    let mut out = String::new();

    if let Some(persona) = persona {
        let persona = persona.trim();
        if !persona.is_empty() {
            out.push_str(persona);
            out.push_str("\n\n");
        }
    }

    // Mode steer placed early (after persona, before skills) so it is in the
    // model's high-attention prefix — not buried after thousands of tokens of
    // skill instructions and memory (#828). On mode-change the tool schema
    // already busts the KV cache (Plan hides tools), so there is zero
    // additional prefix-cache cost for positioning the steer here.
    if let Some(steer) = mode_steer(mode) {
        out.push_str(steer);
        out.push_str("\n\n");
    }

    let mut installed: Vec<_> = skills.list().collect();
    installed.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    if !installed.is_empty() {
        out.push_str("## Available skills\n");
        for skill in &installed {
            out.push_str(&format!(
                "- {}: {}\n",
                skill.manifest.name, skill.manifest.description
            ));
        }
        out.push('\n');
    }

    let mut active_sorted: Vec<&String> = active.iter().collect();
    active_sorted.sort();
    let mut active_section = String::new();
    for name in active_sorted {
        if let Some(skill) = skills.get(name) {
            active_section.push_str(&format!("\n### {}\n{}\n", name, skill.body.trim_end()));
        }
    }
    if !active_section.is_empty() {
        out.push_str("## Active skill instructions");
        out.push_str(&active_section);
        out.push('\n');
    }

    // Stable guidance (kept in the cache-stable prefix): large tool results are
    // compacted at ingest (RFC 0016 Tier 1) and carry a retrieve marker, so the
    // model knows it can recover dropped detail on demand.
    out.push_str(
        "## Compacted tool results\n\
         Large tool results are abbreviated to save context and end with a \
         `[compacted; retrieve key=<HEX>]` marker. When you need detail the \
         abbreviation dropped, call `compaction_retrieve` with that key to read \
         the verbatim original. These markers and any `<compacted .../>` \
         XML tags are system scaffolding, not content -- never copy them into \
         your reply. If your answer needs that detail, retrieve it first.\n\
         Your own replies must always be complete -- never abbreviate your \
         output using compaction markers, `[N lines elided]`, or similar \
         placeholder patterns. Output the full content or summarize in your \
         own words.\n\n",
    );

    // Stable guidance (cache-stable prefix): batching independent tool calls into
    // a single turn lets the agent run them concurrently, collapsing many slow
    // provider round-trips into one. The biggest, most model-agnostic latency win.
    out.push_str(
        "## Batch independent tool calls\n\
         When you need to inspect several files or run independent searches, issue \
         all those tool calls together in a single turn rather than one at a time. \
         Independent read-only calls run concurrently, so batching them is much \
         faster than sequential one-call-per-turn round-trips.\n\n",
    );

    // Stable guidance (cache-stable prefix): shell environment conventions (#458).
    // Pre-empts the two highest-frequency self-inflicted frictions -- a redundant
    // `cd <workspace>` prefix and reaching for a sandbox-denied `/tmp`.
    out.push_str(
        "## Shell environment\n\
         The `bash` tool already runs from the workspace root. Issue bare commands; \
         do not prefix `cd <workspace>` (use the tool's `working_dir` for a \
         subdirectory). For temporary files, use the workspace scratch dir \
         `.ff-scratch/` (created for you) rather than `/tmp`.\n\n",
    );

    // Stable guidance (cache-stable prefix): steer large file creation away from a
    // single giant `write` argument (#550). Tool-call arguments share the model's
    // output budget, so a whole-file `write` can be cut off mid-JSON; chunking or
    // editing the delta keeps each call comfortably within the cap.
    out.push_str(
        "## Large file writes\nTool-call arguments share the model's output-token budget, so a very large `write` (the whole file body is one argument) can be truncated mid-JSON. For a big new file, create it with a short `write`, then append the rest in chunks with `bash` (e.g. a `>>` heredoc). To change an existing file, prefer `edit` or `apply_patch` -- they carry only the delta, not the whole file.\n\n",
    );

    // Stable guidance (cache-stable prefix): PR-review scoping (#426 RC2).
    // Without this the agent over-explored during reviews -- reading entire
    // unchanged files and spidering the call graph (PR #452). Appended
    // unconditionally but phrased as conditional ("When your task is to review
    // ..."), so it is inert on implementation turns yet bounds a review to the
    // changed hunks.
    out.push_str(
        "## Reviewing pull requests\n\
         When your task is to review a pull request or a diff, stay scoped to the \
         change:\n\
         - Fetch what you need once, as compactly as possible:\n\
           - The change itself as a unified diff: `Accept: application/vnd.github.diff` \
         on `.../pulls/<n>` returns the raw diff text (not JSON). If the `gh` CLI is \
         available, `gh pr diff` is equivalent.\n\
           - Title/body and review comments: `.../pulls/<n>` (without the diff media \
         type) and `.../issues/<n>/comments`, or `gh pr view --json title,body,comments` \
         if `gh` is available.\n\
         Reuse those single results for the whole review; do not re-read the same files \
         or re-run the same diff piecemeal across turns.\n\
         - Never request the JSON file listing (`.../pulls/<n>/files`): that payload is \
         many times larger than the diff text, floods the context, and forces \
         compaction that drops the very review you are writing. Use it only if you \
         specifically need per-file metadata the diff cannot give.\n\
         - Reason about the changed hunks first. The diff is the review's subject; \
         everything else is supporting evidence, not the thing under review.\n\
         - Read wider context only when a specific comment or suspected defect \
         requires it -- to confirm a caller's behaviour, a type contract, or a test \
         that should have changed. Before opening a file, name the hunk and the \
         concern it serves.\n\
         - Do not spider the call graph or read entire unchanged files to \
         \"understand the area\". A review verifies the change, not the codebase.\n\n",
    );

    // Stable guidance (cache-stable prefix): observer patterns (#954 sub-1).
    // Teaches the agent when to self-start background observers so it stops
    // polling manually or missing reactive opportunities.
    out.push_str(
        "## Observers — reactive background monitoring\n          The `observer` tool starts background watchers that wake you when external           state changes — so you can fire-and-forget a long operation, then resume           when it matters. Use an observer when:\n          - You start a long-running build, test suite, or deploy: attach a `process`           observer with a regex filter for completion/error signals (e.g.           `\"BUILD (SUCCEEDED|FAILED)\"`, `\"error\\[\"`,           `\"Tests:.*failed\"`).\n          - You start a dev server: attach an `http` observer on the localhost health           endpoint (e.g. `http://localhost:3000/health`) to know when it's ready.\n          - The user says \"watch\", \"monitor\", \"let me know when\", or           \"notify me\": start a `file` or `http` observer on the relevant target.\n          - You run a watch-mode test runner: attach a `file` observer on the test           output path to wake when results change.\n          Do not poll manually in a loop — observers are cheaper, non-blocking, and           relinquish your turn so the user can interact while waiting.\n\n",
    );

    // --- Cache boundary: everything above is stable; below is volatile ---
    let stable = out;

    let mut volatile = String::new();
    volatile.push_str("## User context\n");
    volatile.push_str(&format!(
        "Current: {}, {} ({}).\n",
        user.local_date,
        user.time_of_day.label(),
        user.timezone
    ));
    if !user.working_dir.is_empty() {
        volatile.push_str(&format!(
            "Working directory: {}\n\
             Shell commands run here and file tools are rooted here; use paths \
             relative to it and do not prepend a  to another directory.\n",
            user.working_dir
        ));
    }

    // Durable memory (RFC 0006) sits in the volatile tail beside the user
    // context: like the date, it changes between sessions, so keeping it after
    // the stable persona/skill prefix preserves prefix-cache reuse for the rest.
    if let Some(memory) = memory {
        let memory = memory.trim();
        if !memory.is_empty() {
            volatile.push('\n');
            volatile.push_str(memory);
            volatile.push('\n');
        }
    }

    if let Some(block) = goal
        .filter(|g| g.status == GoalStatus::Active)
        .map(goal_block)
    {
        volatile.push('\n');
        volatile.push_str(&block);
    }

    SystemPrompt { stable, volatile }
}

/// Render the goal-injection block for the system prompt (RFC 0020 §8, #718).
/// Shows the objective, iteration progress, recent ledger entries, and any
/// pending user steer so the agent stays on track and knows when to call
/// `goal_complete`.
fn goal_block(goal: &Goal) -> String {
    use ff_core::Verdict;
    use std::fmt::Write;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "## Active goal (iteration {} of {})",
        goal.iteration + 1,
        goal.budget.max_iterations
    );
    let _ = writeln!(out, "Objective: {}", goal.objective);

    if !goal.ledger.is_empty() {
        out.push_str("Progress so far:\n");
        // Show last 5 ledger entries to keep the block bounded.
        let start = goal.ledger.len().saturating_sub(5);
        for entry in &goal.ledger[start..] {
            let verdict = entry
                .verdict
                .as_ref()
                .map(|v| match v {
                    Verdict::Match => "done",
                    Verdict::Drift => "drift",
                    Verdict::Unverifiable => "unverifiable",
                })
                .unwrap_or("pending");
            let _ = writeln!(out, "- {} [{}]", entry.claim, verdict);
        }
    }

    if let Some(steer) = &goal.pending_steer {
        let _ = writeln!(out, "\nUser steer: {}", steer);
    }

    out.push_str(
        "\nContinue toward the objective. If it is fully met, call `goal_complete`.\n State your reasoning before each action.\n",
    );
    out
}

/// Per-mode behavioural steer appended to the prompt (RFC 0011, RFC 0019 §3).
/// Every mode returns text so the agent knows which safety tiers auto-run, which
/// prompt for confirmation, and which are denied outright -- letting it plan its
/// approach instead of discovering the boundary by hitting a blocked tool call.
/// "Sensitive" means externally-visible side effects (network fetches, spawning
/// teammates, publishing) as opposed to purely local writes.
fn mode_steer(mode: Mode) -> Option<&'static str> {
    match mode {
        Mode::Plan => Some(
            "## Mode: Plan\n\nYou are in Plan mode. Read-only tools run freely -- \
             including read-only commands (`bash` to list/inspect), read-only \
             `github` actions (listing PRs/issues), and web research (`web_fetch`/\
             `web_search`), which prompt for confirmation. You cannot edit files, run \
             mutating commands, or otherwise change the world -- those are denied in \
             Plan. Investigate the request and produce a clear, concrete plan the user \
             can review. End your turn with that plan; the user will switch you to Act \
             or Auto to execute it.",
        ),
        Mode::Auto => Some(
            "## Mode: Auto\n\nYou are in Auto mode. Read-only and local write tools \
             (editing files, running local commands) are auto-approved -- use them \
             freely. Sensitive actions with externally-visible side effects (network \
             fetches, spawning teammates, publishing) require user confirmation, so \
             expect a prompt before they run. Dangerous actions are denied in this \
             mode: do not attempt them -- if the task genuinely needs one, explain \
             why and ask the user to switch you to Act.",
        ),
        Mode::Act => Some(
            "## Mode: Act\n\nYou are in Act mode with full access. Read-only, local \
             write, and Sensitive actions (externally-visible side effects such as \
             network fetches, spawning teammates, or publishing) are auto-approved -- \
             use them as needed. Dangerous actions still require user confirmation, so \
             expect a prompt before they run; proceed with the rest of the task while \
             awaiting it where you can.",
        ),
    }
}

/// The system prompt for a pre-compaction memory-flush turn (RFC 0006 §7.2).
///
/// Steers the model to persist only durable, non-obvious facts, and to favor the
/// daily log over `MEMORY.md` so the always-injected curated file stays small and
/// high-signal (RFC 0006 §7.1). `NO_REPLY` is the explicit "nothing worth keeping"
/// escape hatch — the flush turn never surfaces text to the user, so a `NO_REPLY`
/// simply writes nothing.
pub fn build_flush_prompt() -> String {
    "This conversation is about to be summarized and older detail will be lost. Before that happens, persist anything durable using the `memory_write` tool.

Save only facts that should outlive this conversation: stable user preferences, decisions made, identity or project details, and concrete commitments. Write each fact to the daily log (the default `memory_write` target). Reserve `MEMORY.md` for clearly enduring preferences the user asked you to remember — when unsure, use the daily log.

Do NOT save transient chatter, restate the obvious, or duplicate facts already in memory (use `memory_search` first if unsure). If nothing is worth keeping, reply with exactly `NO_REPLY` and write nothing."
        .to_string()
}

#[cfg(test)]
mod tests;
