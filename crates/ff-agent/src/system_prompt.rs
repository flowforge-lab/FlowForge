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

use ff_core::{Goal, GoalStatus, Mode, Verdict};
use ff_skills::SkillRegistry;
use minijinja::{Environment, Value};
use serde::Serialize;
use std::sync::LazyLock;

/// Process-wide template environment, compiled from the inline `.jinja` files
/// once at first use. The bodies are `include_str!`'d at build time, so the
/// deployed binary carries both templates without any runtime fs dependency and
/// `LocalLock` initialization cost is paid exactly once per process.
static TEMPLATES: LazyLock<Environment<'static>> = LazyLock::new(|| {
    let mut env = Environment::new();
    env.add_template("stable.jinja", include_str!("system_prompt/stable.jinja"))
        .expect("stable.jinja must parse");
    env.add_template(
        "volatile.jinja",
        include_str!("system_prompt/volatile.jinja"),
    )
    .expect("volatile.jinja must parse");
    env
});

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
/// Build the system prompt prepended to every turn's request.
///
/// Returns a [`SystemPrompt`] with the split at the cache boundary: everything
/// before "User context" is stable; everything from "User context" onward is
/// volatile. The two halves are rendered from separate `minijinja` templates
/// (`stable.jinja` / `volatile.jinja`) so the split is a first-class value
/// rather than a comment-marked slice. Data shaping (skill sorting, registry
/// resolution, verdict labeling, empty-memory/working-dir folding) lives here;
/// literal prompt copy lives in the templates. See issue #938.
#[allow(clippy::too_many_arguments)]
pub fn build_system_prompt(
    persona: Option<&str>,
    skills: &SkillRegistry,
    active: &[String],
    user: &UserContext,
    memory: Option<&str>,
    extra_instructions: Option<&str>,
    goal: Option<&Goal>,
    mode: Mode,
) -> SystemPrompt {
    let mode_steer = mode_steer(mode).unwrap_or("");

    // Active skill bodies: registry-resolved, sorted by name. Names that don't
    // resolve are dropped (matches legacy push_str behaviour).
    let mut active_sorted: Vec<&String> = active.iter().collect();
    active_sorted.sort();
    let active_entries: Vec<ActiveEntry<'_>> = active_sorted
        .iter()
        .filter_map(|name| {
            skills.get(name).map(|skill| ActiveEntry {
                name,
                body: skill.body.trim_end(),
            })
        })
        .collect();

    // Installed-skill listing: sorted by name.
    let mut installed: Vec<_> = skills.list().collect();
    installed.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    let skill_entries: Vec<SkillEntry<'_>> = installed
        .iter()
        .map(|s| SkillEntry {
            name: &s.manifest.name,
            description: &s.manifest.description,
        })
        .collect();

    let stable_ctx = StableCtx {
        persona: persona.map(|p| p.trim()).filter(|p| !p.is_empty()),
        mode_steer,
        skills: skill_entries,
        active: active_entries,
    };

    let active_goal: Option<GoalCtx<'_>> =
        goal.filter(|g| g.status == GoalStatus::Active)
            .map(|g| GoalCtx {
                iteration: g.iteration,
                max_iterations: g.budget.max_iterations,
                objective: g.objective.as_str(),
                // Bound the ledger to the last 5 entries so the volatile tail
                // stays bounded across iterations (legacy `goal_block` contract;
                // see goal_block_caps_ledger_to_last_five regression test).
                ledger: {
                    let start = g.ledger.len().saturating_sub(5);
                    g.ledger[start..]
                        .iter()
                        .map(|e| LedgerEntry {
                            claim: e.claim.as_str(),
                            verdict: verdict_label(e.verdict.as_ref()),
                        })
                        .collect()
                },
                pending_steer: g.pending_steer.as_deref(),
            });

    let volatile_ctx = VolatileCtx {
        date: user.local_date.as_str(),
        time_of_day: user.time_of_day.label(),
        timezone: user.timezone.as_str(),
        working_dir: if user.working_dir.is_empty() {
            None
        } else {
            Some(user.working_dir.as_str())
        },
        memory: memory.map(|m| m.trim()).filter(|m| !m.is_empty()),
        extra_instructions: extra_instructions
            .map(|m| m.trim())
            .filter(|m| !m.is_empty()),
        goal: active_goal,
    };

    let stable = render_or_panic("stable.jinja", &stable_ctx);
    let volatile = render_or_panic("volatile.jinja", &volatile_ctx);

    SystemPrompt { stable, volatile }
}

fn render_or_panic<T: Serialize>(name: &str, ctx: &T) -> String {
    let tmpl = TEMPLATES
        .get_template(name)
        .unwrap_or_else(|_| panic!("template {name} not registered"));
    let value = Value::from_serialize(ctx);
    tmpl.render(value)
        .unwrap_or_else(|e| panic!("rendering {name} failed: {e}"))
}

/// Map a ledger entry verdict to the label printed in the goal block. Mirrors
/// the legacy `goal_block` helper 1:1 so the rendered prompt is byte-stable.
fn verdict_label(verdict: Option<&Verdict>) -> &'static str {
    match verdict {
        Some(Verdict::Match) => "done",
        Some(Verdict::Drift) => "drift",
        Some(Verdict::Unverifiable) => "unverifiable",
        None => "pending",
    }
}

#[derive(Serialize)]
struct StableCtx<'a> {
    persona: Option<&'a str>,
    mode_steer: &'a str,
    skills: Vec<SkillEntry<'a>>,
    active: Vec<ActiveEntry<'a>>,
}

#[derive(Serialize)]
struct VolatileCtx<'a> {
    date: &'a str,
    time_of_day: &'a str,
    timezone: &'a str,
    working_dir: Option<&'a str>,
    memory: Option<&'a str>,
    extra_instructions: Option<&'a str>,
    goal: Option<GoalCtx<'a>>,
}

#[derive(Serialize)]
struct SkillEntry<'a> {
    name: &'a str,
    description: &'a str,
}

#[derive(Serialize)]
struct ActiveEntry<'a> {
    name: &'a str,
    body: &'a str,
}

#[derive(Serialize)]
struct GoalCtx<'a> {
    iteration: u32,
    max_iterations: u32,
    objective: &'a str,
    ledger: Vec<LedgerEntry<'a>>,
    pending_steer: Option<&'a str>,
}

#[derive(Serialize)]
struct LedgerEntry<'a> {
    claim: &'a str,
    verdict: &'a str,
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
