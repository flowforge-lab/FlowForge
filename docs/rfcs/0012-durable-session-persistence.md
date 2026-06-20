# 0012 — Durable Session Persistence & Export

- **Status:** Proposed
- **Milestone:** _M3 (UX hardening)_
- **Author:** tonytan4ever
- **Depends on:** nothing hard. Complements RFC 0011 P2 (#265 — persisting per-session `mode`); reuses the `ff-memory` `FlushLedger` SQLite pattern.
- **Tracking issue:** #280 (epic)

## 1. Summary & Goals

Make sessions and their full message history **survive an app restart**, and add
per-session **export to Markdown / JSON**.

Today `SessionStore` (`crates/ff-session/src/lib.rs`) is purely in-memory — two
`HashMap`s behind a `Mutex` — and the desktop builds a fresh empty one on every
launch (`apps/desktop/src-tauri/src/state.rs`, `store: SessionStore::new()`), with no
load and no save. So every conversation is lost when the process exits. (The SQLite
already in the repo, `rusqlite` in `ff-memory`, is only the RFC 0006 *memory* index —
unrelated to chat history.)

Goals:

- Conversations persist across restarts, transparently — no frontend change, no
  change to the `run_turn` call sites.
- Keep the exact `SessionStore` public API so its 30+ call sites and all existing
  tests compile and pass unchanged.
- Export any session to **JSON** (lossless, machine-round-trippable) or **Markdown**
  (a clean human-readable transcript).

Non-goals (v1): cross-device sync, encryption at rest, full-text search over history
(could reuse the ff-memory FTS5 pattern later), CLI persistence (the CLI stays
ephemeral), and session *import*.

## 2. Storage impact

A message row is ~150 bytes of metadata (two UUIDs, role, timestamps, `seq`) plus its
content. Content is dominated by tool results (bash / view output), not prose.

| Session size | Rows | Avg row | On disk |
|--------------|------|---------|---------|
| Heavy day | 2,000 | 3 KB | ~6 MB |
| Marathon | 5,000 | 5 KB | ~25 MB |
| Pathological | 20,000 | 8 KB | ~160 MB |

SQLite is comfortable into the multi-GB range, so even a pathological session is
negligible. The binding constraint on a long session is the model's context window,
not storage — and that is already handled by compaction (RFC 0006), which truncates
what is *sent* to the model while the store keeps the *full* transcript. So we get a
complete durable history on disk and a bounded prompt at inference time.

Caveat: `get_messages` returns the whole `Vec` per call (it already does, in RAM).
Under SQLite a giant session re-reads everything each turn — fine at the sizes above.
The `messages(session_id, seq)` index is laid out so paginated / lazy reads can be
added later **without a schema migration**. Flagged as a future optimization, not v1.

## 3. Design — SQLite-backed `SessionStore`

Follow the established in-repo pattern (`ff-memory/src/flush.rs` `FlushLedger`): a
struct holding `conn: Mutex<Connection>` with `open(path)` (disk) and
`open_in_memory()` (tests).

- `SessionStore::new()` -> in-memory SQLite (`:memory:`). Preserves every existing
  test and the ephemeral CLI with **zero behaviour change**.
- `SessionStore::open(path)` -> file-backed; used by the desktop.
- Replace the two `HashMap`s with SQL against the connection. Mutation frequency is a
  handful per turn, so plain SQL is more than fast enough; there is **no in-memory
  mirror** (single source of truth — avoids sync bugs and makes export trivial).
- Every method keeps its signature and return type: `create_session`, `add_message`,
  `add_tool_result_message`, `get_messages`, `list_sessions`, `set_title`,
  `set_status`, `set_session_phenotype`, `session_phenotype`, `set_message_content`,
  `attach_tool_calls`, `delete_session`, `fork_session`.
- `PRAGMA foreign_keys = ON`; `PRAGMA user_version` plus a tiny migration runner from
  day one (even though the v1 migration list is just "create schema").

### Schema

```sql
CREATE TABLE sessions (
  id         TEXT PRIMARY KEY,
  goal       TEXT,
  title      TEXT,
  summary    TEXT,
  status     TEXT    NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  phenotype  TEXT,
  mode       TEXT            -- RFC 0011 P2 (#265): persisted here so mode survives too
);

CREATE TABLE messages (
  id           TEXT PRIMARY KEY,
  session_id   TEXT    NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  seq          INTEGER NOT NULL,   -- per-session insertion order
  role         TEXT    NOT NULL,
  content      TEXT    NOT NULL,
  tool_calls   TEXT,               -- JSON-encoded Vec<ToolCall>, or NULL
  tool_call_id TEXT,
  created_at   INTEGER NOT NULL
);

CREATE INDEX idx_messages_session ON messages(session_id, seq);
```

`Role` / `SessionStatus` map to text via serde; `tool_calls` round-trips through
`serde_json`. The first-user-message auto-title logic in `push_message` is reproduced
as a query.

## 4. Desktop wiring

- `AppState::new` -> `SessionStore::open("~/.config/flowforge/sessions.db")` (alongside
  the existing `provider-registry.json` / `search.json` sidecars), falling back to
  `:memory:` with a `tracing::warn!` if open fails — the same resilience pattern as the
  `FlushLedger` open in `state.rs`.
- The frontend needs no change: it already lists and reads sessions through the store,
  so it repopulates on launch automatically.

## 5. Export

The pure-SQL design makes export almost free: a session's full, ordered, faithful
record already lives in queryable tables.

- **JSON** = serde-serialize `{ session: Session, messages: Vec<Message> }`. These
  types already derive `Serialize` / `TS`, so the export is the canonical wire shape
  (re-importable if import ever lands). A query plus `serde_json::to_string_pretty`.
- **Markdown** = a render pass over the ordered messages: an `# {title}` heading and a
  metadata block (goal, created / updated, phenotype, mode), then `## You` /
  `## Assistant` / `## Tool` sections. Assistant `tool_calls` render as
  `**Tool call:** name(args)`; `Role::Tool` results render under the call they bind to
  via `tool_call_id`. Markdown is a *clean reading transcript* — long tool output is
  folded / truncated and system messages are skipped; **JSON stays fully lossless.**
- Lives as a pure `export_session(id, Format) -> String` in `ff-session`, unit-tested
  on fixture sessions. The desktop exposes a `#[tauri::command] export_session` (same
  shape as `get_messages` / `rename_session`), registered in `generate_handler!`. The
  FE adds an "Export" action (Markdown / JSON) in the session menu that calls the
  command and saves via the Tauri file dialog.
- `Format { Markdown, Json }` lives in `ff-core`, ts-rs-exported (`"markdown" | "json"`),
  mirroring how `Mode` / `SearchBackend` are exported.

## 6. Data model

- No change to `Session` / `Message` wire types (they are already serde + ts-rs).
- New `Format { Markdown, Json }` enum in `ff-core` (ts-rs exported).
- New SQLite schema as in section 3; `mode` column included now so RFC 0011 P2 gets
  persistence for free (one migration instead of two).

## 7. Phasing

| Phase | Label | Scope |
|-------|-------|-------|
| **P1** | backend | SQLite-backed `SessionStore` (`new` -> `:memory:`, `open` / `open_in_memory`), schema + migration runner, full method port. Round-trip / cascade-delete / ordering / auto-title tests. Ships testable alone. |
| **P2** | backend | Desktop `AppState` opens the file DB with `:memory:` fallback; smoke check that a restart preserves sessions. |
| **P3** | backend + frontend | Export: `export_session` core (JSON + Markdown) with fixture tests, the Tauri command, the `Format` binding, and the FE "Export" menu + save dialog. |
| **P4** | backend | *(stretch)* Persist per-session workspace: move `session_cwd` off `AppState`'s `HashMap` into the session row, fixing the secondary restart wart. |

Dependency: P1 -> P2 / P3; P4 last and optional.

## 8. Non-goals & open questions

**Non-goals:** cross-device sync; encryption at rest; history search; CLI persistence;
session import; a retention / pruning policy (v1 keeps everything — a "keep last N /
prune abandoned" policy can be a later RFC).

**Open questions:**

- Should Markdown export be configurable (verbose vs. folded tool output) rather than
  always folded?
- Do we want a one-click "export all sessions" alongside per-session export?

**Resolved decisions:** SQLite (not JSON files); pure SQL (no in-memory mirror); DB at
`~/.config/flowforge/sessions.db`; the `mode` column is added now; Markdown is a clean
folded transcript while JSON is lossless.
