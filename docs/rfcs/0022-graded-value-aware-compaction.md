# 0022 — Graded, Value-Aware Compaction (reusing memory's decay/salience model)

- **Status:** Proposed
- **Milestone:** M7 (follow-on to #933)
- **Author:** tonytan4ever
- **Depends on:** RFC 0016 (multi-medium, decay-governed compaction — this RFC
  operationalizes its Tier 1 grading and Tier 3 decay thesis), RFC 0007 (usage-driven
  decay & salience), the shipped `ExtractiveCompactor` / `AbstractiveSummarizer` /
  `CompactionCache` in `ff-agent`, and #933 levers A.1/A.2 (cache-stable prefix) + B.2
  (ingest-time tool-result compaction).
- **Tracking issue:** #933 (Cache-aware compaction + context compression for TTBF)

## 1. Summary & Goals

The #933 epic closed the **latency wall**: with A.1 (system-prompt cache split) and A.2
(frozen-boundary tier-1 prefix caching), a live 132K-token session now prefills in
~3.5s at a ~21:1 cache read:write ratio. Time-to-first-byte no longer scales with
context size — the provider KV cache absorbs a warm prefix regardless of length.

What remains is the **budget wall**: the message region keeps growing (~100K tokens /
300+ messages on a long session), and once it crosses the Tier-2 fraction the
*abstractive* summarizer engages — the one genuinely lossy step, where early detail is
replaced by an LLM's paraphrase. B.2 (#964) pushes that onset out by compacting large
tool-result blobs at ingest, but it does not change the *shape* of what we keep.

This RFC proposes the shape change, in two steps, and its thesis is:

> **A long session should remember like a person does: recent turns in full, older
> turns progressively blurred, and *important* older turns kept sharp regardless of
> age — with the verbatim original always retrievable. FlowForge already implements
> exactly this decay/reinforcement model for on-disk memory (RFC 0007); in-context
> compaction should reuse that model rather than reinvent it.**

Goals:
- **Graded, not binary.** Replace the current cliff (`KEEP_RECENT_VERBATIM=6` verbatim,
  everything older uniformly compressed) with depth-graded compression strength.
- **Value-aware, not position-only.** Let *what a message is worth* — not just how old
  it is — decide how hard it is compressed, so a key decision survives while a 500-line
  dump fades.
- **Reuse memory's machinery.** Generalize RFC 0007's `Salience` model (recency ×
  frequency, half-life decay, reinforcement, pinning) so the same abstraction scores
  both memory chunks and cold session messages.
- **Marginalize Tier 2.** Make reversible extractive grading strong enough that the
  lossy abstractive summarizer becomes a rare last resort, not a routine step.
- **Cache-stable and additive.** Every grade boundary must be frozen exactly like A.2,
  so grading never busts the prefix cache; nothing changes behavior unless opted in.

Non-goals in §7.

## 2. Where we are (verified against `main`)

The compaction stack today (`ff-agent/src/compaction_extractive.rs`,
`compaction_abstractive.rs`, `compaction_cache.rs`):

- **Tier 1 — extractive, reversible, near-lossless.** `ExtractiveCompactor::compress_one`
  routes by content kind (`compress_json` key-path/array trim, `compress_lines`
  head/tail elision), emits a `[compacted; retrieve key=…]` marker, and stores the
  verbatim original in `compaction_originals` so `compaction_retrieve` fetches it back.
  This is *fold + unfold*, not loss.
- **Tier 2 — abstractive, lossy.** `AbstractiveSummarizer::summarize_cold` collapses the
  whole cold prefix into one LLM-written summary (original still stored, but the model
  now sees the paraphrase). This is the quality cliff.
- **Position is the only axis.** Both tiers split at `keep_recent`: `messages[..cold_end]`
  are treated uniformly; `messages[cold_end..]` are byte-identical. There is **no
  gradient** and **no notion of per-message value** — a one-line decision and a 2000-line
  `codegraph_explore` dump are compressed identically if both fall in the cold region.

Memory, by contrast, already grades by value over time (RFC 0007):

- `chunk_stats` carries a `weight` with **lazy exponential decay** (`last_accessed`,
  half-life), `reinforce` on recall (`reinforce_gain`) and `reinforce_ambient` on
  injection (`ambient_gain`), a derived `dormant` predicate, and `pinned` (never decays).
- `consolidate.rs` defines the `Salience` trait — `score(&chunk, occurrences) -> f32` —
  with a `RecencyFrequencySalience` default (recency = `0.5^(age/half_life)`,
  half-life 14d; frequency = `min(1, occurrences/saturation)`, saturation 3), and a
  `PROMOTION_SCORE_CUTOFF` that already drives promote/demote decisions. There is even a
  standing `TODO(M6.3): LLM-driven Salience` extension point.

**The insight: memory and in-context compaction are the same problem at two timescales
(RFC 0016 §5), and memory already solved the harder half.**

## 3. Step 1 — Graded extractive compaction (small, independent, do first)

Replace the binary cold/recent split with **N depth bands**, each with its own
`ExtractiveCompactor` config, from gentlest (newest cold) to most aggressive (oldest):

- **Band 0 — recent tail** (`KEEP_RECENT_VERBATIM`): byte-identical, unchanged.
- **Band 1 — warm** (next slice): light touch — large `max_value_chars`, generous
  `keep_head_lines`/`keep_tail_lines`. Preserves most structure.
- **Band 2 — cold** : today's default strength.
- **Band 3 — frozen** (oldest): aggressive — tight value/line caps.

All bands are still Tier 1: reversible, marker-tagged, original in
`compaction_originals`. This is purely *within* the extractive compactor — no LLM, no new
failure mode. It directly realizes "older turns occupy less, but nothing is truly lost."

### Design points
- **Band assignment by depth**, computed from message index relative to `cold_end`, so it
  is deterministic given transcript length.
- **Cache stability is mandatory.** Band boundaries must advance in the same frozen,
  monotonic way A.2 froze the tier-1 prefix (`CompactionCache::get_tier1/put_tier1`). A
  message may move to a *deeper* band as the session grows, but its compacted bytes for a
  given band must be stable. Re-compressing an already-marked message is skipped (the
  existing `COMPACTION_MARKER_PREFIX` guard), so a message only re-compresses when it
  crosses a band boundary — a bounded, monotonic event, not a per-turn churn.
- **Config source of truth**: a `GradedConfig { bands: Vec<(depth_threshold,
  ExtractiveCompactor)> }`, env/default-driven, so grading is tunable without code
  changes and default-off-compatible.

### API sketch
```rust
// compaction_extractive.rs — new, alongside compact_cold_collect / compact_range_collect.
pub fn compact_graded_collect(&self, messages: &[Message], bands: &GradedBands) -> ColdCompaction;
```
Same `ColdCompaction` return (messages + originals + savings), so the `run_turn` wiring
and originals-persistence path are unchanged; only the per-message strength selection is
new.

### Tests
- A message compresses more aggressively the deeper its band; band 0 is verbatim.
- Byte-identical output across two calls at the same transcript length (cache stability).
- A message that crosses a band boundary re-compresses exactly once; already-marked
  content is never re-touched.
- Originals for every band resolve via `compaction_retrieve`.

## 4. Step 2 — Value-aware compaction (reuse memory's `Salience`; RFC-worthy)

Grading by depth alone still treats a key decision and a stale dump identically if
they share a band. Step 2 adds the *value* axis by **reusing memory's decay/salience
model**, not by inventing a second scorer.

### 4.1 Generalize `Salience` into a shared abstraction
`ff-memory::consolidate::Salience` is currently typed to `MemoryChunk`. Extract the
*scoring model* (recency × frequency, half-life, saturation, the cutoff discipline, and
the future LLM-Salience extension point) into a form both crates can use — e.g. a small
`ff-salience` scoring core, or a generic `Salience<T>` trait — so:
- memory keeps `Salience<MemoryChunk>` with `RecencyFrequencySalience` unchanged, and
- compaction gains `MessageSalience`, scoring a cold `Message` from signals it *does*
  have (see 4.3).

### 4.2 The reuse boundary (important — avoid a foot-gun)
Memory's decay is persisted in the `chunk_stats` table keyed by `chunk_key` (durable,
cross-session, content-addressed). **Session messages are transient with a fresh id per
turn**, so we do **not** reuse that table. What we reuse is the **model and the trait**
(the recency×frequency formula, half-life shape, reinforcement concept, pin/dormant
semantics, `PROMOTION_SCORE_CUTOFF`-style thresholds) — *not* the storage. This RFC is
explicit about that seam so an implementer does not try to bolt `chunk_stats` onto the
transcript.

