# 0006 — Memory System

- **Status:** Proposed
- **Milestone:** M5
- **Author:** tonytan4ever
- **Depends on:** RFC 0001 (system-prompt injection hook; four-layer model), RFC 0002 (ambient-context injection + local-first/consent posture)
- **Tracking issue:** _M5: Memory System_

## 1. Summary & Goals

FlowForge forgets everything between sessions. The model re-learns who the user
is, what they decided last week, and what they are working on, every single turn
of every new conversation. M5 gives FlowForge **durable memory**: facts,
preferences, and decisions that survive a session, are surfaced to the model when
relevant, and remain **plain files the user owns and can edit**.

The differentiator is the same one RFC 0002 claims for ambient context: FlowForge
is a **local-first desktop app**. Memory lives on disk as human-readable Markdown,
the index is a local SQLite database, and recall needs **no cloud round-trip**.
A user can open their memory in any editor, diff it in git, or delete it — no
hosted vector store, no opaque embedding service required.

Goals:
- Durable facts/preferences/decisions persist across sessions.
- Relevant durable memory is **in front of the model** without it having to ask.
- The agent can **recall** older notes on demand (search + targeted read).
- Memory is **captured reliably** — including right before context is lost.
- Everything is local-first, transparent, user-editable, and inspectable.

Non-goals are in §10.

## 2. Prior Art & The Convergent Pattern

