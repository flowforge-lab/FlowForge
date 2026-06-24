# 0016 — Multi-Medium, Decay-Governed Context Compaction

- **Status:** Proposed
- **Milestone:** M7
- **Author:** tonytan4ever
- **Depends on:** RFC 0006 (memory system: Markdown source of truth, FTS5 index,
  ambient injection, recall tools), RFC 0007 (usage-driven decay & dormancy), and the
  `CompactionStrategy` / `ContextPressureEstimator` seam already in
  `ff-agent/src/compaction.rs` (M5.2)
- **Tracking issue:** _M7: Context Compaction_

## 1. Summary & Goals

A long-running session eventually overruns the model's context window. Today FlowForge
detects the pressure (the `ProxyTokenEstimator`, a `chars/4` proxy vs a budget) and runs
a single strategy — `MemoryFlush` — which persists durable facts to on-disk memory
before they would be lost. But FlowForge **never actually compacts the transcript**: the
flush writes to memory, then the full history is still sent to the model. The seam was
built (RFC 0006 §7.2; `CompactionStrategy` is "the *how* seam … future strategies become
sibling implementations the host can swap in") but the compress step was deferred.

This RFC fills that gap, and reframes the gap while doing so. The thesis:

> **Context compaction is not "summarize the transcript." It is a content-aware,
> multi-medium, decay-governed, reversible token-budget pipeline — and it should share
> the same temporal/decay machinery as long-term memory, because in-context compaction
> and on-disk memory are the same problem at two timescales.**

Goals:
- **Preserve the most relevant information per token spent** — measured against task
  outcome, not transcript fidelity.
- **Reversible by default** — compaction is lossy *in context* but the original is
  retrievable on demand, so a dropped detail is recoverable, never gone.
- **Content-aware** — code, JSON/tool-output, and prose compress by different means; a
  single summarizer is the wrong default.
- **Unified with memory** — reuse RFC 0007 decay (`weight`, `last_accessed`, half-life)
  and the temporal timeline (`consolidate.rs` recency x frequency) so cold context is
  *progressively* re-compressed as it ages, exactly as memory fades.
- **Additive** — every tier plugs into the existing `CompactionStrategy` trait; nothing
  already shipped changes behavior unless opted in (the RFC 0007 discipline).

Non-goals are in §8.

## 2. Prior Art & The Convergent Pattern

The field has moved past "summarize history." A survey (full citations in §11):

- **Threshold-triggered abstractive summary** — OpenClaw's *Context Compaction*
  lifecycle and the Strands `ConversationManager` (sliding-window vs summarization)
  are representative. The common shape: estimate
  tokens, fire at a fraction of the window, condense the cold tail, keep recent turns
  verbatim, flush durable facts first. This is necessary but is the *fallback*, not the
  headline — abstractive summary has a quality cliff and is irreversible.
- **Extractive token-pruning** — LLMLingua / LLMLingua-2 (Microsoft): a small model
  scores token importance and drops low-information tokens, up to ~20x compression with
  minimal loss; LLMLingua-2 is a task-agnostic BERT-class token classifier distilled
  from GPT-4. Cheap, local, model-agnostic — a strong fit for tool-output and RAG prose.
- **Content-aware routing + reversibility** — Headroom: a `ContentRouter` detects content
  type and dispatches to specialized compressors (JSON, AST-aware code, prose), keeps a
  local cache of originals (CCR — "reversible compression"), and exposes a retrieve tool
  so the model can pull detail back on demand. Reports 60-95% token reduction on real
  agent workloads. Its three borrowable ideas: (a) type-routed compressors beat one-size
  summary, (b) reversibility removes the "what if the summary dropped the one thing I
  needed" fear, (c) compress at *ingest* (tool outputs, file reads, RAG) — that is where
  the tokens actually are, not only in chat history.
- **Optical compression** — DeepSeek-OCR ("Contexts Optical Compression"): render text to
  an image and encode it as vision tokens. At <10x compression, OCR decode precision is
  ~97%; at 20x, still ~60%. Critically, the authors propose *progressively downscaling
  older images* so token count falls and text blurs — "thereby accomplishing textual
  forgetting." That is an optical implementation of RFC 0007's exponential decay.
- **Learned soft-prompt compression** — Gist tokens, AutoCompressor, ICAE, 500xCompressor:
  compress context into a few learned embeddings. Powerful but require per-model training
  and are not portable across FlowForge's multi-provider design — surveyed, not adopted
  (§8).
- **OS-style memory paging** — MemGPT / Letta: treat the context window as RAM and an
  external store as disk; the agent self-manages what is resident via function calls.
  FlowForge's `memory_write` / `memory_search` + RFC 0007 decay already *is* a paging
  system; this RFC connects the in-context compactor to it.

**The white space:** no system cleanly fuses content-aware compression + reversibility +
a principled decay/forgetting model + a temporal memory timeline. FlowForge already owns
three of those four pillars (RFC 0006, RFC 0007, `consolidate.rs`). That is the
defensible angle this RFC commits to.

## 3. The Medium Question (verified)

The choice of *representation* for a compacted chunk is a first-class design axis. Three
candidate media were investigated; findings (sources in §11):

- **Natural-language transfer (e.g. compact in a denser human language).** Information
  *density* genuinely differs across languages — but speech *information rate* converges
  to ~39 bits/s across 17 languages (Coupe et al., *Science Advances* 2019): denser
  languages are produced more slowly. More decisively for us, the LLM unit is the
  **token**, and BPE tokenizers are English-biased: Arabic (2+ bytes/char) and Chinese
  (3+ bytes/char) typically **fragment into more tokens**, so the same content in a
  "denser" language often costs *more* tokens on current tokenizers, not fewer. **Verdict:
  rejected as a general lever** — the density win is real at the symbol level and erased
  at the token level. (German-style lexicalization of compound concepts — *Schadenfreude*,
  *Weltschmerz* — is a real but narrow trick: a one-word concept handle vs a sentence-long
  gloss. Filed as a micro-optimization, not a medium.)
- **Structured / symbolic.** Dropping prose ceremony in favor of structured deltas
  (key-paths for JSON, AST skeletons for code) is reliably token-cheap and lossless for
  the structure that matters. **Verdict: adopted** (Tier 1).
- **Optical.** Rendering cold text to a downscaled image (DeepSeek-OCR) is the most
  promising *dense* medium and uniquely aligns with decay. **Verdict: adopted as a
  research bet** (Tier 3), gated on a vision-capable model — which FlowForge now detects
  via `Provider::supports_vision()` (RFC for #338).

The principle: **leave natural language for a denser representation when the content
allows it, and let the decay clock pick how dense.**

## 4. Architecture: Tiered Strategies on the Existing Seam

The existing `ContextPressureEstimator` decides *when*. The host composes the tiers in
order, each relieving more pressure (and costing more) than the last. Tier 0 (flush) and
Tier 2 (abstractive) plug into the existing async, provider-driven `CompactionStrategy`
trait; **Tier 1 (extractive) is deliberately *not* a `CompactionStrategy`** — it is a
deterministic, synchronous pre-send wire transform applied to the request transcript
(see "Tier 1" below and §9 Q1 amendment). This avoids forcing a pure-CPU mechanism through
an async/Provider-shaped seam, while keeping `is_over(fraction)` the single trigger.

- **Tier 0 — Flush (shipped).** `MemoryFlush`: a silent bounded turn that persists durable
  facts to memory before anything is compressed away. Runs first so later lossy tiers can
  never destroy a fact that should have outlived the session. No change.
- **Tier 1 — Content-aware extractive compaction + reversible retrieve (new, primary
  ROI).** A `ContentRouter` classifies each cold message / tool output and dispatches:
  AST-trim for code, key-path prune for JSON/tool-output, LLMLingua-2-style token-prune
  for prose. Originals are cached keyed by message id (an FTS5 side table, reusing the
  RFC 0006 index machinery); a `compaction_retrieve` tool lets the model pull the original
  back on demand. Cheap, local, no quality cliff, reversible. **Wired (M7.1a + M7.1b)** as (a) a per-tool-result
  compaction at ingest, persisting `(message_id, key, original)` to a `compaction_originals`
  side table; and (b) a cold-prefix wire transform in `run_turn` gated by
  `is_over(EXTRACTIVE_COMPACT_AT_FRACTION)`, leaving the `KEEP_RECENT_VERBATIM` most
  recent messages byte-identical and skipping content already carrying the marker so
  ingest-time and pre-send passes never double-compact.
- **Tier 2 — Abstractive cold-tail summary (new, fallback).** When Tier 1 is exhausted,
  LLM-condense the oldest turns into a summary message, preserve the most recent N turns
  verbatim, and mark the boundary with the existing summary divider so the UI can render a
  "compacted" affordance. This is the conventional path, demoted to fallback.
- **Tier 3 — Decay-as-compaction (research bet).** Cold context is not compacted once and
  frozen; it is *progressively re-compressed as it ages*, driven by the same
  `0.5^(age/half_life)` clock as RFC 0007 memory decay. The compression *level* (and
  optionally the *medium* — prose -> structured -> downscaled optical -> dropped) is a
  function of age and access, mirroring DeepSeek-OCR's "resize older images to forget."
  Optical tiers gate on `supports_vision()`.

## 5. Consilience With Memory & the Temporal Timeline

This is the load-bearing design claim, not a nicety:

- **One decay clock.** RFC 0007 already defines lazy exponential decay (`weight`,
  `last_accessed`, half-life, `reinforce` / `ambient_gain`, dormancy). Tier 3 reuses *that
  function* to schedule re-compression of in-context chunks. A chunk that gets recalled
  (reinforced) is re-expanded; a chunk that idles is re-compressed — the same signal,
  applied to context instead of to ambient injection.
- **One temporal model.** `consolidate.rs` already ranks by recency x frequency over a
  dated timeline (daily-log chunks decay from their date; curated chunks held high). The
  compactor's "which turns are cold" question is the same recency question, answered by
  the same code.
- **One reversibility story.** RFC 0007 never deletes — dormancy is reversible via a single
  recall. Tier 1's CCR cache and `compaction_retrieve` give the in-context compactor the
  same guarantee: compaction is reversible, deletion is not on the table.
- **Two timescales, one mechanism.** On-disk memory is "what survives across sessions";
  context compaction is "what stays resident within a session." Unifying them means a fact
  flushed to memory (Tier 0), then compacted out of context (Tier 1-3), is still one
  recall away — `memory_search` and `compaction_retrieve` are the same paging gesture at
  different ranges.

## 6. Pressure Estimation (sharpening the trigger)

The `chars/4` `ProxyTokenEstimator` is intentionally coarse and is the documented seam
for per-model context windows. M7 plugs real per-model window metadata into the estimator
at the same place the `supports_vision` capability flag lands (per-connection model
metadata, RFC for #338) — so the *trigger* becomes accurate without touching the
strategies that consume it. The fire fraction and per-tier thresholds stay
user-inspectable, consistent with RFC 0007's "everything is inspectable" posture.

## 7. Non-Goals

- **No transcript deletion.** Like RFC 0007, the strongest action is compress-with-retrieve;
  originals are cached, never destroyed.
- **No learned/soft-prompt compression** (gist / ICAE / AutoCompressor / 500xCompressor) —
  requires per-model training and breaks multi-provider portability.
- **No natural-language medium transfer** — tokenizer bias defeats the density win (§3).
- **No KV-cache surgery in v1.** Cache-aligned prefixes (Headroom's CacheAligner) are
  acknowledged as valuable but deferred; v1 compacts message content, not provider caches.
- **No change to the Markdown memory source of truth.** As in RFC 0006/0007, compaction
  affects only what is *sent*, never the user's files.

## 8. Phasing

- **M7.0** — Land Tier 2 (abstractive cold-tail summary) on the existing seam, default-off.
  The smallest closure of the "we never actually compact" gap; gives a baseline.
- **M7.1** — Tier 1: `ContentRouter` + JSON/AST/prose compressors + CCR cache +
  `compaction_retrieve` tool. The primary token-ROI tier. Default-off, opt-in.
- **M7.2** — Real per-model context windows into `ProxyTokenEstimator` (§6); accurate
  trigger.
- **M7.3 (research)** — Tier 3 decay-as-compaction: wire the RFC 0007 decay clock to
  re-compression level; spike the optical medium behind `supports_vision()`.

## 9. Open Questions

1. **Tier ordering vs. cost.** Should Tier 1 (extractive) always precede Tier 2
   (abstractive), or should the router pick directly based on content type and pressure?
   *Amendment (M7.1b):* Tier 1 is implemented as a deterministic pre-send wire transform,
   not a `CompactionStrategy` impl. Tier 2 stays on the async/provider-driven strategy
   seam; ordering reduces to "extractive runs first because it is mechanical and free,
   abstractive is the fallback when extractive cannot relieve enough pressure."
2. **CCR cache lifetime.** How long are compaction originals retained, and do they share
   the FTS5 index / decay with memory, or live in a session-scoped side store?
3. **Optical decode trust.** At 20x optical compression DeepSeek-OCR is ~60% accurate —
   acceptable only for the coldest, lowest-stakes tail. What is the age threshold past
   which lossy-optical is safe, and how does the model signal it needs a `retrieve`?
4. **Reinforcement coupling.** When a `compaction_retrieve` fires, should it reinforce the
   underlying memory chunk (RFC 0007 `reinforce`) — i.e. is a context recall also a memory
   recall? Likely yes; needs the same SNR caution as RFC 0007 §10 Q#1.
5. **Eval harness.** Compaction quality must be measured by task outcome, not compression
   ratio. What golden set proves "same answers, fewer tokens" (cf. Headroom's proof
   workloads)?

## 10. References

Listed to credit the work that informed this design.

1. **DeepSeek-AI.** "DeepSeek-OCR: Contexts Optical Compression." arXiv:2510.18234, Oct 2025.
   https://arxiv.org/abs/2510.18234 · code: https://github.com/deepseek-ai/DeepSeek-OCR
   — optical compression; progressive-downscale "textual forgetting"; the decay/medium fusion.
2. **Headroom (Headroom Labs).** "The context compression layer for AI agents."
   https://github.com/headroomlabs-ai/headroom
   — content-aware routing (SmartCrusher/CodeCompressor/Kompress), reversible CCR,
   compress-at-ingest, CacheAligner. The core inspiration for Tier 1.
3. **Jiang, H., Wu, Q., Lin, C.-Y., Yang, Y., Qiu, L.** "LLMLingua: Compressing Prompts for
   Accelerated Inference of Large Language Models." EMNLP 2023. arXiv:2310.05736 ·
   https://github.com/microsoft/LLMLingua — extractive token-pruning, up to ~20x.
4. **Pan, Z., et al. (Microsoft).** "LLMLingua-2: Data Distillation for Efficient and
   Faithful Task-Agnostic Prompt Compression." ACL 2024 Findings. arXiv:2403.12968
   — the task-agnostic BERT-class compressor proposed for Tier 1 prose.
5. **Coupe, C., Oh, Y., Dediu, D., Pellegrino, F.** "Different languages, similar encoding
   efficiency: Comparable information rates across the human communicative niche."
   *Science Advances* 5(9):eaaw2594, 2019. https://www.science.org/doi/10.1126/sciadv.aaw2594
   — the ~39 bits/s cross-language information-rate result behind the §3 medium verdict.
6. **Packer, C., et al.** "MemGPT: Towards LLMs as Operating Systems." arXiv:2310.08560,
   2023. (Project: Letta.) — OS-style memory paging; the RAM/disk framing in §2/§5.
7. **Mu, J., Li, X. L., Goodman, N.** "Learning to Compress Prompts with Gist Tokens."
   NeurIPS 2023. arXiv:2304.08467 — learned soft-prompt compression (surveyed, §8).
8. **Chevalier, A., Wettig, A., Ajith, A., Chen, D.** "Adapting Language Models to Compress
   Contexts" (AutoCompressor). EMNLP 2023. arXiv:2305.14788 — surveyed, §8.
9. **Ge, T., et al.** "In-context Autoencoder for Context Compression in a Large Language
   Model" (ICAE). arXiv:2307.06945, 2023 — surveyed, §8.
10. **Li, Y., et al.** "Compressing Context to Enhance Inference Efficiency of Large Language
    Models" (Selective Context). EMNLP 2023. arXiv:2310.06201 — extractive baseline.
11. **Strands Agents.** `ConversationManager` (sliding-window and summarization context
    strategies). Amazon agent framework documentation — the threshold-triggered summary
    baseline in §2.
12. **OpenClaw.** Context & Context Compaction documentation. https://docs.openclaw.ai
    — `contextTokens` cap, threshold-triggered summarize-and-replace, memory flush.
> Note: items 11-12 are framework/product references used as design influences; 1-10 are
> primary sources. Where a result is quoted (compression ratios, bits/s), the number is
> taken from the cited source and should be re-verified before it is used as an engineering
> constant.
