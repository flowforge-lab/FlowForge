# 0017 — Durable Scheduled-Task Cron Runner

- **Status:** Proposed
- **Milestone:** M7+
- **Author:** tonytan4ever
- **Depends on:** RFC 0005 (three-tier model selection — `resolve_model_selection`),
  RFC 0011 (Plan/Act/Auto modes — `Approver` trait + `Safety` tiers), and RFC 0012
  (durable session persistence — the `ff-session` SQLite store pattern). The headless
  long-run behavior this needs is the four-clause contract in §3–§3.2, owned here — it is
  **no longer gated on #74**; #74 stays open for goal-mode loop durability (its items 1–3,
  not required by a bounded scheduled fire)
- **Tracking issue:** #188 (real scheduled-task cron runner + ts-rs bindings)
- **Supersedes:** the FE-owned mock contract in `apps/desktop/src/lib/scheduled.ts`
  (#132 / SET.9)

## 1. Summary & Goals

FlowForge's Settings → **Scheduled** section ships today (#132) against a hand-written,
FE-owned `ScheduledTask` type and three mock IPC commands (`listScheduledTasks` /
`toggleScheduledTask` / `createScheduledTask`) backed by in-memory state in `mock.ts`.
There is no backend type, no durable store, and no cron runner: tasks vanish on restart
and never fire. The FE invents `cadenceLabel` from `cron`, so the two can already diverge.

This RFC specifies the real backend — a durable, ts-rs-typed scheduled-task store and a
tokio cron runner — and reframes what a "fire" is:

> **Thesis:** A scheduled fire is not a new execution model. It is the *existing* headless
> `run_turn` path (already shipping in `apps/cli`) driven by a non-interactive `Approver`
> bounded by a per-task **safety ceiling**. We reuse the agent loop, the `Approver`/`Safety`
> machinery (RFC 0011), and the `ff-session` SQLite pattern (RFC 0012) rather than inventing
> a parallel runner. "Auto-approve within a policy" and "read-only only" are the same
> mechanism with a different ceiling, so the safe default makes the knob free.

Goals:
- **One source of truth for the type** — `ScheduledTask` becomes a `#[derive(TS)]` Rust
  binding; the FE-owned type and the `ipc.ts` CONTRACT NOTE are retired.
- **Durable** — tasks survive app restart (SQLite, mirroring `ff-session`).
- **Fires on schedule, headlessly** — a tokio tick loop fires due tasks as agent runs even
  when no window is focused, and stamps `last_run`.
- **Derived cadence label and `next_run`** — both computed from the cron expression on read,
  never persisted, so they cannot drift from `cron`.
- **Safe by default** — a fire cannot silently perform a destructive action with no human
  present; `Dangerous` tool calls are always denied headless.

Non-goals (this RFC): natural-language schedule parsing, multi-machine / distributed
scheduling, calendar (iCal) import, and per-task notification routing.

## 2. Current State (what we are replacing)

`apps/desktop/src/lib/scheduled.ts` (FE-owned):

```ts
interface ScheduledTask {
  id: string; name: string; builtin: boolean;
  cron: string; cadenceLabel: string;
  nextRun: number | null; lastRun: number | null; paused: boolean;
}
interface CreateScheduledTaskInput { name: string; cron: string; cadenceLabel: string; }
```

- `ipc.ts` carries a CONTRACT NOTE (SET.9) marking these as FE-owned mock commands.
- `mock.ts` seeds tasks in-memory and implements `list` / `toggle` / `create`.
- The "New task" form (FE mockup) already collects **Instructions**, **Workspace**, and
  **Profile** — three fields the mock `CreateScheduledTaskInput` does *not* have. The real
  input type must grow them (see §6 — a flagged contract change for the FE PR).

## 3. The Load-Bearing Decision: headless execution

A scheduled task fires "even when you're away" — there is no human to approve a tool call.
The existing UI fire path, `spawn_assistant_turn` (`apps/desktop/src-tauri/src/lib.rs:558`),
is welded to a live window: it builds a `UiApprover` that emits an approval-request event
and **blocks on a human click**. Reusing it headlessly would hang on the first `Write` /
`Dangerous` tool call.

**We already solved this.** `apps/cli` runs `run_turn` headlessly via `CliApprover`
(`apps/cli/src/approver.rs`), which implements the same `ff_agent::Approver` trait the UI
uses, driven by an `ApprovalMode` policy (`Yes` / `Deny` / `Prompt`) evaluated against each
tool's `Safety` tier (`ReadOnly` / `Write` / `Dangerous`). A non-TTY (`Piped`) `Deny`
policy auto-denies elevated calls instead of prompting — exactly the headless contract a
scheduler needs.

**Decision:** a fire is a `run_turn` call with a new `ScheduledApprover` that auto-allows
up to a per-task **safety ceiling**:

| ceiling                | ReadOnly | Write | Dangerous |
|------------------------|----------|-------|-----------|
| `ReadOnly` (default)   | allow    | deny  | deny      |
| `Write` (opt-in)       | allow    | allow | deny      |

`Dangerous` is **always denied** in a headless fire, enforced in `ScheduledApprover`
**regardless of task kind** so a builtin (§5) cannot escalate. A denied call returns a
denial outcome the run records (same as CLI `Deny`) — it never hangs. We ship the safe
default; the ceiling knob is free.

This is why the runner half of #188 was originally sequenced behind **#74**: a fire is a
headless run, and it should not ship before its long-run behavior is bounded and safe.

### 3.1 Halt-and-surface on `ask_user` (the fourth contract clause)

The safety ceiling above handles `Write` / `Dangerous` tool calls. One more headless gap
remains: `ask_user`. A live UI fire blocks on a human answering; a headless fire has no
human, and the current dismissed-answer path (`agent/lib.rs:1279`) lets the model *continue
on the dismissed result* — i.e. grind on or guess a decision that needed a person.

**Decision:** a mid-fire `ask_user` **halts the run and records a distinct terminal
outcome, `needs_attention`** — not folded under `error` (it is not a failure), `cancelled`
(it is not a timeout/cancel), or `ok` (it did not complete). This keeps the "a human
decision is pending" signal legible to the UI, the audit trail (`scheduled_runs.status`,
§8.4), and any later resume path. A scheduled fire that stops for a human is reported
differently from timeout cancellation, policy denial, runtime error, and success.

