# 0024 — Just-in-Time Tool Loading (a three-layer, brain-derived tool search)

- **Status:** Proposed
- **Milestone:** M7+ (context/attention orchestration)
- **Author:** tonytan4ever
- **Depends on:** RFC 0003 (MCP host — the bridge that registers external tools),
  RFC 0018 (tiered & workspace-scoped MCP — per-context server provenance; this RFC is
  the per-*tool* analogue of that per-*server* work), RFC 0001 (phenotype = a switchable
  working set — the natural home for a task-type prior), RFC 0007 (usage-driven decay &
  salience — reused for Layer 3 consolidation), RFC 0016/0022 (context compaction — same
  cache-stable-prefix discipline, applied to the tools block instead of the message region)
- **Tracking issue:** #1107 (epic); Phase 1: #1108
- **Prior art:** Anthropic's "Tool Search Tool" + `defer loading` (2026-07). This RFC
  adopts its Layer 1 wholesale and extends it with two layers that a purely
  engineering-framed design does not reach.

## 1. Summary & Goals

Every turn, FlowForge serializes **every registered tool's full definition** — name,
description, and complete parameter JSON schema — into the provider `tools` block, which
sits in the cached prompt prefix ahead of the messages. There is no filtering for a
top-level turn: `ToolRegistry::openai_tools()` calls `openai_tools_for(None, true)`, and
`allowed = None` means the filter passes *all* tools (`crates/ff-tools/src/registry.rs:250`).

Built-in tools (31, measured at 7,503 tokens) are a fixed, healthy cost. The problem is the
**MCP bridge**: at registry-build time every bridged tool from every connected server is
registered
unconditionally (`for tool in ff_mcp::build_bridged_tools(...) { reg.register(tool) }`,
`apps/desktop/src-tauri/src/state.rs`). Nothing caps or filters them. Connect a few
servers and the tools block balloons before a single question is asked.

**Measured on a live workstation (tiktoken `cl100k_base`), three connected servers:**

| server | tools | tokens | avg/tool |
|---|---|---|---|
| a large internal multi-tool server | 37 | ~27,900 | ~750 |
| a notes/vault server | 39 | ~5,600 | ~140 |
| codegraph | 1 | ~390 | ~390 |
| **MCP total** | **77** | **~33,900** | |

The built-in side was then measured exactly, by serializing the real registry through
`openai_tools()` and counting with `ff_llm::count_tokens` — the same tokenizer the agent
already uses for its own `tool_tokens` accounting:

| part | tools | tokens | mean | median |
|---|---|---|---|---|
| `ToolRegistry::with_defaults()` (ff-tools core) | 17 | **4,691** | | |
| desktop-registered additions (`state.rs`) | 14 | **2,812** | 200 | 121 |
| **built-in total** | **31** | **7,503** | ~242 | ~121 |

So the full standing tools block is **≈41,400 tokens per turn** across **108 tools** —
spent purely to keep tools *on standby*, before any work. For scale, Anthropic reported
~55K tokens for five busy servers; we reach ~41K with three, one of which contributes a
single tool. **MCP is 81% of the total (33,890 / 41,400)**, and one large internal
multi-tool server alone accounts for **67% of the whole tools block**. That server is also
extremely head-heavy: its heaviest single tool is ~2,300 tokens (a polymorphic
"one tool, dozens of actions, doc-length description" shape), while the median tool across
all servers is ~200. That long tail is exactly what just-in-time loading exists to defer.

The built-in distribution, by contrast, is healthy (median 121) and should **not** be
deferred: these are the high-frequency, general-purpose tools, and 31 of them alone sits
comfortably inside the accuracy sweet spot. This is why Phase 1 defers MCP only. Three
built-ins are nonetheless notably heavy — `observer` (527), `notebook_runner` (519) and
`process_manager` (386), together 1,432 tokens or 19% of the built-in cost. That is a
description-discipline cleanup, not a deferral target, and is out of scope here.

The cost is not only tokens. Past ~30–50 simultaneously-offered tools, model
tool-selection accuracy measurably degrades — too many similar options dilute attention.
At 108 tools (77 MCP + 31 built-in) we are ~2–3× over that threshold, so blindly adding
servers **silently lowers agent accuracy** on top of the token tax.

> **Thesis:** Stop trying to fit every tool into context up front. Keep only a small,
> cache-stable core plus a *search* affordance in the prefix; defer the rest so they are
> *findable but not loaded*; and inject the few that are actually needed — appended at the
> end so the cached prefix is never disturbed. This is the just-in-time principle good
> engineers already apply everywhere else. **But a brain does not merely search on demand
> — it also *pre-activates* the working set it expects to need, and *consolidates* which
> tools fire together into a faster prior. FlowForge should do all three.**

