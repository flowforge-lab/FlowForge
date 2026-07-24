# 0023 — Session Confluence (pattern completion / CA3)

- **Status:** Proposed
- **Milestone:** _Open_
- **Author:** tonytan4ever
- **Depends on:** the shipped `fork_session` deep-copy (`ff-session`), RFC 0006
  (memory: canonical user files, local-first, derived & rebuildable), RFC 0010
  (cognitive consistency: canonical vs. derived side tables), RFC 0016 /
  RFC 0022 (reversible, decay-governed compaction — reused for the optional
  post-confluence summary).
- **Tracking issue:** #1073 (epic); blocked by #1074 (persist fork lineage).

## 1. Summary & Goals

FlowForge already ships **fork** (`fork_session`): a deep copy of a session's
transcript into a fresh session so a user can explore a branch without
disturbing the original. Fork is the harness analogue of hippocampal **pattern
separation** (dentate gyrus): take a shared experience and split it into
distinct memories so parallel explorations don't smear together.

Fork has no inverse. Once a user has forked a task three ways to try three
approaches, there is no first-class way to bring the worthwhile parts back onto
one trunk. Today the only workaround is exporting one session's JSON and
attaching it into another — which is not a merge at all: it drops session B in
as an *external document*, so A's context is pushed into *more* separation, not
completion. (The user's own words: "using B's pattern to separate A's pattern.")

This RFC proposes **session confluence**: the inverse of fork. Its thesis:

> **Fork is pattern separation (dentate gyrus); confluence is pattern
> completion (CA3). Fork splits one trunk into parallel explorations;
> confluence rejoins them so the trunk stays clear. A confluence is not a new
> source of truth — it is a *projection* derived from its parent sessions,
> which remain untouched and canonical.**

Goals:
- **Confluence, not mutation.** A confluence produces a *new* derived session.
  Parents are never destroyed or rewritten (contrast: `git merge` moves branch
  pointers). This keeps confluence aligned with RFC 0006/0010: parents stay
  canonical, the confluence is derived & rebuildable while they exist.
- **Same-lineage only (V1).** V1 confluence operates only within a single fork
  **tree**. Because fork is single-parent, the shared history of N sessions is
  their **lowest common ancestor (LCA)** on that tree — an exact, linear,
  free computation. Forking twice just deepens the tree; the algorithm is
  unchanged.
- **Segment-concatenate, never interleave.** V1 concatenates whole segments in
  fork-time order (per-parent order preserved). Interleaving by message
  timestamp is prohibited: it breaks each session's causal chain and splits
  `tool_use`/`tool_result` pairs, which providers reject (HTTP 422).
- **Lineage is structured metadata, not inline text.** Provenance lives in
  structured fields (`origin_session_id`), never as `<session_id>` tags inside
  message content — inline tags pollute and get mimicked by the model and can't
  be queried by code.
- **Reuse the reversible summarizer.** Any post-confluence compression reuses
  RFC 0016/0022's *reversible* compaction (verbatim original retained), not a
  one-shot lossy squash.

Non-goals in §8.

## 2. Neuroscience frame

The hippocampus runs two complementary operations, and a healthy memory system
needs both strong:

- **Pattern separation** (dentate gyrus) — split similar experiences into
  distinct memories so they don't collapse into one blur. **Fork.**
- **Pattern completion** (CA3) — from a partial or overlapping cue, reconstruct
  a single *de-duplicated* whole. Smell a scent, the entire childhood scene
  returns as one memory — not two fragments played side by side. **Confluence.**

This distinction is load-bearing for the design. CA3 completion reconstructs
**the shared part** into one trunk; it does **not** merge the divergent parts.
So confluence must de-duplicate the shared ancestor **P** while preserving the
divergent tails **A' / B' / C'** verbatim — those tails are the *product* of
pattern separation and are exactly what the user forked to produce. Confluence
is therefore CA3 (on the shared prefix) and dentate gyrus (on the tails)
**at the same time** — both sides of a healthy memory system in one operation.

## 3. Relationship to existing subsystems

**vs. RFC 0006 / 0010 (canonical vs. derived).** 0010 takes the position to its
limit: derived side tables never hold truth; deleting `~/.flowforge` loses audit
trails, never canonical content. Confluence is isomorphic: the confluence
session is a **projection** derived from the lineage tree + parent transcripts.
Parents remain canonical; the confluence is rebuildable while they exist, and
degrades (not corrupts) when they don't (§6).

**vs. RFC 0016 / 0022 (reversible compaction).** The optional post-confluence
summary is not a new lossy step — it routes through the shipped reversible
compactor so the verbatim original is always retrievable. "Compression optional"
is thus sharpened to "**compression optional _and reversible_**."

**vs. RFC 0006 memory `consolidate` (the division of labor).** This is the most
important boundary in this RFC. "Make knowledge/experience converge across *any*
sessions" is **already** an organ in FlowForge — memory consolidation. Two
distinct mechanisms, deliberately kept separate:

| | Mechanism | Brain analogue | Combines |
|---|---|---|---|
| **Same-lineage confluence** | this RFC | CA3 pattern completion | fork-tree **transcripts**, exact-dedup the shared prefix |
| **Any-session convergence** | memory `consolidate` (RFC 0006, shipped) | cortical slow consolidation | **semantic facts** across arbitrary sessions, not raw transcript |

Two unrelated sessions' transcripts concatenated produce no completion (there is
no shared ancestor to reconstruct). Genuine cross-session convergence is a
*semantic* operation — feed both sessions' experience to `consolidate` and let
it distill durable memory facts. Therefore **"any session" is a non-goal for
confluence** and is routed to the consolidate line instead; otherwise confluence
slowly degrades into a poor man's consolidate.

