# 0007 — Memory Hygiene: Usage-Driven Decay & Dormancy

- **Status:** Proposed
- **Milestone:** M6
- **Author:** tonytan4ever
- **Depends on:** RFC 0006 (M5 storage model: Markdown source of truth, derived FTS5
  index, `MemoryChunk`, ambient injection, recall tools, consolidation §7.3)
- **Tracking issue:** _M6: Memory Hygiene_

## 1. Summary & Goals

M5 (RFC 0006) gives FlowForge durable memory, but everything written stays equally
loud forever. `MEMORY.md` is auto-injected into every system prompt (RFC 0006 §5), so
an unbounded curated file steadily eats the token budget, and stale facts compete with
current ones for the model's attention. RFC 0006 §7.3 names a "consolidation pass" to
prune stale entries but leaves the trigger as an open question (§11.3).

M6 makes that pass **data-driven**. Each chunk accumulates lightweight usage
statistics; recall and ambient hits **reinforce** a chunk, idle time **decays** it, and
a chunk that falls below a threshold becomes **dormant** — dropped from ambient
injection to protect the budget, but still returned by `memory_search` (tagged with its
age) so nothing is ever lost. Memory gets quieter over time without the user pruning by
hand, and recall stays complete.

Goals:
- Keep the ambient-injected set **small and current** automatically.
- **Never delete** — dormancy is reversible; a single recall can wake a chunk.
- Keep the FTS5 index **rebuildable** (RFC 0006 §4): decay state must survive a reindex.
- Be **inspectable** — the user can see and reset weights, like the rest of memory.

Non-goals are in §8.

## 2. Model: Reinforcement and Decay

Each chunk carries three statistics beyond its RFC 0006 §4 fields:

- `weight: f32` — current salience, in `[0, 1]`. New chunks start at `1.0`.
- `last_accessed: i64` — epoch millis of the most recent reinforcement.
- `access_count: u32` — lifetime reinforcement count (signal for consolidation, §6).

**Reinforcement (on access).** A chunk is reinforced when it is *used*, not merely
present:
- it was returned by `memory_search` in the top-`k`, or
- it was part of the ambient block **and** that turn produced a response (a weak
  signal, applied at a lower factor than an explicit search hit).

Reinforcement bumps `weight` toward `1.0` and stamps `last_accessed`:

```
weight = min(1.0, weight + reinforce_gain * (1.0 - weight))
```

(diminishing returns — an already-salient chunk gains little; a faded one recovers fast.)

**Decay (idle).** A periodic pass applies exponential decay by elapsed idle days:

```
days = (now - last_accessed) / ONE_DAY
weight = weight * decay_factor.powf(days)
```

Decay is computed lazily from `last_accessed` (no per-day cron writes): the pass simply
recomputes `weight` for chunks whose `last_accessed` is older than the last pass. Both
`reinforce_gain` and `decay_factor` are config (§5) with conservative defaults.

## 3. Dormancy: Quiet, Not Gone

A chunk whose `weight` drops below `dormant_threshold` is **dormant**. Dormancy changes
exactly two behaviours:

- **Ambient injection (RFC 0006 §5) skips dormant chunks.** This is the whole point —
  the always-injected set shrinks to what is currently salient, protecting the token
  budget. Curated `MEMORY.md` entries that have gone cold stop being prepended every
  turn.
- **`memory_search` still returns dormant chunks**, but tags them so the model knows
  the fact is old, e.g. `[dormant · last recalled ~6 months ago]`. A search hit
  **reinforces** the chunk (§2), so recalling a dormant fact can lift it back above the
  threshold — dormancy is fully reversible and needs no special "undelete".

Dormancy is a **derived predicate** (`weight < dormant_threshold`), not stored state, so
there is no separate lifecycle to keep consistent.

## 4. Persistence: A Durable Side Table

RFC 0006 §4 is explicit that the SQLite index is a **derived artifact, rebuildable from
the Markdown at any time**. Usage statistics cannot be rebuilt from Markdown, so they
must not live in the FTS5 content table (a reindex would wipe them).

M6 adds a **durable side table** that survives `reindex`:

```sql
CREATE TABLE chunk_stats (
    chunk_key    TEXT PRIMARY KEY, -- stable identity, see below
    weight       REAL NOT NULL DEFAULT 1.0,
    last_accessed INTEGER NOT NULL,
    access_count INTEGER NOT NULL DEFAULT 0
);
```

