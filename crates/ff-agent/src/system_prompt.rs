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

/// Per-server cap on injected MCP `initialize` guidance (#1173).
///
/// 1.76x the largest value measured across the three configured servers
/// (codegraph 4653 B, builder-mcp 918 B, obsidian none), so real guidance fits
/// and a runaway server does not.
pub const MAX_MCP_INSTRUCTIONS_BYTES: usize = 8 * 1024;

/// Cap across *all* servers' guidance combined.
///
/// A per-server cap alone does not bound server *count*: three servers at the
/// per-server cap would already be ~6700 tokens of stable prefix.
pub const MAX_MCP_INSTRUCTIONS_TOTAL_BYTES: usize = 16 * 1024;

const MCP_TRUNCATION_MARKER: &str =
    "\n[... truncated: server guidance exceeded the injection budget]";

/// Fit one server's guidance to `budget` bytes, truncating on a char boundary.
///
/// Truncates rather than dropping because the highest-value content is up front:
/// codegraph's "there is a single tool" sits in its opening section, and dropping
/// the whole block to save its tail loses the part that mattered. Callers get a
/// visible marker so the model can tell "the server said this much" from "the
/// server stopped mid-sentence".
///
/// This deliberately does **not** follow `MAX_EXTRA_INSTRUCTIONS_BYTES`, which
/// warns and injects anyway. That policy's stated reason is honoring *the user's*
/// explicit instruction; MCP guidance is text from a third-party process the user
/// never read, so the premise does not transfer.
fn fit_mcp_instructions(text: &str, budget: usize) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || budget == 0 {
        return None;
    }
    if text.len() <= budget {
        return Some(text.to_string());
    }
    // Reserve room for the marker; if the budget cannot even hold the marker,
    // keep the prefix and let the marker be what gets dropped.
    let body_budget = budget.saturating_sub(MCP_TRUNCATION_MARKER.len());
    if body_budget == 0 {
        return None;
    }
    // Byte slicing must land on a char boundary or this panics on any
    // multi-byte character.
    let mut end = body_budget.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    Some(format!("{}{}", &text[..end], MCP_TRUNCATION_MARKER))
}

/// One server's guidance, already admitted and ready to inject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpGuidance {
    /// The server id, shown to the model so it can attribute the advice.
    pub server: String,
    /// The server's `instructions`, as sent.
    pub text: String,
}

/// Whether a server's guidance is worth injecting: can the model reach its tools
/// this turn?
///
/// Two independent ways in, and missing either one is a silent bug:
///
/// - `standing` -- the server has at least one non-deferred tool, so its tools are
///   in the block from turn one and no admission ever happens for them.
/// - `admitted` -- a deferred server whose tools `tool_search` has since admitted.
///
/// Gating on `admitted` alone permanently suppresses the guidance of every
/// `defer = false` server, which is precisely the set an operator opted into
/// keeping resident. Gating on neither injects advice for tools the model cannot
/// call.
///
/// `admitted` holds bridged names (`mcp__<server>__<tool>`); the match is by
/// prefix from a known server id, never by splitting a name, because the bridge
/// leaves the server segment verbatim and an id containing `__` has no
/// unambiguous split.
pub fn server_guidance_is_reachable(
    server_id: &str,
    standing_server_ids: &std::collections::HashSet<String>,
    admitted_tool_names: &std::collections::HashSet<String>,
) -> bool {
    if standing_server_ids.contains(server_id) {
        return true;
    }
    let prefix = format!("mcp__{server_id}__");
    admitted_tool_names.iter().any(|n| n.starts_with(&prefix))
}

