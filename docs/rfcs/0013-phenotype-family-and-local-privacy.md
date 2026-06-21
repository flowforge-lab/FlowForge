# 0013 — Phenotype Family & Local-Privacy Enclave

- **Status:** Proposed
- **Milestone:** _M4 (phenotype maturity)_
- **Author:** tonytan4ever
- **Depends on:** RFC 0001 (skills & phenotypes), RFC 0011 (Plan/Act/Auto modes — the toolset-filtering seam), #332 (multimodal attachment scaffold), RFC 0005 (provider connections)
- **Tracking issue:** #350 (epic)

## 1. Summary & Goals

FlowForge ships two phenotypes today: a reserved, empty `default` (the deletion-proof
fallback) and a seeded **`codon`** (码子) — a *coding specialist* (codegraph DNA, a
disciplined Research -> Plan -> Implement -> Verify engineer persona,
`max_iterations = 50`) that is the current factory-active default (#298, explicitly a
"for now" choice "until we revisit defaults"). Codon is a specialist, not a generalist;
there is no general-purpose orchestrator and no privacy-restricted pheno. This RFC
proposes rounding out a curated **phenotype family** — adding three opinionated,
ready-to-use phenos alongside `codon` — and the one piece of new machinery they need to
be honest: a **network-egress policy** so a phenotype can guarantee "no data leaves this
machine," not just "the model runs locally."

The three new phenotypes (joining the existing `codon`):

- **`orchestrator`** — the *proposed* new factory-active default. An Aki-equivalent
  generalist tuned to *decompose, delegate, and summarise* rather than to out-reason a
  frontier model. Cheap, fast, good at routing work to sub-agents and MCP tools — it can
  hand a coding sub-task to `codon`, a hard analysis to `erudite`. (Today's factory
  default, `codon`, is a coding specialist; this RFC revisits that "for now" choice — see
  §5.)
- **`erudite`** — a deep-reasoning specialist. Pins a strong "thinking" model, a high
  iteration cap, and a rigorous-analysis persona. The pheno the orchestrator delegates
  hard problems to.
- **`enclave`** — a strict-privacy, local-only pheno for organisations that cannot let
  PII cross the internet. Pins a local model (Ollama / candle-vllm) **and** runs under a
  local-only egress policy that strips network-capable tools from the advertised set.

Goals:

- Round out the working set: a general-purpose default plus a small bench of specialists
  (`codon` for code, `erudite` for reasoning, `enclave` for privacy), instead of only a
  coding specialist or a blank `default`.
- Make "no PII over the internet" an **enforceable** property of a pheno, not a hope
  that rests on the user picking a local model.