Goals:
- **Cut the standing tools-block cost by the bulk of the MCP contribution** (Anthropic
  reports >85% reduction; deferring the one large server alone reclaims ~28K/turn here).
- **Keep the simultaneously-offered tool count near the accuracy sweet spot** (~a few
  built-ins + a handful of primed/searched tools, not ~100).
- **Preserve — and strengthen — prefix caching** (#947 / RFC 0016), never regress it.
- **Do it as an extension of existing seams**, not a rewrite: the `allowed` allowlist
  parameter on `openai_tools_for` is already the injection point.

## 2. Non-goals

- Not changing the MCP host, supervisor, or bridge wire protocol (RFC 0003/0018 stand).
- Not removing or reshaping any individual server's tools (the polymorphic-tool cleanup
  on the large internal server is a worthwhile but *separate* effort; see §9).
- Not touching sub-agent allowlisting semantics beyond unifying them with the new
  dynamic-allow concept (§6).
- Not a new persistence subsystem: Layer 3 reuses RFC 0007's salience/decay model.

## 3. The three layers (brain-derived)

Anthropic's Tool Search is, at its core, **a flat retrieval index over tool definitions
plus just-in-time loading**. That is good engineering — and it is exactly the kind of
engineering-framed solution FlowForge exists to *deepen* by reasoning from how a brain
actually gates attention. Reasoned from the brain, tool exposure is not one mechanism but
**three**, and only the first is what the video describes.

### Layer 1 — Retrieval (hippocampal-style index) — *adopt wholesale*

Match-on-demand. Keep in the cached prefix only (a) the always-on built-in core and (b) a
single `tool_search` meta-tool. Mark every deferred tool `defer = true`: not loaded into
context, but still discoverable. When the model needs a capability, it calls
`tool_search(query)`; we resolve the query against a retrievable index of deferred tools
and **inject at most ~5** matching full definitions into the next turn.

This is the layer the industry has converged on, and we copy it faithfully. But note what
even a hippocampus does *not* do: it never re-scans the entire store from scratch on every
recall. Pure on-demand search is the floor, not the ceiling.

### Layer 2 — Predictive pre-activation (prefrontal task-set / priming) — *differentiator*

The prefrontal cortex does not wait until a tool is needed to go find it. Given the
current task context, it **pre-activates the working set it expects to use** — a "task
set." You sit down at your coding desk and the tools of coding are already primed; you do
not re-derive "do I have a compiler?" each time.

In the harness: on session start (and as the recent task type becomes evident), use the
**phenotype declaration plus the recent turns' task type** to *predictively pre-inject*
the deferred tools that this class of task usually needs — **without waiting for the model
to issue an explicit `tool_search`**. This is attention-gating that opens *ahead* of
demand by relevance. Concretely it saves the extra round-trip a purely reactive search
costs, and it matches how a person works: the relevant subset is already on the bench.

Layer 2 is where a brain-derived design pulls ahead of the flat-index approach: retrieval
is reactive; priming is *anticipatory*.

### Layer 3 — Consolidation & co-activation (what fires together, wires together) — *deepest differentiator*

The hippocampus records **which things co-occur in which context**, and during sleep
consolidates those co-firings into faster, more direct pathways — a learned prior.

In the harness: **record which tools are actually invoked together within a task type, and
which `tool_search` hits were actually *used* after being injected** (vs. searched and
ignored). Offline — a consolidation pass, reusing RFC 0007's usage-driven salience/decay
machinery — fold that history into a "task type -> tool subset" prior. Next time a similar
task begins, Layer 2's pre-activation is *more accurate* because it draws on consolidated
experience, not a static guess.

This turns tool-search from a **static index** into a **living one that sharpens with
use** — precisely the "hippocampal index: retrieve-then-load into working memory" model,
with the missing ingredient (**consolidation**) added. Anthropic stops at Layer 1.
Layers 2 and 3 are FlowForge's body-length lead, and they are not a bolt-on: they are the
same context/attention-orchestration principle that governs memory (RFC 0007) and
compaction (RFC 0016/0022), applied to the tool surface.

### How the layers compose

```
                        cached prefix (never reordered)
   built-in core  +  tool_search  +  [ primed set (L2) ]        user question
        (always)       (always)         (anticipatory)
                                             |
                              on-demand tool_search hits (L1)
                                             |
                                   appended at the END, only-grow
                                             |
                    usage recorded --> offline consolidation (L3) --> better L2 next time
```