## 4. Lineage data model (prerequisite)

Confluence's exact de-duplication needs to know **where P is**. Today it can't:
`fork_session` is a deep copy but persists **no lineage** — the `Session` row has
no `parent_session_id` and no fork point. This is the one thing that is painful
to add later, because already-forked history cannot be back-filled with lineage.
It is split into its own blocking sub-issue.

- `sessions.parent_session_id TEXT NULL` — `REFERENCES sessions(id) ON DELETE
  SET NULL`.
- `sessions.fork_point_seq INTEGER NULL` — the parent's message boundary at fork
  time, so V2 de-dup can locate the shared prefix precisely.
- Written by `fork_session`; NULL on all pre-existing sessions = "lineage root",
  behavior unchanged.
- Lineage is the physical substrate of pattern separation: *remember where you
  branched from.*

## 5. V1 — same-lineage segment concatenation

1. **Selection constraint.** Confluence accepts ≥2 sessions that belong to the
   same fork tree (share a root). Reject cross-tree selections in V1.
2. **Order.** Sort the divergent tails by fork time; concatenate whole segments,
   per-segment order preserved. Never interleave.
3. **De-dup deferred.** V1 tolerates the repeated shared prefix (the "triple-P"
   redundancy) and appends. Because lineage metadata exists, the system *knows*
   which span is the redundant P; confluence scope is bounded, so the redundancy
   is tolerable for V1.
4. **One phenotype.** A session binds exactly one persona/skills/model. The
   confluence session must pick one phenotype (default: inherit the most-recent
   tail's; user-overridable). Inline `origin_phenotype` is provenance only, never
   runtime behavior.
5. **Optional reversible summary.** Large confluences default to one pass of the
   existing reversible summarizer (RFC 0016/0022); small ones skip it. This is a
   *threshold*, not a naked on/off toggle.

## 6. Deletion & degradation semantics

- Fork is a deep copy, so a confluence (and every forked child) is
  **self-contained** — it references no parent rows.
- `messages` cascade-deletes only their own session's rows (`ON DELETE
  CASCADE`). Deleting a parent leaves every child's transcript intact.
- With `ON DELETE SET NULL`, deleting a parent nulls the child's
  `parent_session_id`: the child becomes a lineage "orphan root" — usable,
  forkable, but no longer precisely locatable within the old tree.
- Confluence still works with a deleted ancestor; it merely loses the ability to
  locate P exactly and falls back to treating the branches as independent tails.
  Deleting an ancestor is rare; graceful degradation, not a hard error.

## 7. V2 — de-duplication (the real pattern completion)

With a lineage tree in place, de-dup is a pure increment: compute the LCA of the
selected sessions, collapse the shared prefix P to a single copy, keep the
divergent tails verbatim. Lossless, exact, free (no LLM) — because fork is a deep
copy, P is byte-identical across branches. **The enhancement direction is "turn
on de-dup," not "relax the same-lineage constraint."** Relaxing the constraint
walks backward (toward undifferentiated blur); turning on de-dup walks forward
(CA3 completion goes live).

Known hard case, deferred within V2: if a user edited a message inside the shared
prefix, the branches' P diverge. V2 de-dups only up to the first divergence
point; everything after is kept per-branch.

## 8. Non-goals

- **Any-session confluence.** Cross-lineage transcript merge is explicitly out.
  Cross-session *semantic* convergence is memory `consolidate`'s job (§3).
- **De-duplication (V1).** Deferred to V2 (§7); V1 appends and tolerates the
  redundant shared prefix.
- **Mutating or destroying parents.** Confluence only ever projects a new derived
  session.
- **Interleaving by timestamp.** Prohibited (§1, tool-pair integrity).
- **Multi-phenotype runtime.** A confluence binds exactly one phenotype (§5.4).

## 9. Open questions

- Edited-shared-prefix divergence detection (§7) — heuristic vs. exact.
- Whether the confluence UI should visualize the fork tree (RFC 0006 local-first:
  lineage should be user-visible, not a hidden SQLite edge).
- Post-confluence summary threshold defaults (§5.5).
