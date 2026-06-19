# 0008 — Biosphere (Unified User-Context Model)

- **Status:** Proposed
- **Milestone:** time-of-day → next ambient slice; strata convention → with SET.8 (#131); signals → M8 (NeuroForge)
- **Author:** tonytan4ever
- **Depends on:** RFC 0001 (system-prompt injection hook), RFC 0002 (ambient time/location), RFC 0006 (memory system)

## 1. Summary & Goals

FlowForge already knows two things about the user: durable **memory** (RFC 0006 —
`MEMORY.md` + daily logs) and ambient **context** (RFC 0002 — current date +
timezone). They were built as separate features and share no framing. This RFC
unifies them into one model — **Biosphere** — so the agent reasons about the user
as a coherent whole: who they are, how they work, what they are doing now, and the
live situational "weather" around them.

Biosphere is FlowForge's own answer to the who/how/what convention that local-first
assistants converge on, expressed in our genetics/ecology lineage (genotype →
phenotype → Codon; now the *environment* those organisms live in).

Goals:
- One named model that spans durable memory **and** live ambient context.
- A documented section convention for curated memory (Identity / Patterns / Focus)
  that the backend and the Settings UI both honor — no view-layer guessing.
- Coarse **time-of-day** awareness, in front of the model but never a directive.
- A clean seam for NeuroForge (M8): Biosphere is the situational-signal source.
- Zero new memory IPC, zero schema change, fully local-first and user-editable.

## 2. The Biosphere Model

A biosphere is an organism plus the environment it lives in. The user-context
model has the same two parts:

```
Biosphere
├── Durable strata  (what the agent knows — persisted Markdown, user-owned)
│     ├── Identity   (WHO):  role, stable traits, hard preferences
│     ├── Patterns   (HOW):  conventions, working style, recurring decisions
│     └── Focus      (WHAT): current priorities / active work
└── Ambient layer   (the live "weather" — recomputed per session, never persisted)
      ├── Time:     local date + coarse time-of-day band + IANA timezone
      └── Location: coarse, opt-in, override-able (RFC 0002 / #39)
```

The durable strata are **memory** (RFC 0006). The ambient layer is **context**
(RFC 0002). Biosphere is the umbrella that gives both a shared vocabulary and a
single mental model — and the single source NeuroForge reads.

## 3. Durable Strata — a Heading Convention, Not New Plumbing

The strata are **Markdown heading conventions inside the existing curated
`MEMORY.md`** — *not* new files, IPC, or index schema. RFC 0006's data model is
unchanged; this RFC only names the sections it always implied ("the who/how
equivalent").

```markdown
## Identity
- Role, team, stable traits, hard preferences.

## Patterns
- How the user works; conventions; recurring decisions.

## Focus
- Current priorities and active work.
```

| Stratum | ≈ Aki | Backing | Injected ambiently? |
|---|---|---|---|
| **Identity** | who | `## Identity` in `MEMORY.md` | yes (curated) |
| **Patterns** | how | `## Patterns` in `MEMORY.md` | yes (curated) |
| **Focus** | what | `## Focus` in `MEMORY.md` | yes (curated) |

Rules:
- Sections are **optional and lenient**: a missing section is "nothing recorded
  yet," never an error (consistent with RFC 0006's lenient reads).
- The file remains plain, user-editable Markdown. A user (or the agent) may keep
  freeform content outside these headings; the convention organizes, it does not
  constrain.
- `daily/*.md` is unchanged — the append-only journal, the raw material recall
  searches over and `consolidate` distills into the strata.

## 4. The Convention is a Soft-Contract (resolves SET.8 / #131)

The Settings → Memory section (SET.8, #131) renders WHO/HOW/WHAT. Building that as
a *view-layer-only* parsing convention would let an undecided data shape calcify in
the UI. This RFC instead makes the headings a **documented soft-contract** that
three consumers honor:

1. **The Settings UI** (#131) renders Identity / Patterns / Focus from these
   headings — now a real convention, not `@provisional`.
2. **`memory_write`** gains an optional target stratum so durable facts land in the
   right section (Identity vs Patterns vs Focus) instead of an undifferentiated
   append. Backward compatible: no stratum → existing behavior.
3. **`consolidate`** (RFC 0006 P2) groups promoted curated content under the
   canonical headings during its full-file rewrite.

"Soft" because it is convention over Markdown, not a parser that rejects
non-conforming files. The contract is *where things go when structured*, not a
schema the user must obey.

## 5. Ambient Layer

Recomputed each session, never persisted — the live environment.

### 5.1 Time
- **Date** (shipped): `local_date` at date granularity.
- **Time-of-day band** (new): a coarse 4-value enum, *not* a timestamp (§6).
- **Timezone** (shipped): IANA name.

### 5.2 Location
- Per RFC 0002 / #39 — coarse, opt-in (off by default), override-able, never IP-
  derived. Included in the ambient block only when present. No change to that
  RFC's privacy model; Biosphere just gives it a home.

## 6. Time-of-Day — Coarse Band, by Design

Add a 4-band time-of-day to the ambient context. **Not** exact minutes.

```rust
pub enum TimeOfDay { Morning, Afternoon, Evening, Night }
```

Bands (local clock): Morning 05:00–11:59, Afternoon 12:00–16:59, Evening
17:00–20:59, Night 21:00–04:59. (Exact boundaries are a tuning detail.)

**Why a band, not a timestamp.** The system prompt is built so its stable prefix
(persona, skills, tools) is byte-identical across a session, letting the inference
server reuse the KV-cache; the ambient block is deliberately the **last, volatile
section** so it can change without busting that prefix. Date granularity keeps even
the tail stable all day. A 4-band value transitions at most ~3 times per session —
bounded churn confined to the already-volatile tail — so it buys human-meaningful
"evening" awareness at negligible cache cost. Exact minutes would re-render the
tail every turn; rejected.

**Not a directive.** Time-of-day is rendered as situational *context*, never an
instruction. The agent must not gate behavior on it (no "it's late, I'll refuse").
Its real consumers are soft: natural phrasing, and NeuroForge pacing (§8).

## 7. Delivery (Injection)

Unchanged seam: the ambient block is prepended to the system prompt via the RFC
0001 §4 hook, last, after persona/skills. Rendered example:

```
## User context
Current: 2026-06-19, evening (America/Chicago).
Location: Austin, TX, US.        (only when present)
```

Durable strata reach the model through RFC 0006's existing ambient memory block
(curated `MEMORY.md`) — no second injection path. Biosphere reframes; it does not
add a new prompt section.

## 8. NeuroForge / Signals Synergy

Biosphere is the canonical **situational-signal source** for NeuroForge (M8):
time-of-day (late-night → gentler pacing), elapsed-session awareness (break
nudges), location (locale-aware defaults). `UserContext` becomes an `ff-signals`
source, as RFC 0002 §8 anticipated — now with an explicit owner and shape.

## 9. Data-Model Changes

- `UserContext` (`ff-agent`): add `time_of_day: TimeOfDay`, computed in
  `UserContext::now()` from the local clock. The struct stays preformatted/pure;
  the band is the only new field.
- `memory_write` tool: optional `stratum: Identity | Patterns | Focus` argument
  (curated target only); omitted → current behavior.
- **No** new memory IPC, **no** index schema change, **no** new files.

## 10. Phasing

| Phase | Scope |
|---|---|
| **Strata convention** | Document headings; teach SET.8 (#131) + `memory_write` to honor them. |
| **Time-of-day** | `TimeOfDay` band + ambient render. |
| **Location** | RFC 0002 / #39 (unchanged track), now framed under Biosphere ambient. |
| **Signals** | `UserContext` as an `ff-signals` source (M8, NeuroForge). |

## 11. Non-Goals & Open Questions

**Non-goals:**
- A fifth four-layer-model layer — Biosphere is framing over RFC 0006 + RFC 0002.
- Strict-schema memory (rejecting non-conforming Markdown) — convention only.
- Minute-precision time; location history; background polling (per RFC 0002).
- Auto-profiling — strata are only what the user/agent explicitly write
  (RFC 0006 §10).

**Open questions:**
- Time-of-day band boundaries — fixed (above) or user-tunable later?
- Should `Focus` be hand-curated, or primarily fed by `consolidate` from `daily/`?
  (Lean: consolidate-fed, hand-editable.)
- Do we ever surface Biosphere strata to the model *labeled* (e.g. "Identity:")
  or as plain curated text? (Lean: plain text in the ambient block; labels are a
  UI affordance, not a prompt one.)