- Reuse the existing toolset-filtering seam (RFC 0011 / #240) so the egress policy is a
  thin, well-understood addition — not a parallel subsystem.

Non-goal up front: this is not an OS-level network sandbox (see §10).

## 2. Background — why these model choices (early 2026)

The pheno definitions below pin specific model *classes*; this section justifies them so
the choices are grounded, not asserted.

- **Vision is "image-in, text-out" everywhere that matters.** Claude Opus and the strong
  open VLMs are multimodal in the *vision* sense (image + document input), not
  any-to-any. No audio/video/image-generation in scope.
- **Best open VLM right now: Qwen3-VL** (dense + MoE, 256K context, strong visual
  *reasoning*, agentic use, OCR). It ships explicit **Thinking** variants. Frontier
  proprietary still edges it on the hardest reasoning; open models are at parity on
  document understanding and visual QA.
- **Best *local* VLM with real reasoning: Qwen3-VL-8B-Thinking** — fits a single
  consumer GPU (~16-24 GB depending on quant) and has first-class Ollama support
  (`qwen3-vl:8b`, Ollama >= 0.12.7). Step up: Qwen3-VL-30B-A3B (MoE, ~3B active params).
  Low-VRAM fallback: MiniCPM-V / InternVL small variants.
- **Tiny reasoners are now viable** — e.g. VibeThinker-1.5B (text-only) reaches strong
  math/code reasoning at 1.5B params. This is the proof point behind splitting
  `orchestrator` (cheap router) from `erudite` (strong reasoner): you no longer need one
  giant model to do both, and a small specialist can carry the reasoning load.

These are *guidance defaults*, not hard pins — every pheno's `model` is overridable, and
the enclave pheno in particular will track whatever local VLM the deployment has.

## 3. What already exists (so this stays thin)

- **Phenotype model** (`ff_core::Phenotype`): `{ name, skills[], model?, persona?,
  max_iterations? }`, loaded from `~/.flowforge/phenos/<name>.toml` (RFC 0001 §7). A
  built-in `default` (reserved, empty) always exists so the app has a valid baseline.
  A pheno can already pin a model, a persona, and an iteration cap — that covers
  `orchestrator` and `erudite` with **zero new fields**.
- **Existing seed + selection machinery** (#304, #298): `seed_builtin_content` writes
  `~/.flowforge/phenos/codon.toml` and the bundled `codegraph` skill *write-if-absent* on
  first run, and `initial_phenotype(persisted, resolve)` resolves the factory-active pheno
  (persisted choice -> `codon` -> built-in `default`). The new phenos slot straight into
  this seed path; changing the factory default is a one-line change to that resolver's
  preferred name. `codon` itself is the model: a seeded `.toml` carrying skills + persona
  + `max_iterations`, `model` intentionally unset so it inherits the connection's model.
- **Toolset filtering** (RFC 0011 / #240): the agent already filters the *advertised*
  toolset per turn — Plan mode advertises only `Safety::ReadOnly` tools via
  `ToolContext.allowed`. The egress policy reuses this exact seam, keyed on a tool's
  network behaviour instead of its `Safety`.
- **Multimodal scaffold (#332):** `ff_core::{Attachment, AttachmentKind::Image,
  AttachmentSource}` and `LlmMessage::multimodal(...)` exist. **No provider consumes
  attachments yet** — every provider's `from_*` hardcodes `attachments: Vec::new()`.
  So the message layer is ready; the provider send-path wiring is the open dependency
  for the enclave *vision* path (see §7).
- **Providers** (RFC 0005): candle-vllm, Ollama, Bedrock, SiliconFlow, native Anthropic
  (#326). Local inference = Ollama or candle-vllm.

## 4. The egress policy — the one piece of new machinery

Pinning a local model stops *inference* egress. It does **not** stop a tool from
shipping PII out: `web_fetch`, `web_search`, a `bash` `curl`, or an outbound MCP server
can all reach the internet regardless of which model is answering. "No PII over the
internet" therefore needs a control at the **tool layer**, not the model layer.

Proposal: a tool gains a coarse **network classification** — does invoking it reach the
network? — and a phenotype carries an **egress policy** over that classification:

```
egress = "open"        # default: all tools advertised (today's behaviour)
egress = "local-only"  # network-capable tools are stripped from the advertised set
```

Mechanics (mirrors Plan-mode filtering exactly):

- Tools declare whether they are network-capable. Built-ins are statically classified
  (`web_fetch`/`web_search` = network; `bash` = network-capable-by-default and so
  **excluded** under `local-only`, fail-safe; file/view/memory = local). MCP-bridged
  tools are treated as **network-capable unless proven otherwise** (fail safe, not fail
  open) — an MCP server is an arbitrary external process.
- When a pheno's `egress == "local-only"`, the advertised toolset is filtered to local
  tools only, the same way Plan filters to ReadOnly. The model never sees a tool that
  could leak data.
- The egress policy composes with Mode: `enclave` + `Plan` = local read-only tools
  only; `enclave` + `Auto` = local tools, writes auto-approved.

Where it lives (decision deferred to §10): a new `egress` field on `Phenotype`, since it
is a property of the working set, not of the per-turn autonomy dial (Mode). Keeping it on
the pheno means a deployment ships one `enclave.toml` and every session bound to it is
local-only by construction.

## 5. The three phenotypes

Seed files land in the same place user phenos do (`~/.flowforge/phenos/<name>.toml`),
written on first run if absent (the existing write-if-absent seed path).

**`orchestrator`** (proposed factory-active)

```toml
# orchestrator.toml
skills = ["delegation", "summarisation"]   # indicative; resolved against the registry
model = ""                                  # unset -> the connection's default model
persona = """
You are an orchestrator. Decompose the request into sub-tasks, delegate each to the
right sub-agent or tool, and synthesise a concise result. Prefer routing over solving
everything yourself; hand coding to a coding pheno (codon) and hard reasoning to a
reasoning specialist (erudite).
"""
max_iterations = 20
egress = "open"
```

**`erudite`**

```toml
# erudite.toml
skills = []
model = ""        # pin a strong "thinking" model at deploy time
persona = """
You are a reasoning specialist. Think step by step, state assumptions, check your work,
and prefer rigour over speed. Produce a defensible answer, not a fast one.
"""
max_iterations = 40
egress = "open"
```

**`enclave`**

```toml
# enclave.toml
skills = []
model = "qwen3-vl:8b"   # local VLM via Ollama; override per deployment
persona = """
You operate in a privacy-restricted environment. No data may leave this machine. You
have only local tools; network tools are unavailable by design. Do not attempt to
exfiltrate, encode, or route any user data outward.
"""
max_iterations = 30
egress = "local-only"
```

The built-in `default` pheno is **unchanged** — it stays the reserved, empty safety net
the code relies on (`DEFAULT_PHENOTYPE = "default"`, the final fallback in
`initial_phenotype`). We do **not** redefine it.

The factory-**active** selection is the live question. Today it is `codon` (#298), which
the code comments flag as a "for now" default "until we revisit defaults" — this RFC is
that revisit. `codon` is a *coding* specialist; landing every new user in a coding pheno
is the wrong first impression for a general assistant. Proposal: move the factory-active
selection to **`orchestrator`** (a one-line change to the preferred name in
`initial_phenotype`), and keep `codon` seeded and one switch away for coding sessions —
the orchestrator can also delegate coding sub-tasks to it. `default` remains the bare
fallback when no seed landed. A persisted user choice still always wins, so no existing
user who selected `codon` is disturbed.

## 6. Composition story

`orchestrator` routes, `codon` codes, `erudite` reasons, `enclave` contains:

- The orchestrator delegates a coding sub-task to a `codon` sub-agent (codegraph DNA,
  the verify loop) and a hard analytical sub-task to an `erudite` sub-agent, then
  summarises — a small router model driving specialists, no single giant model.
- In a privacy-restricted deployment the active pheno is `enclave`; every session is
  local-only by construction, including any sub-agents it spawns (the egress policy is
  inherited by children, same as the Mode/allowlist inheritance today).

## 7. Multimodal dependency (not fully specced here)

`enclave`'s value proposition includes "show the local model a screenshot/document
without it leaving the box." That needs the provider send-path to actually consume
`Message.attachments` (#332 left this to per-provider tickets). Scope for this RFC:

- **In scope:** name the dependency; pick **Ollama first** (it is the local path and
  speaks the OpenAI-compatible image format the enclave model uses).
- **Out of scope / follow-up:** the full provider-vision wiring design (image
  materialisation from `AttachmentSource::Path`, base64 at send time, per-provider
  content-part shaping). If that grows its own design weight it becomes RFC 0014; until
  then it is tracked as implementation tickets under this epic.

## 8. Data model

- New `Phenotype.egress: Egress` field (`enum Egress { Open, LocalOnly }`,
  `#[serde(default)]` = `Open` so existing phenos and the on-disk TOML are
  backward-compatible). ts-rs export for the Settings UI.
- Tool network classification: a method on the tool trait (e.g. `fn reaches_network(...)
  -> bool`) with a conservative default of `true` for unannotated/MCP tools.
- The advertised-toolset filter in the agent loop gains an egress pass alongside the
  existing Mode/`allowed` pass.
- Three new seed `.toml` files (`orchestrator`, `erudite`, `enclave`) added to the
  existing `seed_builtin_content` path alongside `codon`; the factory-active preferred
  name in `initial_phenotype` changes from `codon` to `orchestrator`.
- No session-record or IPC schema churn beyond the pheno field.

## 9. Phasing

| Phase | Label | Scope | Ships alone? |
|-------|-------|-------|--------------|
| **P1** | backend | `Egress` enum + `Phenotype.egress` field + tool network classification + the local-only advertised-toolset filter (reusing the #240 seam). Unit-tested in ff-agent/ff-core. | Yes — CLI value |
| **P2** | backend | Seed the three new phenos write-if-absent (alongside the existing `codon`) + change the factory-active preferred name in `initial_phenotype` from `codon` to `orchestrator`. CLI can select any of them. | Yes |
| **P3** | frontend | Surface `egress` in the pheno/Settings UI (a "local-only" badge on the pill so the guarantee is visible); TS bindings. | Yes |
| **P4** | backend | Ollama attachment send-path (the #332 follow-up) so `enclave` can do local vision. | Yes |

Dependency: **P1 -> P2/P3**; P4 is independent and can land in parallel.

## 10. Non-goals & open questions

**Non-goals:**

- **Not an OS-level network sandbox.** The egress policy gates at the *tool layer* — a
  network-capable tool is not advertised to the model. It does not firewall the process,
  block raw sockets a rogue MCP server might open, or stop a local model from being
  exfiltrated by some other channel. This is an honest, stated boundary, like Plan mode's
  tool-layer guarantee in RFC 0011 §12.
- Training, fine-tuning, or fleet management of local models.
- Image generation, audio, or video.

**Open questions:**

- **Egress granularity.** Coarse `open` / `local-only` to start. Do we later want
  per-tool or per-MCP-server allowlisting (e.g. "this one internal MCP is on the
  corporate LAN and is fine")? Likely yes, as a follow-up.
- **Egress home.** A field on `Phenotype` (proposed) vs. a separate per-session switch
  like Mode. Pheno-level is cleaner for a fixed-policy enterprise deployment; a per-
  session switch is more flexible for a mixed user. Proposal: pheno-level now, revisit if
  a per-session need appears.
- **`bash` under `local-only`.** Excluded by the fail-safe default (it can `curl`). Is a
  no-network-restricted bash worth building, or is "no shell in enclave" acceptable for
  v1? Proposal: no shell in enclave for v1.

**Resolved:** the three new names are `orchestrator` / `erudite` / `enclave`, joining the
existing `codon`; the built-in `default` stays the reserved empty fallback. **Revisited
(was #298 "for now"):** the factory-active selection moves from `codon` to `orchestrator`,
with `codon` kept seeded and one switch away.