### 4.3 `MessageSalience` inputs (all locally available)
- **Recency** — depth from `cold_end` (the same age signal memory gets from `last_accessed`).
- **Frequency / reinforcement** — how often this message's content is *referenced later*
  in the transcript (a later message quoting/retrieving it), mirroring memory's
  `reinforce` on recall. A retrieved original (`compaction_retrieve` was called on its
  key) is a strong reinforcement signal.
- **Role/kind weight** — a user directive or an assistant decision outranks a raw tool
  dump of equal length (the "important sticks" prior).
- **Size penalty** — large low-signal blobs (big `codegraph_explore`/diff outputs) score
  low, so they are the first to be aggressively folded.
- **Pin** — an explicit "keep sharp" marker (or a heuristic for decision-class content),
  mapping to memory's `pinned` (never decays).

### 4.4 How the score is used
The salience score **selects the band** from Step 1 rather than depth alone: high-salience
cold messages stay in a gentle band (or verbatim) even when old; low-salience ones drop to
an aggressive band immediately. Tier 2 (lossy) only fires when even the most aggressive
Tier-1 band cannot bring the transcript under budget — i.e. it becomes the rare last
resort, satisfying the "marginalize abstractive" goal.

### 4.5 Shared future: LLM-driven salience
Memory's standing `TODO(M6.3): LLM-driven Salience` and compaction's value scorer become
the **same upgrade**: when a semantic-similarity/LLM salience impl lands, both memory
consolidation and context compaction inherit it through the shared abstraction. One
scorer, two timescales — the RFC 0016 §5 consilience made concrete.