/// Apply both caps to the admitted servers' guidance, in the order given.
///
/// Order is the caller's: it must be deterministic for the stable prefix to stay
/// byte-identical across turns (RFC 0024 §276). Servers are consumed in order
/// until the total budget is exhausted, so a stable input order yields stable
/// output bytes.
///
/// Returns the fitted list plus the number of servers dropped entirely, which the
/// caller should surface -- silent omission gives no signal that the model is
/// missing a server's guidance.
pub fn fit_mcp_guidance(guidance: &[McpGuidance]) -> (Vec<McpGuidance>, usize) {
    let mut out: Vec<McpGuidance> = Vec::new();
    let mut spent = 0usize;
    let mut dropped = 0usize;
    for g in guidance {
        let remaining = MAX_MCP_INSTRUCTIONS_TOTAL_BYTES.saturating_sub(spent);
        let budget = MAX_MCP_INSTRUCTIONS_BYTES.min(remaining);
        match fit_mcp_instructions(&g.text, budget) {
            Some(text) => {
                spent += text.len();
                out.push(McpGuidance {
                    server: g.server.clone(),
                    text,
                });
            }
            None => dropped += 1,
        }
    }
    (out, dropped)
}

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

/// Everything the system prompt is built from.
///
/// A struct rather than a positional argument list because the list had reached
/// eight, three of which are `Option<&str>`: at that point a caller can transpose
/// two arguments and still compile, and #1173 was about to add a fourth
/// `Option<&str>`. Named fields make that class of mistake impossible instead of
/// merely unlikely.
///
/// Construct with [`SystemPromptInputs::new`] and set the optional fields you
/// need, so adding a field later does not touch every call site.
pub struct SystemPromptInputs<'a> {
    /// Phenotype persona text, injected at the top of the stable prefix.
    pub persona: Option<&'a str>,
    /// Installed skills, listed for discovery.
    pub skills: &'a SkillRegistry,
    /// Names of skills whose bodies are injected in full.
    pub active: &'a [String],
    /// Date, working directory, and shell hints for the volatile tail.
    pub user: &'a UserContext,
    /// Durable memory, already selected and formatted.
    pub memory: Option<&'a str>,
    /// Project instructions (`AGENTS.md` and friends), volatile tail.
    pub extra_instructions: Option<&'a str>,
    /// The active goal, if a goal loop is running.
    pub goal: Option<&'a Goal>,
    /// Permission mode, which selects the mode-steer paragraph.
    pub mode: Mode,
    /// Per-server MCP `initialize` guidance for servers whose tools are admitted
    /// (#1173). Already capped by [`fit_mcp_guidance`]; order must be
    /// deterministic to keep the stable prefix byte-identical.
    pub mcp_guidance: &'a [McpGuidance],
}

impl<'a> SystemPromptInputs<'a> {
    /// The always-required fields; optional ones default to absent.
    pub fn new(
        skills: &'a SkillRegistry,
        active: &'a [String],
        user: &'a UserContext,
        mode: Mode,
    ) -> Self {
        Self {
            persona: None,
            skills,
            active,
            user,
            memory: None,
            extra_instructions: None,
            goal: None,
            mode,
            mcp_guidance: &[],
        }
    }
}

/// Build the system prompt prepended to every turn's request.
///
/// Returns a [`SystemPrompt`] with the split at the cache boundary: everything
/// before "User context" is stable; everything from "User context" onward is
/// volatile. The two halves are rendered from separate `minijinja` templates
/// (`stable.jinja` / `volatile.jinja`) so the split is a first-class value
/// rather than a comment-marked slice. Data shaping (skill sorting, registry
/// resolution, verdict labeling, empty-memory/working-dir folding) lives here;
/// literal prompt copy lives in the templates. See issue #938.
pub fn build_system_prompt(inputs: &SystemPromptInputs<'_>) -> SystemPrompt {
    let &SystemPromptInputs {
        persona,
        skills,
        active,
        user,
        memory,
        extra_instructions,
        goal,
        mode,
        mcp_guidance,
    } = inputs;
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
        mcp_guidance: mcp_guidance
            .iter()
            .map(|g| McpGuidanceEntry {
                server: &g.server,
                text: g.text.trim_end(),
            })
            .collect(),
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
    mcp_guidance: Vec<McpGuidanceEntry<'a>>,
}

#[derive(Serialize)]
struct McpGuidanceEntry<'a> {
    server: &'a str,
    text: &'a str,
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
