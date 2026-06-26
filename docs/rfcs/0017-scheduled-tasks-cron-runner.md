
## 2. Current state (what we are replacing)

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
- `mock.ts` seeds tasks in-memory and implements `list/toggle/create`.
- The "New task" form (mockup 2) already collects **Instructions**, **Workspace**, and
  **Profile** — three fields the mock `CreateScheduledTaskInput` does *not* have. The real
  input type must grow them (see §6, a flagged contract change for the FE PR).

## 3. The load-bearing decision: headless execution

A scheduled task fires "even when you're away" — there is no human to approve a tool call.
The existing UI fire path, `spawn_assistant_turn` (`apps/desktop/src-tauri/src/lib.rs:558`),
is welded to a live window: it builds a `UiApprover` that emits an approval-request event
and **blocks on a human click**. Reusing it headlessly would hang on the first `Write`/
`Dangerous` tool call.

**We already solved this.** `apps/cli` runs `run_turn` headlessly via `CliApprover`
(`apps/cli/src/approver.rs`), which implements the same `ff_agent::Approver` trait the UI
uses, driven by an `ApprovalMode` policy (`Yes` / `Deny` / `Prompt`) evaluated against each
tool's `Safety` tier (`ReadOnly` / `Write` / `Dangerous`). A non-TTY (`Piped`) `Deny`
policy auto-denies elevated calls instead of prompting — exactly the headless contract a
scheduler needs.

**Decision:** a fire is a `run_turn` call with a new `ScheduledApprover` that auto-allows
up to a per-task **safety ceiling**:

| ceiling      | ReadOnly | Write | Dangerous |
|--------------|----------|-------|-----------|
| `ReadOnly` (default) | allow | deny | deny |
| `Write` (opt-in)     | allow | allow | deny |

`Dangerous` is **always denied** in a headless fire. A denied call returns a denial outcome
the run records (same as CLI `Deny`) — it never hangs. This makes "auto-approve within a
policy" and "read-only only" the *same mechanism* with a different ceiling: we ship the safe
default and the knob is free. No new approval subsystem.

This is why the runner half of #188 sequences behind **#74**: a fire is a headless goal
run, and it should not ship before the goal loop it triggers is durable.

## 4. Architecture

New crate **`ff-scheduled`** (keeps the `cron` dependency and the runner loop out of the
agent hot path; the issue allows `ff-core` but a dedicated crate mirrors `ff-session`):

```
ff-scheduled
├── model.rs    ScheduledTask (#[derive(TS)]) + CreateScheduledTaskInput + RunRecord + SafetyCeiling
├── store.rs    SqliteStore — CRUD, last_run stamping, run-record append (mirror ff-session)
├── cron.rs     parse expr, compute next_run, derive cadence_label  ← single source of truth
└── runner.rs   tokio interval tick → find due → fire via TaskRunner → stamp → emit
```

- **Cron crate:** `cron` (a thin parser exposing an `upcoming()` iterator), **not**
  `tokio-cron-scheduler`. The latter owns its own job registry, which would compete with our
  durable store as the source of truth and risks double-fire across restarts. We own the tick
  loop (a single `tokio::interval`, ~30s) and compute due tasks from the store each tick.
  Restart-safe, no duplicate scheduler state.
- **`cadenceLabel`** is computed in `cron.rs` on read and never persisted, killing the FE/
  backend drift the issue calls out.
- **`next_run`** is likewise computed on read from `cron` + `now`, never stored (avoids
  staleness after a missed tick or a clock change).
- **Firing decoupled via a trait** so `ff-scheduled` stays Tauri-free:

```rust
#[async_trait]
pub trait TaskRunner: Send + Sync {
    /// Fire one task as a headless agent run. Returns the created session id and outcome.
    async fn fire(&self, task: &ScheduledTask) -> RunOutcome;
}
```

The desktop crate implements `TaskRunner`: it creates a session (workspace + profile from
the task), builds the registry, and calls `run_turn` with a `ScheduledApprover` at the
task's `SafetyCeiling`. Built-in tasks map to a named internal action (e.g. memory
consolidation) rather than a free-text prompt (see §6).

### Data flow

```
tokio interval (30s)
  → store.due_tasks(now)            // paused excluded; next_run <= now
    → for each: TaskRunner::fire    // headless run_turn @ safety ceiling
      → store.stamp_last_run(id, now) + append RunRecord{session_id, status}
      → app.emit("scheduled:fired", {id, sessionId, lastRun})
```

## 5. Type & schema

### `ScheduledTask` (ts-rs binding, replaces the FE type — same wire shape, plus new fields)

```rust
#[derive(Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduledTask {
    pub id: String,
    pub name: String,
    pub builtin: bool,
    pub cron: String,
    pub cadence_label: String,        // derived; never persisted
    pub next_run: Option<i64>,        // computed; epoch ms
    pub last_run: Option<i64>,        // stamped; epoch ms
    pub paused: bool,
    // new fields the form already collects:
    pub prompt: String,               // "Instructions"
    pub workspace: Option<String>,
    pub profile: Option<String>,
    pub safety_ceiling: SafetyCeiling, // ReadOnly (default) | Write
}
```

### SQLite schema