### 3.2 The full headless-safe contract (and why this RFC, not #74)

Clauses 1–4 — **bounded** (max-iterations + per-fire timeout + cancellation, §8.2),
**safety ceiling** (`Dangerous` always denied, §3), **mechanical terminal status**
(§8.4), and **halt-and-surface on `ask_user`** (§3.1) — are the complete minimal subset a
headless fire needs. A fire runs a **bounded single `run_turn`**, *not* the open-ended
`goal` loop, so it does **not** require the durable task ledger / fresh-context iteration
from #74 (deferred to the goal-mode milestone). This RFC therefore now carries the full
headless-safe contract: the runner (PR-B / #542) depends on this section, **not** on the
open-ended #74 discussion. #74 remains open for goal-mode loop durability (its items 1–3),
which a bounded scheduled fire does not need.

## 4. Architecture

New crate **`ff-scheduled`** (keeps the `cron` dependency and the runner loop out of the
agent hot path; the issue allows `ff-core`, but a dedicated crate mirrors `ff-session`):

```
ff-scheduled
├── model.rs    ScheduledTask (#[derive(TS)]) + CreateScheduledTaskInput + RunRecord + SafetyCeiling
├── store.rs    SqliteStore — CRUD, last_run stamping, run-record append (mirror ff-session)
├── cron.rs     parse expr, prev/next occurrence, derive cadence_label  ← single source of truth
└── runner.rs   tokio interval tick → find due → fire via TaskRunner → stamp → emit
```

- **Cron crate:** `cron` (a thin `chrono`-aware parser exposing `upcoming()` / `after()`
  iterators), **not** `tokio-cron-scheduler`. The latter owns its own job registry, which
  would compete with our durable store as the source of truth and risks double-fire across
  restarts. We own the tick loop (a single `tokio::interval`, ~30 s) and compute due tasks
  from the store each tick. Restart-safe, no duplicate scheduler state.
- **`cadence_label`** is computed in `cron.rs` on read and never persisted.
- **`next_run`** is computed on read as the next future occurrence (`cron.after(now)`),
  never stored (avoids staleness after a missed tick or a clock change). It is a *display*
  value only — **not** the firing trigger (see the due predicate below).

### Due-detection (the correct predicate)

`next_run` is the next **future** occurrence, so a "fire when `next_run <= now`" test would
*never* be true. Firing keys off the **previous** occurrence and the durable `last_run`:

> A task is **due** at tick time `now` when it is not paused and
> `prev_occurrence(cron, now)` exists and is **strictly greater than `last_run`**
> (treating a `None` `last_run` as `-∞`).

After firing we stamp `last_run = now`, so the same slot cannot re-fire on the next tick.
This also makes the §8.1 "skip missed fires" policy fall out for free: if the app was down
across several slots, only the single most-recent past slot is `> last_run`, so the task
fires **once** on wake, not once per missed slot. Durable `last_run_ms` keeps a future
per-task catch-up (PR-C) a clean add.

### Data flow

```
tokio interval (~30 s)
  → store.due_tasks(now)               // not paused AND prev_occurrence(cron, now) > last_run
    → for each (serialized, per-fire timeout): TaskRunner::fire   // headless run_turn @ ceiling
      → store.stamp_last_run(id, now) + append RunRecord{ session_id, status }
      → app.emit("scheduled:fired", { id, sessionId, lastRun })
```

### Firing decoupled via a trait (keeps `ff-scheduled` Tauri-free)

```rust
#[async_trait]
pub trait TaskRunner: Send + Sync {
    /// Fire one task as a headless agent run. Returns the created session id and outcome.
    async fn fire(&self, task: &ScheduledTask) -> RunOutcome;
}
```

The desktop crate implements `TaskRunner`: it creates a session (workspace + profile from
the task) and calls `run_turn` with a `ScheduledApprover` at the task's ceiling. Because the
fire's session has no pane and no per-session model override, the model/connection is
resolved by RFC 0005's `resolve_model_selection` precedence (no session override → phenotype
→ global) — the runner does not invent its own selection.