## 5. Consilience with RFC 0016 and RFC 0007

- RFC 0016 already framed Tier 3 as "decay-as-compaction" reusing the RFC 0007 clock;
  this RFC is the concrete, shippable form of that thesis, sequenced after #933 proved
  the cache-stability groundwork.
- RFC 0007's reinforcement (recall strengthens, disuse fades) is exactly the
  "important older turns stay sharp" behavior — applied to the transcript instead of
  the memory file.
- Reversibility is preserved end-to-end: every band and the summary all store originals,
  so `compaction_retrieve` remains the universal escape hatch. Grading is lossy *in
  context*, never lossy *on disk*.

## 6. Phasing

- **6.1 — Step 1 (graded extractive).** `compact_graded_collect` + `GradedBands` config,
  frozen-boundary stable, default bands = today's behavior (a single cold band) so it is
  a no-op until tuned. Ships independently of Step 2. Depends only on #964 landing.
- **6.2 — Step 2a (generalize `Salience`).** Extract the shared scoring core; memory
  behavior unchanged (regression-locked by its existing tests).
- **6.3 — Step 2b (`MessageSalience` → band selection).** Wire value-aware band choice;
  measure; tune Tier-2 fire fraction downward as Tier-1 proves sufficient.
- **6.4 — (shared, later) LLM-Salience.** The M6.3 memory TODO and compaction value
  scorer land together on the shared abstraction.

## 7. Non-Goals

- **RAG / external retrieval (#939).** This RFC deliberately does *not* add a retrieval
  layer. Graded reversible compaction is the near-term lever; RAG remains the eventual
  "constant context regardless of session length" bet and, if built, becomes the
  long-term *storage backend* for the originals this pipeline already keeps — not a
  replacement for grading.
- **Changing the store.** The transcript stays fully verbatim on disk (B.2's Option-A
  persisted-compaction is a separate, still-deferred track). All grading is a pre-send
  wire transform.
- **New pressure estimator.** Reuses the existing `ProxyTokenEstimator` / `ContextPressure`.

## 8. Open Questions

1. **Band count & thresholds.** How many bands, and depth cutoffs? Start with 3–4,
   defaulted to today's single-band behavior, then tune against measured
   `messageTokens` / Tier-2 fire rate on a real dev session.
2. **Frequency signal cost.** Detecting "referenced later" cheaply — scanning for a
   message's `retrieve key` reuse is O(n) per turn; is a maintained back-reference index
   worth it, or is depth+role+size enough for v1?
3. **Salience extraction shape.** Generic `Salience<T>` trait vs a standalone
   `ff-salience` crate vs a scoring struct passed both call sites — which minimizes churn
   in `ff-memory` while giving `ff-agent` clean access?
4. **Pin heuristic.** Is decision-class content ("we chose B", a user directive)
   detectable cheaply enough to auto-pin, or is pinning explicit-only for v1?
5. **Measurement discipline.** All tuning must use a fresh, blob-heavy dev session with
   per-turn `prefill_estimates` / cacheWrite deltas — never a polluted meta-session, and
   never `breakdown.messageTokens` alone (it reflects the verbatim store, not the
   wire-only compacted size).

## 9. References

- RFC 0016 — Multi-Medium, Decay-Governed Context Compaction (Tiers 0–3; the parent vision).
- RFC 0007 — Usage-driven decay & salience (`weight`, half-life, `reinforce`, `dormant`, `pinned`).
- RFC 0006 — Memory system (Markdown source of truth, `compaction_retrieve` reversibility).
- #933 — Cache-aware compaction epic (A.1 #950, A.2 #951/#955, B.2 #964).
- #939 — RAG / external retrieval (explicit non-goal here; eventual Phase-2).
- Microsoft LLMLingua / LLMLingua-2 — extractive token-pruning prior art (RFC 0016 §11).
