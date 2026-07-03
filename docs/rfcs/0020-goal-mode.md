# 0020 — Goal Mode: persistent autonomous objective loop

- **Status:** Proposed
- **Milestone:** 0.2.0
- **Author:** tonytan4ever
- **Depends on:** RFC 0019 (permission matrix — safety gating only), RFC 0011 (Plan/Act/Auto), RFC 0017 (scheduled runner — reused seams), #229 (approval gate)
- **Tracking issue:** #683
- **Supersedes / implements:** #74 (loop durability for goal mode)

## 1. Summary & Goals

Ship **goal mode** — a persistent autonomous objective loop where the agent keeps
iterating toward a stated objective until it completes, exhausts its budget, or the
user interrupts it. This is the headline autonomy capability for 0.2.0 and the
enabler for FlowForge developing FlowForge.

Goals:

- **Self-continuing loop** — when a goal is active, the agent re-drives itself after
  each turn (instead of idling for user input) until a stop condition fires.
- **Durable** (#74) — the objective + a per-iteration checkpoint are persisted so a
  crash/restart resumes from the last completed iteration, not from scratch.
- **Budget-bounded** — configurable stop conditions (max iterations, max cumulative
  tokens, max wall-clock). The agent states its reasoning when it stops.
- **Observable and steerable** — a status panel shows objective, iteration count,
  last action, budget remaining; the user can steer (refine mid-run), pause, or abort.
- **Safe by construction** — the loop runs *under* the permission matrix (RFC 0019),
  never around it. This is the single non-negotiable invariant (§4).

Non-goals:

- **Multi-session / multi-agent orchestrated goals** — a goal lives in one session
  (v1). Cross-session orchestration is a future extension (Team tab / C2).
- **A new autonomy tier** — goal mode is orthogonal to Plan/Auto/Act; it runs *within*
  whichever mode the session is in.
- **OS-level sandboxing** — RFC 0011 §12 stands; the matrix is the trust boundary.

## 2. Relationship to existing infrastructure

Goal mode is largely an orchestration layer over seams that already exist, so most of
it can be built before RFC 0019 lands (see §9 dependency analysis):

| Need | Reused seam (today) |
|------|---------------------|
| Headless self-driving turn | `spawn_assistant_turn` → `run_turn` (`apps/desktop/src-tauri/src/lib.rs`), already runs under an `Approver` with a per-fire budget (`DesktopTaskRunner::fire`, RFC 0017) |
| Durable JSON persistence + resume | `ff-scheduled` store pattern (SQLite / JSON mirror) |
| Terminal status vocabulary | `RunStatus` / `RunRecord` (`ff-core/src/scheduled.rs`) |
| Per-turn cancel | `CancelToken` (`ff-agent`) |
| Mode awareness in prompt | `mode_steer` in `ff-agent/src/system_prompt.rs` (extended by #701) |
| Safety gating | permission matrix (RFC 0019) — the ONE piece goal mode adds no new mechanism for |

## 3. The `Goal` model

A goal is single-session, so it is keyed by `session_id` and persisted to
`~/.flowforge/goals/<session_id>.json`.

```rust
/// A persistent autonomous objective bound to one session (#683).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct Goal {
    pub session_id: String,
    /// The objective text the loop iterates toward.
    pub objective: String,
    pub status: GoalStatus,
    /// Completed iterations so far (a checkpoint boundary bumps this).
    pub iteration: u32,
    pub budget: GoalBudget,
    /// Cumulative usage across all iterations, checked against `budget`.
    pub spent: GoalSpend,
    /// Last-completed-iteration checkpoint: the summary the resume path replays
    /// from, plus the epoch-ms it was written. `None` before the first boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub checkpoint: Option<GoalCheckpoint>,
    /// Pending user steer to fold into the next iteration (§6). Cleared once consumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pending_steer: Option<String>,
    #[ts(type = "number")]
    pub created_ms: i64,
    #[ts(type = "number")]
    pub updated_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub enum GoalStatus {
    /// Loop is self-continuing.
    Active,
    /// User-paused at an iteration boundary; resumable.
    Paused,
    /// The agent judged the objective met and called `goal_complete`.
    Completed,
    /// A stop condition or unrecoverable error ended it. `reason` explains why.
    Failed,
    /// Budget exhausted (iterations / tokens / wall-clock) before completion.
    Exhausted,
}

/// Stop conditions. `None` on an optional field = that dimension is unbounded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct GoalBudget {
    /// Default 25.
    pub max_iterations: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub max_wall_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct GoalSpend {
    #[ts(type = "number")]
    pub tokens: u64,
    #[ts(type = "number")]
    pub wall_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../apps/desktop/src/bindings/")]
pub struct GoalCheckpoint {
    /// Short natural-language progress note the loop writes at each boundary and
    /// injects on resume, so a restarted loop reorients without re-reading the
    /// whole transcript.
    pub summary: String,
    #[ts(type = "number")]
    pub at_iteration: u32,
    #[ts(type = "number")]
    pub written_ms: i64,
}
```

## 4. The safety invariant (why #682 is a prerequisite to *ship*)

Goal mode adds **no new approval mechanism**. Every tool call in an autonomous
iteration goes through the exact same gate an interactive turn does — resolved from
the RFC 0019 permission matrix for the session's mode:

- A goal in **Auto** auto-approves ReadOnly/Write, **gates Sensitive** (push, PR,
  network egress), and **hard-denies Dangerous**.
- A goal in **Act** additionally auto-approves Sensitive but still asks on Dangerous.
- A goal in **Plan** is read-only by construction (it can research/plan autonomously
  but cannot mutate) — useful for autonomous investigation.

Because a headless loop has no human at the approval prompt, an `Ask` cell inside a
goal iteration resolves to **pause-and-surface** (status → `Paused` with a
`NeedsAttention`-style reason), never auto-approve. This mirrors the scheduled
runner's `ask_user`-dismissed behavior (RFC 0017 §8.4). The matrix is therefore the
literal boundary that makes autonomous running safe — hence RFC 0019 is a hard
prerequisite to *ship*, though not to *build* the loop (§9).

## 5. Lifecycle & loop

### 5.1 State machine

```
              goal_set
                 │
                 ▼
     ┌───────► Active ──── goal_complete ───────► Completed
     │           │
 goal_resume     ├── stop condition (budget) ───► Exhausted
     │           │
     │           ├── unrecoverable error ───────► Failed
     │           │
     │           └── goal_pause / Ask cell hit ─► Paused
     └───────────────────────────────────────────┘
```

### 5.2 Iteration boundary

One iteration = one `run_turn` to a terminal answer (which itself may span many
tool-call sub-turns). At each boundary the loop, in order:

1. Accumulates `spent` (tokens from the turn's stats; wall-clock delta).
2. Writes a `GoalCheckpoint` (progress summary) and persists the `Goal` atomically
   (temp-file + rename, like `default_mode` persistence).
3. Checks stop conditions: `iteration >= max_iterations`, `spent.tokens >= max_tokens`,
   `spent.wall_ms >= max_wall_ms`, an `Ask`/pause, cancel, or `goal_complete`.
4. If none fire, folds any `pending_steer` into the next turn's context and
   re-drives `run_turn`. Otherwise transitions to the terminal/paused status.

### 5.3 Interruptibility

Per the open question resolution (§10): a goal is **interruptible mid-turn** (the
in-flight LLM call is cancelled via `CancelToken`), but **resume replays from the
last completed iteration** — a mid-turn interrupt discards only the partial turn, not
the checkpoint. This keeps resume deterministic.

## 6. Steering (user messages during a run)

A user message sent while a goal is `Active` is **not** a normal turn — it becomes a
**steer**: stored in `pending_steer` and folded into the next iteration's context
(prepended as a high-priority instruction). This lets the user refine the objective
mid-flight without racing the loop. A steer never bypasses the budget or the matrix.

## 7. IPC contract (for FE)

```
goal_set(sessionId, objective, budget?): Goal      // begin/replace the session's goal
goal_status(sessionId): Goal | null                // current goal snapshot (panel poll / event)
goal_pause(sessionId): Goal                         // pause at next boundary
goal_resume(sessionId): Goal                        // resume a paused/checkpointed goal
goal_clear(sessionId): void                         // abort + delete checkpoint
goal_complete(sessionId, summary?): Goal            // agent-invoked: objective met (also a tool)
```

Plus a `goal:updated` event (`{ goal: Goal }`) emitted at each boundary so the panel
re-renders without polling — same pattern as `scheduled:changed` / `session:title-updated`.

`goal_complete` is dual-surface: an IPC command **and** an agent tool (ReadOnly
safety — it only writes goal state, no side effect), so the model can declare the
objective met from inside the loop.

## 8. System-prompt injection

When a goal is active, inject a volatile-tail block (after memory, near `mode_steer`
— no prefix-cache regression, same slot rationale as #701):

```
## Active goal (iteration N of MAX)
Objective: <objective>
Progress so far: <checkpoint.summary>
<pending steer, if any>
Continue toward the objective. If it is fully met, call `goal_complete`.
State your reasoning before each action.
```

Eventually the mode-constraint line is derived from the resolved matrix cell (so the
model's self-description of what it may do / must ask about matches reality), reusing
#701's work.

## 9. Dependency analysis — what can be built before #682

Only the **safety-gating wire-up (§4)** and **ship** wait on RFC 0019. Everything
else builds against stable seams and gets the matrix snapped in at one call site:

| Track | Depends on #682? | Rationale |
|-------|:---:|-----------|
| A. This RFC | No | Design only |
| B. `Goal` type + `~/.flowforge/goals/*.json` persistence + resume | **No** | Pure data/serde; goal *state* is independent of how actions are gated |
| C. Self-continue loop skeleton (budget, checkpoint, boundary) | **No** | Wraps existing `spawn_assistant_turn`/`run_turn`; gate via *today's* `mode_auto_approves` as a placeholder, swap to `matrix.cell(mode, safety)` when #682 lands — a one-line change at one seam |
| D. FE goal status panel | No (needs §7 IPC shape) | Build against a `MockIpc` faker, like the search panel |
| E. System-prompt goal injection | No (rides #701, not #682) | `mode_steer` extension |
| F. Safety-gating wire-up (§4 Ask→pause, matrix lookup) | **Yes** | The one true dependency |
| G. Ship to users | **Yes** | Design principle #1 |

## 10. Open questions (resolved defaults)

- **Pauseable mid-turn or at boundary only?** → **Interruptible mid-turn** (cancel the
  in-flight call), resume replays from the last completed iteration (§5.3).
- **Token budget per-goal or per-iteration?** → **Per-goal cumulative** (`GoalSpend`).
- **Can the user message during a run?** → **Yes** — it becomes a steer (§6).
- **Default `max_iterations`?** → **25**, overridable at `goal_set` or in settings.
- **What does an `Ask` cell do inside a headless goal iteration?** → **Pause + surface**
  (status `Paused`), never auto-approve (§4).