## 5. Type, Schema & Timezone

### `ScheduledTask` (ts-rs binding, replaces the FE type)

The mock's `prompt` + `builtin: bool` would let an illegal state exist (a builtin with a
free-text prompt). We make it unrepresentable with a `task_kind` sum type:

```rust
#[derive(Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum TaskKind {
    Prompt(String),          // user task: free-text instructions
    Builtin(BuiltinAction),  // app task: a named internal action (e.g. MemoryConsolidate)
}

#[derive(Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduledTask {
    pub id: String,
    pub name: String,
    pub cron: String,
    pub kind: TaskKind,                // replaces prompt + builtin
    pub workspace: Option<String>,
    pub profile: Option<String>,
    pub safety_ceiling: SafetyCeiling, // ReadOnly (default) | Write
    pub paused: bool,
    pub cadence_label: String,         // derived; never persisted
    pub next_run: Option<i64>,         // computed display value; epoch ms
    pub last_run: Option<i64>,         // stamped; epoch ms
}
```

`builtin` in the FE wire shape is derived from `kind` (`matches!(kind, Builtin(_))`) so the
existing FE badge keeps working without a separate column.

### SQLite schema

```sql
CREATE TABLE scheduled_tasks (
  id             TEXT PRIMARY KEY,
  name           TEXT NOT NULL,
  cron           TEXT NOT NULL,
  kind           TEXT NOT NULL,        -- 'prompt' | 'builtin'
  kind_value     TEXT NOT NULL,        -- the instructions, or the BuiltinAction name
  workspace      TEXT,
  profile        TEXT,
  safety_ceiling TEXT NOT NULL DEFAULT 'read_only',
  paused         INTEGER NOT NULL DEFAULT 0,
  last_run_ms    INTEGER,
  created_ms     INTEGER NOT NULL
);
-- next_run and cadence_label are derived on read, never columns.

CREATE TABLE scheduled_runs (          -- backs the ↗ "open session" affordance + audit trail
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id     TEXT NOT NULL REFERENCES scheduled_tasks(id) ON DELETE CASCADE,
  session_id  TEXT,
  fired_ms    INTEGER NOT NULL,
  status      TEXT NOT NULL            -- ok | error | cancelled | needs_attention (see §8.4)
);
```