Local-first assistant agents (e.g. OpenClaw, Nous Research's Hermes Agent) have
independently converged on the same shape, and we adopt it:

1. **Markdown is the source of truth.** Human-readable, user-editable, git-friendly.
   The model only "remembers" what gets written to disk.
2. **Two layers** — a curated long-term file and an append-only dated daily log.
3. **The index is a disposable accelerator,** not the truth. Rebuildable from the
   Markdown at any time; lexical (FTS5) by default, vectors optional.
4. **Durable writes are nudged right before context is lost** (pre-compaction).

We diverge on one axis: those tools default to **cloud** embeddings (OpenAI).
FlowForge already runs a **local model server** (candle-vLLM, warmed on focus),
so our path to semantic recall is **local-first** — see §4.

## 3. Where This Sits in the Four-Layer Model

RFC 0001 §2 is unchanged. Memory is not a fifth layer; it is **session state** that
feeds two existing seams:
- the **system-prompt injection hook** (RFC 0001 §4, used by RFC 0002 for ambient
  context) — for *ambient* durable memory, and
- the **`ToolRegistry`** (RFC 0001 / M3) — for *on-demand* recall tools.

A new crate `ff-memory` owns the files, the index, and the recall logic. It is
consumed by the desktop shell (injection + flush) and exposes tools through the
same registry MCP and built-in tools use.

## 4. Data Model

Two Markdown layers under the app data dir (alongside `mcp.json`, `provider.json`):

```
~/.flowforge/memory/
  MEMORY.md            # curated, durable: facts, preferences, decisions (ambient)
  daily/YYYY-MM-DD.md  # append-only running log; today + yesterday read at start
```

- **`MEMORY.md`** is the `who/how` equivalent: small, curated, high-signal. It is
  **auto-injected** into the system prompt (§5).
- **`daily/*.md`** is the working log: cheap to append, never auto-injected wholesale
  (only today + yesterday at session start), the raw material recall searches over.

The SQLite index (`memory/index.db`) is a **derived artifact** — deletable and
rebuildable. Schema (FTS5-first):

```rust
/// One indexed unit of memory. Chunked from the Markdown files.
pub struct MemoryChunk {
    pub id: i64,
    pub source: MemorySource,   // Curated { } | Daily { date }
    pub path: PathBuf,          // file it came from
    pub heading: Option<String>,// nearest Markdown heading (for context)
    pub text: String,           // the chunk body (FTS5-indexed)
    pub line_start: u32,        // for memory_get targeting
    pub line_end: u32,
    pub embedding: Option<Vec<f32>>, // None in FTS-only mode (phase 2+)
}

pub enum MemorySource {
    Curated,
    Daily { date: NaiveDate },
}
```

Indexing runs on a **debounced file watch** (the `notify` crate already used by
`ff-mcp`'s config watcher): edit a Markdown file, the affected chunks reindex.

## 5. Delivery: Ambient Injection + Recall Tools (hybrid)

Mirrors RFC 0002's hybrid delivery.

- **Ambient injection.** A compact block — `MEMORY.md` (curated) plus
  today + yesterday's daily logs — is prepended to the system prompt through the
  **same RFC 0001 §4 hook** RFC 0002 uses. Rationale (same as RFC 0002): models
  rarely think to *call* a tool to check what they know; durable context must be
  in front of them. Bounded by a token budget; if `MEMORY.md` exceeds it, the
  injector includes a head + a note to `memory_search` for the rest.
- **Recall tools** for everything past the ambient window:
  - `memory_search(query, k)` — ranked chunks. FTS5/BM25 by default; hybrid when an
    embedding backend is configured (§6).
  - `memory_get(path, line_start?, line_end?)` — targeted read. **Degrades gracefully**
    on a missing file (returns empty text + path, never an error — so the agent can
    handle "nothing recorded yet" without try/catch).
  - `memory_write(text, target)` — append to today's daily log, or (with care)
    upsert a curated fact. See §7.

## 6. Recall Backend: FTS5-First, Vectors Optional

The key decision. Recall sits behind a trait so the backend is swappable:

```rust
pub trait MemoryIndex: Send + Sync {
    fn reindex(&self, chunks: &[MemoryChunk]) -> Result<()>;
    fn search(&self, query: &str, k: usize) -> Result<Vec<ScoredChunk>>;
}
```

- **v1 ships `Fts5Index`** (SQLite FTS5 / BM25). Zero cloud dependency, fast, fully
  local, no model required. This is the **default and the floor** — recall always
  works even with no embedding provider.
- **`HybridIndex` (later)** adds embeddings and fuses vector similarity with BM25
  (so exact IDs/symbols still hit). Embeddings come from, in preference order:
  1. the **local model server** (candle-vLLM embedding endpoint) — local-first, no
     cloud, the FlowForge-native path;
  2. the configured cloud provider's embedding API — **explicit opt-in only**.
  If embeddings are unavailable or return a zero-vector, `HybridIndex` falls back to
  BM25 — never a hard failure.

This keeps M5 shippable and local-first now, and leaves semantic recall as an
additive, opt-in enhancement with no schema churn (the `embedding` column is
already reserved in §4).

## 7. Capture: When Memory Gets Written

Three capture paths, increasing in durability:

1. **Explicit.** User says "remember this" → agent calls `memory_write` → appended
   to today's daily log (or curated, if clearly a durable preference). Never kept
   only in conversation.
2. **Pre-compaction memory-flush.** When a session nears auto-compaction, the shell
   fires a **silent agentic turn** ("persist anything durable now; reply `NO_REPLY`
   if nothing"). One flush per compaction cycle. Default prompts are silent so the
   user never sees the turn. This is the single highest-leverage reliability win —
   durable facts are saved *before* the context that produced them is summarized away.
3. **Consolidation (later).** A periodic pass promotes recurring/high-signal facts
   from daily logs into `MEMORY.md` and prunes stale curated entries, keeping the
   ambient-injected file small. Phase 3; manual curation works until then.

For v1, the agent writes the **daily log** freely; **`MEMORY.md` edits are
conservative** (explicit "remember" or consolidation), so the always-injected file
stays small and high-signal.

## 8. Privacy Posture

Same contract as RFC 0002 §1/§7:
- All memory is **local**: Markdown on disk, SQLite index on disk.
- Files are **inspectable and editable** — a Settings pane lists them and opens them.
- **Embeddings are off by default.** FTS-only needs no model and no network.
- Any **cloud** embedding call is **explicit opt-in**, surfaced in Settings.
- A user can **clear** memory (delete files; index rebuilds empty) or disable it
  entirely (`memory.enabled = false`).

No cloud chat tool can offer "your memory is files you own, indexed locally, with
no embedding service required." That is the differentiator.

## 9. Phasing

- **M5.0** — `ff-memory` crate: Markdown layers + `MEMORY.md` ambient injection via
  the RFC 0001 hook. No recall yet. Smallest shippable unit.
- **M5.1** — SQLite **FTS5** index + `memory_search` / `memory_get` / `memory_write`
  tools + debounced reindex.
- **M5.2** — pre-compaction memory-flush turn.
- **M5.3 (optional)** — `HybridIndex`: local-model embeddings first, cloud opt-in;
  Settings controls; consolidation pass.

## 10. Non-Goals

- **No multi-user / shared memory.** Single local user, like the rest of FlowForge.
- **No hosted vector store.** Local SQLite only; cloud is opt-in *embeddings*, not
  storage.
- **No automatic PII extraction or background "profiling."** Memory is what the
  agent or user explicitly writes, not inferred surveillance (cf. RFC 0002's
  "deliberately not tracking").
- **No cross-session memory in group/sandboxed contexts** until session scoping
  exists (see §11).

## 11. Open Questions

1. **Curated-file scope.** Always-inject `MEMORY.md` (like a global profile), or
   scope it to a "main/private" session once multi-context exists? Defaulting to
   always-on for the single-context app today.
2. **Daily-log read window.** Today + yesterday at session start — enough, or make
   it configurable?
3. **Consolidation trigger.** Time-based, size-based (when `MEMORY.md` exceeds the
   injection budget), or manual-only for v1?
4. **Embedding chunking.** Heading-based chunks (§4) vs fixed-size windows — settle
   when M5.3 is scoped.

## 12. Future Work

M5 ships durable memory and local recall. Three follow-on capabilities build on
that base; each is its own RFC and milestone so 0006 stays the stable contract for
the storage model, ambient injection, and recall tools they all extend.

- **M6 — Memory Hygiene (RFC 0007).** Usage-driven decay and dormancy. Chunks gain
  access statistics; recall and ambient hits reinforce them, idle time decays them,
  and below-threshold chunks become *dormant* — excluded from ambient injection to
  protect the token budget, but still returned by `memory_search` (tagged with their
  age). This makes the §7.3 consolidation pass data-driven rather than heuristic.
  Decay state lives in a durable side table so the FTS5 index stays rebuildable (§4).
- **M7 — Cognitive Consistency (RFC 0008).** Temporal fact tracking. When a curated
  fact is superseded, the old assertion is time-bounded rather than silently
  overwritten, so memory has history. A first slice does supersession only; a full
  subject/predicate/object relation graph and a `memory_evolution` recall tool are a
  later phase. The relation layer is **derived and advisory** — Markdown remains the
  canonical source of truth (§2).
- **M8 — Desktop-Native Episodic Memory (RFC 0009).** Context anchors. Captures
  low-sensitivity desktop context (working directory, active window/app) as chunk
  metadata at write time, enabling a `context_filter` on `memory_search`. Opt-in and
  surfaced in Settings; clipboard and network are explicitly out of the first slice
  to honour the §10 "no inferred surveillance" non-goal. This is recall a cloud chat
  tool structurally cannot offer.

Sequencing rationale: M6 is the smallest and lowest-risk (a side table plus a decay
pass over the existing index); M7 adds an extraction step and is scoped down to
supersession first; M8 depends on desktop signal capture and a consent surface, so it
lands last.