```sql
CREATE TABLE scheduled_tasks (
  id            TEXT PRIMARY KEY,
  name          TEXT NOT NULL,
  cron          TEXT NOT NULL,
  prompt        TEXT NOT NULL,
  workspace     TEXT,
  profile       TEXT,
  safety_ceiling TEXT NOT NULL DEFAULT 'read_only',
  builtin       INTEGER NOT NULL DEFAULT 0,
  paused        INTEGER NOT NULL DEFAULT 0,
  last_run_ms   INTEGER,
  created_ms    INTEGER NOT NULL
);
-- next_run and cadence_label are derived on read, never columns.

CREATE TABLE scheduled_runs (        -- backs the ↗ "open" affordance
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id     TEXT NOT NULL REFERENCES scheduled_tasks(id) ON DELETE CASCADE,
  session_id  TEXT,
  fired_ms    INTEGER NOT NULL,
  status      TEXT NOT NULL          -- ok | denied | error | cancelled
);
```

### Commands & events

| command | notes |
|---|---|
| `list_scheduled_tasks() -> Vec<ScheduledTask>` | joins store + computed next_run/label |
| `create_scheduled_task(input) -> ScheduledTask` | validates cron; rejects bad expr |
| `toggle_scheduled_task(id) -> ScheduledTask` | flips `paused` |
| `delete_scheduled_task(id)` | rejected for `builtin` |
| `run_scheduled_task_now(id) -> RunOutcome` | the ▶ affordance (mockup 1) |
| `preview_cadence(cron) -> String` | live label for the Custom-cron tab |

Events: `scheduled:fired` `{ id, sessionId, lastRun }` and `scheduled:changed` `{ id }` so
the UI live-updates `nextRun`/`lastRun` without polling.

## 6. Surface gaps in the mockups to resolve

These are visible in the FE design but unspecified by #188; the RFC pins them down:

1. **Run-now (▶) / pause (⏸) per row** → adds `run_scheduled_task_now` and confirms
   `toggle` = pause/resume (mockup 1 shows ⏸ on active, ▶ on paused tasks).
2. **Open (↗) per row** → jumps to the session a fire created; requires the
   `scheduled_runs` linkage above (latest `session_id` for the task).
3. **Built-in tasks** ("Memory Organizer", `Builtin` badge) → cannot be deleted; their
   "fire" is a **named internal action**, not a free-text prompt. Seeded on first run.
   Proposal: a `BuiltinAction` enum (`MemoryConsolidate`, ...) the `TaskRunner` dispatches.
4. **Custom cron tab** → free-text expression validated on input, with a live cadence
   preview via `preview_cadence`.
5. **Contract change for the FE PR:** `CreateScheduledTaskInput` must grow `prompt`,
   `workspace`, `profile`, and `safety_ceiling` (the form already collects the first three).
   Flag to @abidkhan03 before he regenerates against the binding.

## 7. Sequencing — two shippable PRs

**PR-A (this issue's core; ships now, NOT blocked on #74):**
- `ff-scheduled` crate: `model.rs` (+ ts-rs binding), `store.rs`, `cron.rs`.
- Regenerate `bindings/`; delete `lib/scheduled.ts` FE type + `ipc.ts` CONTRACT NOTE.
- Wire the four real commands + `preview_cadence` to the store. **Firing stubbed**
  (`TaskRunner` that records a `denied`/`stub` run). Tasks persist, list, compute
  `nextRun`/`cadenceLabel` — unblocking the FE against real bindings immediately.

**PR-B (behind #74):**
- `runner.rs` tokio loop + the desktop `TaskRunner` impl (real `run_turn` + `ScheduledApprover`).
- `scheduled:fired` / `scheduled:changed` events; `lastRun` stamping; `run_scheduled_task_now`.

**PR-C (follow-up):** built-in seeding + `BuiltinAction` dispatch, `scheduled_runs` linkage
for ↗, and an idle/missed-fire policy (catch-up vs skip on wake).

## 8. Open questions

1. **Missed fires:** if the app was closed when a task was due, on next launch do we
   catch up (fire once) or skip to the next slot? Proposal: **skip** for the first cut
   (a stale digest is noise); revisit per-task `catch_up` later.
2. **Concurrency:** cap concurrent scheduled fires (they compete with the user's live
   session for the provider). Proposal: serialize fires (queue, one at a time).
3. **Safety ceiling default & UI:** default `ReadOnly` is safe but limits usefulness
   (a "git skills update" task needs `Write`). Do we expose the ceiling in the New-task
   form, or infer it? Proposal: expose it as an "Allow file changes" toggle = `Write`.
4. **Profile/workspace validity at fire time:** a task may reference a deleted profile or
   a moved workspace. Proposal: fail the fire with a recorded `error` status surfaced on ↗.

## 9. Verification plan

- `ff-scheduled` unit tests: cron parse + `next_run` + `cadence_label` derivation (table-
  driven over the Hourly/Daily/Weekly/Monthly/Custom presets in the form); store CRUD +
  restart durability; `due_tasks` excludes paused and future tasks.
- `ScheduledApprover` tests mirroring `approver.rs`: ReadOnly ceiling denies Write/Dangerous;
  Write ceiling allows Write, denies Dangerous; denial never blocks.
- Desktop: command round-trips under `VITE_FF_MOCK=0`; `bindings/ScheduledTask.ts` generated;
  `lib/scheduled.ts` removed; `pnpm typecheck && lint && test` green.
- Workspace: `cargo test -p ff-scheduled`, `cargo clippy --workspace --all-targets -D warnings`,
  `cargo fmt --check`.