### Timezone (evaluation in local time)

A user reading "Daily at 9:00 AM" means **their local wall-clock time**, so the runner
evaluates cron in the host's **local timezone** (`chrono::Local`), not UTC. The `cron` crate
is `chrono`-TZ aware; we feed it `Local::now()`. Stored `*_ms` stamps remain UTC epoch ms
(the wire contract); only schedule *evaluation* is local. DST: a spring-forward slot that
does not exist is skipped (no occurrence that day); a fall-back duplicated hour fires once
(the `> last_run` guard dedupes the repeated wall-clock instant). The form presets map to
canonical 5-field expressions:

| preset   | example expression | meaning              |
|----------|--------------------|----------------------|
| Hourly   | `0 * * * *`        | top of every hour    |
| Daily    | `0 9 * * *`        | 09:00 local daily    |
| Weekly   | `0 9 * * 1`        | 09:00 local Monday   |
| Monthly  | `0 17 17 * *`      | 17:00 local, day 17  |
| Custom   | user-entered       | validated on input   |

### Commands & events

| command | PR | notes |
|---|---|---|
| `list_scheduled_tasks() -> Vec<ScheduledTask>` | A | store + computed next_run/label |
| `create_scheduled_task(input) -> ScheduledTask` | A | validates cron; rejects bad expr |
| `toggle_scheduled_task(id) -> ScheduledTask` | A | flips `paused` |
| `delete_scheduled_task(id)` | A | rejected for `Builtin` tasks |
| `preview_cadence(cron) -> String` | A | live label for the Custom-cron tab |
| `run_scheduled_task_now(id) -> RunOutcome` | B | the ▶ affordance; needs real firing |

The **four** store-only commands plus `preview_cadence` ship in PR-A; `run_scheduled_task_now`
lands in PR-B because it requires the real runner. Events (PR-B): `scheduled:fired`
`{ id, sessionId, lastRun }` and `scheduled:changed` `{ id }` so the UI live-updates without
polling.

## 6. Surface Gaps in the Mockups to Resolve

These are visible in the FE design but unspecified by #188; the RFC pins them down:

1. **Run-now (▶) / pause (⏸) per row** → `run_scheduled_task_now` (PR-B) and `toggle` =
   pause/resume (the FE shows ⏸ on active, ▶ on paused tasks).
2. **Open (↗) per row** → jumps to the latest session a fire created; uses the
   `scheduled_runs` linkage (latest `session_id` for the task).
3. **Builtin tasks** ("Memory Organizer", `Builtin` badge) → cannot be deleted; their fire
   is a `BuiltinAction` (e.g. `MemoryConsolidate`), not a free-text prompt — modelled by the
   `TaskKind` sum type in §5. Seeded on first run.
4. **Custom cron tab** → free-text expression validated on input, with a live cadence
   preview via `preview_cadence`.
5. **Contract change for the FE PR:** `CreateScheduledTaskInput` must carry `kind` (prompt
   text), `workspace`, `profile`, and `safety_ceiling` (the form already collects the first
   three). Flag to @abidkhan03 before he regenerates against the binding.

## 7. Sequencing — two shippable PRs