`reindex` (RFC 0006 §4) rebuilds the FTS5 rows but **leaves `chunk_stats` untouched**,
then re-joins stats to freshly-indexed chunks by `chunk_key`. Orphaned rows (content
that no longer exists) are swept on rebuild.

**`chunk_key` must be stable across edits and reindexes.** A raw `MemoryChunk.id`
(rowid) is not — it churns on rebuild. The key is derived from durable content:
`source + nearest-heading-path + a hash of the chunk's normalized text`. This keeps a
chunk's history attached to its *content* through line-number shifts and reindexes;
materially rewriting a chunk's text intentionally starts it fresh at `weight = 1.0`,
which is the right default for genuinely new content.

This keeps the floor RFC 0006 promised intact: delete `index.db` and everything —
content index and decay state — rebuilds; stats simply reset to neutral.

## 5. Configuration

All knobs live beside `memory.enabled` (RFC 0006 §8), with safe defaults so M6 is
inert until tuned:

| key | default | meaning |
|---|---|---|
| `memory.decay.enabled` | `true` | master switch for the whole M6 mechanism |
| `memory.decay.factor` | `0.98` | daily multiplier (~35-day half-life) |
| `memory.decay.reinforce_gain` | `0.3` | search-hit reinforcement strength |
| `memory.decay.ambient_gain` | `0.05` | weak reinforcement for an ambient-only hit |
| `memory.decay.dormant_threshold` | `0.25` | below this → dormant |

With `memory.decay.enabled = false`, M6 records statistics but never decays or marks
anything dormant — behaviour is identical to M5. This is the rollback path.

## 6. Interaction with Consolidation (RFC 0006 §7.3)

M6 supplies the data RFC 0006's consolidation pass was missing:
- `access_count` + `weight` rank which **daily-log** facts are recurring/high-signal
  enough to **promote** into `MEMORY.md`.
- Sustained-dormant **curated** entries are promotion's inverse: candidates to demote
  out of the always-injected file (the daily-log history still holds them; nothing is
  deleted).

M6 does not *require* consolidation to ship — decay + dormancy already bound the
ambient set. Consolidation simply becomes a principled, data-driven pass once both land.

## 7. Surfacing & Inspectability

Consistent with RFC 0006 §8 (memory is inspectable and user-owned):
- The memory Settings pane shows each chunk's `weight` and dormant state.
- A user can **reset** a chunk's weight (wake it) or **pin** it (exempt from decay) —
  pinning is `weight` held at `1.0`, surfaced as a checkbox, stored in `chunk_stats`.
- Decay never touches the **Markdown** — it only changes what gets *injected*. The
  user's files are never edited by the decay mechanism; the source of truth is
  untouched (RFC 0006 §2).

## 8. Non-Goals

- **No deletion.** M6 never removes Markdown or chunks; dormancy is the strongest action.
- **No semantic merging** of similar facts — that is consolidation (RFC 0006 §7.3),
  informed by but separate from M6.
- **No per-chunk ML.** Decay is a closed-form function of time and access, not a model.
- **No cross-session weighting.** Single local user (RFC 0006 §10); one weight per chunk.

## 9. Phasing

- **M6.0** — `chunk_stats` side table + stable `chunk_key`; reinforcement on
  `memory_search` hits; lazy decay pass. No injection change yet (observe weights only).
- **M6.1** — dormancy: ambient injection (RFC 0006 §5) skips dormant chunks;
  `memory_search` tags them. The token-budget win lands here.
- **M6.2** — Settings surfacing: per-chunk weight, reset, pin.
- **M6.3 (optional)** — feed `access_count`/`weight` into the consolidation pass (§6).

## 10. Open Questions

1. **Ambient reinforcement signal.** Is "chunk was in the ambient block and the turn
   produced a reply" too weak/noisy to count as a hit at all? M6.0 records it behind
   `ambient_gain` so it can be tuned to `0` without code change.
2. **Half-life default.** Is ~35 days (`factor 0.98`) right for a personal assistant, or
   should curated facts decay slower than daily-log chunks (per-source factors)?
3. **Pin vs. high weight.** Is an explicit pin needed, or does normal reinforcement keep
   genuinely-important facts alive on its own? Pinning is cheap insurance; revisit after
   M6.1 usage data.