- **L1** is the safety net: whatever L2 fails to anticipate, the model can still find.
- **L2** removes the round-trip in the common case by priming the likely set.
- **L3** makes L2 progressively right, so L1 fires less and less over time.

## 4. Prefix-caching compatibility (the crux with #947 / RFC 0016)

An earlier read of this problem worried that a *dynamic* tool set fights #947, which sorts
the tools block into a stable order precisely so the cached prompt prefix stays
byte-identical across turns (Bedrock only reads a cache entry back when the prefix
matches; reordering forces a cold prefill and dominates TTBF). **The append-at-end design
resolves that concern — and makes caching stronger, not weaker:**

- **Today:** all ~77 MCP tools live in the prefix. Change the connected-server set, or the
  HashMap iteration reorders (before #947's sort), and the whole prefix busts.
- **With JIT loading:** the prefix shrinks to *built-in core + `tool_search` + the L2
  primed set*, which is small and stable for a given session/phenotype. Deferred tools
  found via L1 are **appended after** the stable region — only-grow, never reorder, never
  insert ahead. The cacheable prefix bytes are unchanged as tools accrue within a turn
  sequence, so cache hit-rate goes **up**.

The one discipline this imposes on request assembly: the tools block is emitted *before*
the messages, so newly-injected tools must be appended **at the tail of the tools array
and never re-sorted into the existing region** within a cache epoch. That is the single
invariant to enforce and test. (#947's name-sort still applies *within* the stable core
and *within* an appended batch; it must not re-sort the core+appended concatenation as a
whole, or the tail would migrate into the prefix.)

Interaction with L2/L3: pre-activation (L2) changes the primed set *between* sessions or
task-type transitions — natural cache epoch boundaries — not mid-sequence, so it does not
churn a live prefix. Consolidation (L3) is fully offline and never touches a live request.

## 5. Retrieval quality (Layer 1 detail)

MCP tool descriptions vary wildly in quality; naive keyword match will miss. Options,
cheapest first:
1. **Keyword / BM25 over name+description+action-list.** Zero new infra. Weak on the
   large server's polymorphic tools whose real capability is buried in a long description.
2. **Reuse existing semantic retrieval.** FlowForge already ships semantic search
   affordances; index deferred tool definitions the same way and match `tool_search`
   queries semantically. Better recall on the polymorphic tools.
3. **Action-level indexing for polymorphic tools.** The heaviest offenders pack dozens of
   `action` enum values into one tool. Index *actions*, not just tools, so a query can hit
   "describe-deployment" without loading the entire multi-action monster. Highest payoff,
   most work; can follow Phase 1.

`tool_search` itself must carry a self-describing prompt so the model knows deferred
capability exists and how to reach it — a deferred tool must not become an *invisible*
tool. The meta-tool's description enumerates the *categories* available (cheap) without the
full per-tool schemas (expensive).

## 6. Seam & implementation

The injection seam already exists:

```rust
// crates/ff-tools/src/registry.rs
pub fn openai_tools_for(&self, allowed: Option<&HashSet<String>>, allow_subagent: bool) -> Vec<Value>
```

Today only sub-agents pass `allowed`. Top-level turns pass `None` (= all). The plan makes
top-level turns pass a **dynamic allow-set** too:

- Add `Tool::defer(&self) -> bool` (default `false`; MCP-bridged tools return `true`, or
  are marked deferrable by server per RFC 0018's per-context provenance). A cheap
  server-level toggle — "defer this whole server" — is the Phase-1 knob and already covers
  the MCP mass: deferring the one large internal server alone reclaims 67% of the total
  standing cost, and all three servers together 81%. Because the default is `false`, none
  of the 33 existing `impl Tool` sites change.
- `openai_tools_for` emits: all non-deferred tools (the core) **+** any tool whose name is
  in `allowed` (the L1 hits + L2 primed set). `allowed = None` keeps *today's* behavior for
  callers that want everything (back-compat / tests).
- The `tool_search` meta-tool resolves a query -> tool names, and its result plumbs those
  names into the session's dynamic allow-set for subsequent turns (mirroring how the
  observer wake path buffers then surfaces on the next turn).
- **Unify with sub-agent allowlisting:** a sub-agent's static allowlist and a top-level
  turn's dynamic allow-set become the same `allowed` concept, composed, not two mechanisms.

Layer 2 (pre-activation): a `phenotype`-declared or learned "tool prior" seeds the dynamic
allow-set at session start. Layer 3 (consolidation): a background pass records tool
co-invocation + search-hit-utilization per task type and updates the prior via RFC 0007's
salience/decay.

## 7. Phasing

Each phase is independently shippable and independently valuable.

- **Phase 1 — Layer 1, server-granularity defer.** Add `defer`, default-defer MCP servers
  (keep small healthy ones like the notes/vault server loaded if desired), add
  `tool_search` with keyword retrieval, plumb hits through the dynamic allow-set, enforce
  the append-at-end cache invariant. **Target: ≈41,400 -> ≈8,300 tokens** (the 31
  built-ins at 7,503 plus `tool_search` itself), a ~80% reduction, and 108 simultaneously
  offered tools down to ~32 plus ≤5 on demand — back inside the accuracy sweet spot.
  Highest value / lowest risk.
- **Phase 2 — Layer 1, semantic + action-level retrieval.** Upgrade `tool_search` to
  semantic match and index actions of polymorphic tools (§5.2–5.3). Improves recall so
  deferral does not cost the model capability.
- **Phase 3 — Layer 2, predictive pre-activation.** Seed the dynamic allow-set from
  phenotype-declared and recent-task-type priors. Removes the search round-trip in the
  common case.
- **Phase 4 — Layer 3, consolidation.** Record co-invocation + hit-utilization; offline
  pass folds it into the prior via RFC 0007. Makes Phase 3 sharpen with use.

## 8. Risks & mitigations

- **Invisible-capability regression.** A deferred tool the model never thinks to search
  for is effectively lost. *Mitigation:* `tool_search`'s self-description enumerates
  categories; Phase 3 pre-activation surfaces likely tools without a search; evals must
  include tasks whose right tool is deferred.
- **Cache invariant violated by a careless sort.** *Mitigation:* a golden test asserting
  the tools-block prefix is byte-stable across a turn sequence while tools are appended.
- **Retrieval miss on polymorphic tools.** *Mitigation:* Phase 2 action-level indexing.
- **L2 cold start.** No history at first session. *Mitigation:* conservative phenotype
  defaults; L2 degrades gracefully to L1 (search still works).
- **Over-eager L2 priming re-inflates the block.** *Mitigation:* cap the primed set small
  (a handful); it competes on the same accuracy budget as L1 hits.

## 9. Related, separate work

The polymorphic "one tool, dozens of actions, doc-length description" shape on the large
internal server is the single biggest contributor (~2.3K tokens for one tool). Splitting or
slimming those tools is worthwhile **independently** of this RFC and compounds with it
(action-level indexing in Phase 2 is the JIT-side lever). Tracked separately.

## 10. Open questions

- Keyword vs. semantic for Phase 1 — is BM25 good enough to ship, or is the polymorphic
  recall problem bad enough to need semantic from day one?
- Where does the task-type signal for L2 live — phenotype only, or a lightweight classifier
  over recent turns?
- Cache epoch boundaries for L2 re-priming — session start only, or also on detected
  task-type transition mid-session (and is the re-prefill worth it)?
- Do we defer per-server (cheap, Phase 1) or per-tool (finer, needs a policy) — and who
  owns that policy, the phenotype or a global default?

## Appendix A — Measurement method

Numbers in §1 were obtained by connecting to each configured MCP server over stdio
JSON-RPC (`initialize` -> `tools/list`), serializing each returned tool into the provider
`tools` block shape (`{type:function, function:{name, description, parameters}}`), and
counting tokens with `tiktoken` `cl100k_base`. The exact per-tool distribution
(min ~86, median ~200, max ~2,300 tokens)
confirms a heavy long tail concentrated in one server — the profile just-in-time loading
is designed for. Re-run before Phase 1 to set a precise baseline and after to verify the
reduction target.

The built-in figures were measured separately and exactly, not estimated: a temporary
integration test in `ff-agent` (which depends on both `ff-tools` and `ff-llm`) built a real
`ToolRegistry::with_defaults()`, serialized it through `openai_tools()`, and counted with
`ff_llm::count_tokens` — the same tokenizer the agent already uses for its own
`tool_tokens` accounting, so the number matches what `context_breakdown` will report. That
covers the 17 core tools (4,691 tokens). The 14 desktop-registered additions
(`state.rs`: web_search, skills×4, memory×4, process_manager, observer, notebook_runner,
compaction_retrieve, goal_complete) each need `AppState`-scoped dependencies to construct,
so they were counted from their live definitions in the same tools-block shape with
`tiktoken cl100k_base` (2,812 tokens). Built-in total: **7,503 across 31 tools**.

A note on reproducing this: the probe was temporary and deliberately not committed. Phase 1
should instead assert the baseline through the existing `tool_tokens` / `tool_specs` fields
in `context_breakdown`, which is the durable instrument and needs no new code.

## Appendix B — Code survey: the seams already exist

A survey of the current implementation found that most of what Layer 1 needs is already
present, which is what makes Phase 1 cheap. Exact sites:

| Capability needed | Already present | Site |
|---|---|---|
| Default-provided trait methods for tool classification | 5 of them (`max_safety`, `min_safety`, `interactive`, `reaches_network`, `dedupe_key`) — adding `defer()` fits the established pattern | `crates/ff-tools/src/registry.rs:99-190` |
| Centralized allowlist computation (layered intersection) | `advertised_tools(mode, egress, matrix, allowed, registry)` — a pure `mode` ∩ `egress` ∩ `allowed` pipeline; the defer pass is one more layer | `crates/ff-agent/src/lib.rs:898` |
| **Single** production injection point | `openai_tools_for(advertised.as_ref(), allow_subagent)` — the only production caller in the repo | `crates/ff-agent/src/lib.rs:1124` |
| Precedent for mutating the tools block mid-turn | `withhold_tools` already empties it on the final iteration, and each iteration rebuilds `ChatRequest` with `tools: tool_schemas.clone()` | `crates/ff-agent/src/lib.rs:1799-1803` |
| Session-aware tool dispatch (needed so `tool_search` can scope its allow-set) | `Tool::run_with_session(args, root, session_id)`; the `observer` tool is a working precedent — holds an `Arc<Supervisor>` and isolates by session | `registry.rs:169`, `crates/ff-observer/src/tool.rs:144` |
| Tools-block token instrumentation | `tool_tokens` + `tool_specs` in `context_breakdown` | `crates/ff-agent/src/lib.rs:858-875` |
| Cache hit/miss telemetry (to prove the prefix invariant empirically) | `cache_hit_tokens` / `cache_miss_tokens` on `AgentEvent::Done` | `crates/ff-agent/src/lib.rs:2067` |

Two consequences worth stating explicitly:

1. **`Tool::defer()` with a `false` default means none of the 33 `impl Tool` sites change.**
   Deferral is opt-in, so the blast radius is the MCP bridge plus the advertise pipeline.
2. **Mid-turn injection is architecturally already possible.** `tool_schemas` is computed
   once (`lib.rs:1122`) and cloned per iteration; `withhold_tools` proves the tools block
   can legitimately differ between iterations of the same turn. Phase 1 needs
   `let mut tool_schemas` and an append, not a redesign of the loop.
3. **No new measurement infrastructure.** `tool_tokens`/`tool_specs` give the before/after
   token delta and `cache_hit_tokens`/`cache_miss_tokens` verify that append-at-end really
   does preserve (and improve) cache hits, rather than assuming it.

One design risk this survey also surfaced: `advertised_tools` is where the **permission
matrix** (mode/egress) is enforced. The defer pass must compose as an *additional*
restriction plus a re-admission of allow-listed names — a deferred tool re-admitted by
`tool_search` must still be filtered by mode/egress. Deferral must never become a
privilege-escalation bypass; §7 Phase 1 carries an explicit test for this.

## Appendix C — LOE

Grounded in Appendix B (all Phase 1 work is an extension of an existing pattern; no new
subsystem).

| Phase | LOE | Risk |
|---|---|---|
| 1 — Layer 1, server-granularity defer | **3–4 days** | Low |
| 2 — Layer 1, semantic + action-level retrieval | **4–6 days** | Medium (retrieval quality needs evaluation) |
| 3 — Layer 2, predictive pre-activation | **3–5 days** | Medium (task-type signal is an open design question) |
| 4 — Layer 3, consolidation | **5–8 days** | High (implementation only; excludes calendar time to accrue data and tune) |

**Total ≈ 15–23 days**, but Phase 1 stands alone and pays for itself immediately.

Phase 1 breakdown:

| Work | Est. | Risk |
|---|---|---|
| `Tool::defer()`, bridge flag, defer pass in `advertised_tools` | 0.5 day | Low — mechanical |
| `tool_search` meta-tool + keyword index | 1.5–2 days | Medium — recall against doc-length descriptions |
| Mid-turn injection + append-at-end invariant | 1 day | Medium — the cache invariant is the sharp edge |
| Tests (incl. golden prefix-stability test) + regression | 0.5–1 day | Low |

The dominant Phase 1 risk is **keyword recall against polymorphic tools** whose real
capability is buried in a long description. If evaluation shows poor recall, Phase 2 is
promoted from optimization to prerequisite and pulls forward.