**PR-A (this issue's core; ships now, NOT blocked on #74):**
- `ff-scheduled` crate: `model.rs` (+ ts-rs binding), `store.rs`, `cron.rs` (parse +
  prev/next occurrence + cadence label).
- Regenerate `bindings/`; delete the `lib/scheduled.ts` FE type + the `ipc.ts` CONTRACT NOTE.
- Wire the four store-only commands + `preview_cadence`. **Firing stubbed** (a `TaskRunner`
  that records a `stub` run). Tasks persist, list, and compute `next_run` / `cadence_label`
  — unblocking the FE against real bindings immediately.

**PR-B (#542; gated on the §3–§3.2 headless-safe contract, not on the open-ended #74):**
- `runner.rs` tick loop + the desktop `TaskRunner` impl (real `run_turn` + `ScheduledApprover`).
- `scheduled:fired` / `scheduled:changed` events; `last_run` stamping; `run_scheduled_task_now`.

**PR-C (follow-up):** builtin seeding + `BuiltinAction` dispatch, the `scheduled_runs` ↗
linkage, a global "pause all scheduled tasks" kill-switch, and a per-task `catch_up` policy.

## 8. Open Questions (with proposed resolutions)

1. **Missed fires → skip.** Falls out of the §4 due predicate: only the most-recent past
   slot is `> last_run`, so a task fires once on wake, not once per missed slot. Durable
   `last_run_ms` keeps a future per-task `catch_up` opt-in clean.
2. **Concurrency → serialize, with a per-fire timeout + cancellation.** Fires compete with
   the user's live session for the provider, so they run one at a time. A serialized queue of
   *full goal runs* means one slow/hung fire could starve later due tasks — so each fire has
   a per-fire timeout and is cancellable, and the next-tick recompute naturally re-queues
   (deduped by task id, so a long fire cannot enqueue the same task twice).
3. **Safety-ceiling UI → an "Allow file changes" toggle = `Write`.** "Write while you're
   away" is a real trust surface, so PR-C adds a global **pause-all kill-switch** and
   `scheduled_runs.status` is the audit trail. The ceiling is enforced in `ScheduledApprover`
   regardless of task kind (a builtin cannot escalate).
4. **`scheduled_runs.status` semantics.** A run usually *completes* even if a tool was
   denied (the agent continues read-only), so most terminal status is **`ok` | `error` |
   `cancelled`**; a denial is incidental within an otherwise-`ok` run, not a terminal status.
   A stale profile / moved workspace fails the fire with `error`, surfaced on ↗. An
   `ask_user` mid-fire is the one exception that gets its own terminal status,
   **`needs_attention`** (§3.1) — it is neither a failure nor a cancellation nor a success,
   and folding it under any of those would lose the "a human decision is pending" signal.
   So the full terminal set is **`ok` | `error` | `cancelled` | `needs_attention`**.

## 9. Verification Plan

- **`cron.rs`** table-driven tests over the form presets (Hourly/Daily/Weekly/Monthly/Custom):
  cadence-label derivation; `next_run` (`after(now)`); and the due predicate —
  **slot at T, tick at T+5 s fires once, tick at T+35 s does NOT re-fire** (the regression that
  would otherwise make the scheduler never-fire or double-fire). A DST spring-forward slot is
  skipped; a fall-back hour fires once.
- **`store.rs`** CRUD + restart durability; `due_tasks` excludes paused and not-yet-due tasks;
  `delete` rejected for `Builtin`.
- **`ScheduledApprover`** tests mirroring `approver.rs`: `ReadOnly` ceiling denies
  Write/Dangerous; `Write` ceiling allows Write, denies Dangerous; `Dangerous` denied for a
  builtin too; a denial never blocks the run.
- **Halt-and-surface (§3.1):** a mid-fire `ask_user` halts the run and records
  `scheduled_runs.status = needs_attention` (not `ok`/`error`/`cancelled`); the run does not
  continue on the dismissed-answer path and does not hang.
- **Desktop:** command round-trips under `VITE_FF_MOCK=0`; `bindings/ScheduledTask.ts`
  generated; `lib/scheduled.ts` removed; `pnpm typecheck && lint && test` green.
- **Workspace:** `cargo test -p ff-scheduled`, `cargo clippy --workspace --all-targets -D warnings`,
  `cargo fmt --check`.
