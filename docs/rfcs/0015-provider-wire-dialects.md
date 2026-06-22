# 0015 — Provider Wire Dialects

- **Status:** Proposed
- **Milestone:** _Provider fidelity (follows RFC 0005)_
- **Author:** tonytan4ever
- **Depends on:** RFC 0005 (provider connections & registry); PR #376 (#375 PR-1 — persisted assistant reasoning). Sibling to #374 (GLM tool-calling).
- **Tracking issue:** #375

## 1. Summary & Goals

OpenAI-compatible gateways agree on the *request shape* but diverge in small, model-
and vendor-specific ways at the wire edge — most visibly around **reasoning** and
**tool-calling**. FlowForge reaches all of them through one `OpenAiProvider`, so those
quirks have nowhere to live except scattered `if vendor == …` branches.

This RFC introduces **`WireDialect`** — a single, typed, per-connection value that
encodes how a provider deviates at the wire edge, selected in one pure function and
applied in one place (`message_to_wire`). Its first two users:

1. **Reasoning round-trip** (#375 core) — resend the assistant's chain-of-thought on
   later tool-calling turns, under the *correct field name* for the gateway.
2. **Empty tool-call content representation** (the tool-call quirk) — emit `""` vs
   `null` vs omit for an assistant message that only carries tool calls.

Goal: **adding the next provider quirk is one match arm + one apply-branch**, not a
new conditional scattered across the call path. Non-goals in §8.

## 2. Background — what exists

- **The capability pattern is already proven.** `supports_vision` is a per-connection
  `bool` (`ff-core::ProviderConnection`) threaded by `build_provider` into
  `OpenAiProvider::with_vision`, then applied inside `chat_stream` by
  `messages_for_wire(messages, supports_vision)` (`ff-llm/src/lib.rs`). Attachments are
  reshaped per-adapter (Bedrock native blocks vs OpenAI data-URI). `WireDialect` is the
  same shape, for a richer signal than a bool.
- **Output reasoning is already multi-field.** `openai.rs` merges
  `reasoning_content` *or* `reasoning` on the way in
  (`reasoning_delta = c.reasoning_content.or(c.delta.reasoning)`). The capture side is
  done; this RFC is the **input / resend** side.
- **PR-1 (#376) persists it.** `Message.reasoning: Option<String>` now survives to the
  next turn; `to_chat` still drops it on the way to the wire. That drop is what PR-2
  closes.

## 3. The wire reality (empirically verified)

Probed live against `api.siliconflow.com/v1` (2026-06):

**Reasoning field name diverges by lineage:**

| Input field | Type | Providers |
|---|---|---|
| `reasoning_content` | string | DeepSeek-direct, **SiliconFlow** (incl. its GLM/MiniMax hosting), Together, vLLM |
| `reasoning` | string | OpenRouter, Cerebras |
| `reasoning_details[]` | structured | Anthropic/Gemini *via* OpenRouter — **deferred** (§8) |
| `<think>…</think>` inline | n/a | raw GLM/MiniMax direct — **not seen via SiliconFlow** (it normalizes to `reasoning_content`); belongs to #374 if a direct-GLM endpoint is added |

> Note: GLM-5.2 *via SiliconFlow* returns reasoning in the separate `reasoning_content`
> field, **not** inline `<think>`. A `<think>`-strip quirk would be folklore on this
> gateway, so it is explicitly **not** implemented here.

**Resending reasoning is a reliability requirement, not just quality:**

`deepseek-ai/DeepSeek-V4-Pro` in thinking mode returns an **intermittent HTTP 400**
(`code 20015: "The reasoning_content in the thinking mode must be passed back to the
API."`) when the assistant's empty-content tool-call message is resent *without*
`reasoning_content`. Resending it is deterministically accepted. This upgrades #375
from "quality drift" to a **correctness/reliability fix** for DeepSeek thinking mode.

**Empty tool-call content representation diverges (the tool-call quirk):**

For an assistant message that carries *only* tool calls (no text):

| Representation | `zai-org/GLM-5.2` | `deepseek-ai/DeepSeek-V4-Pro` |
|---|---|---|
| `content: ""` | ✅ 200 | ✅ 200 |
| `content: null` | ❌ 400 (`20015 invalid parameter`) | ✅ 200 |
| omit `content` | ✅ 200 | ✅ 200 |

FlowForge's `to_chat` sets `content: None`, which serializes to *omitted* (the wire
`ChatMessage.content` is `skip_serializing_if = "Option::is_none"`), so it is
**incidentally safe** today. PR-2 makes the choice an **explicit, tested per-dialect
policy** so a future refactor that emits a spec-literal `null` cannot silently break
GLM.

## 4. Design — `WireDialect`

Internal to `ff-llm` (not an IPC/ts-rs type — no FE/settings surface):

```rust
pub enum ReasoningWire {
    None,             // do not resend CoT (vanilla OpenAI -> avoids 400; default)
    ReasoningContent, // DeepSeek lineage, SiliconFlow (incl. its GLM hosting)
    Reasoning,        // OpenRouter / Cerebras lineage
}

pub enum ToolCallContent {
    Omit,        // assistant tool-call message: omit empty content (current, universal-safe; default)
    EmptyString, // emit "" instead (GLM-lineage rejects null; "" is its accepted form)
}

pub struct WireDialect {
    pub reasoning: ReasoningWire,
    pub tool_call_content: ToolCallContent,
}
```

**Selection — one pure function (single source of truth):**

```rust
pub fn wire_dialect(kind: ProviderKind, vendor: Option<&str>, model: &str) -> WireDialect
```

| Connection | `reasoning` | `tool_call_content` |
|---|---|---|
| `SiliconFlow` | `ReasoningContent` | `EmptyString` if model is GLM/MiniMax, else `Omit` |
| `OpenAi` + vendor `openrouter` | `Reasoning` | `Omit` |
| `OpenAi` (vanilla) | `None` | `Omit` |
| `CandleVllm` / `Ollama` / `Bedrock` | `None` | `Omit` |

Defaults are **no-ops** for every connection that ships today — no behavior change
unless a dialect opts in.

**Carrier — the `#[serde(skip)]` rule (important):**

`openai.rs::message_to_wire` does `serde_json::to_value(msg)` then augments. A wire
`ChatMessage` field that serializes would therefore **auto-leak** to every
OpenAI-compatible call (wrong key for DeepSeek; a 400 risk for vanilla OpenAI). So the
carrier field is **`#[serde(skip)]`** and *every* emission is a deliberate, per-adapter
injection:

```rust
pub struct ChatMessage {
    // …existing…
    #[serde(skip)]
    pub reasoning: Option<String>, // populated by to_chat; injected, never auto-serialized
}
```

**Apply — in `message_to_wire(msg, dialect)`:**

- **Reasoning:** inject the dialect's key **only** when
  `msg.role == "assistant" && msg.tool_calls.is_some() && msg.reasoning.is_some()`
  (DeepSeek's documented rule: resend CoT on tool-call turns, omit otherwise). `None`
  dialect injects nothing.
- **Tool-call content:** when the message is an assistant-only-tool-call (no text) and
  the dialect is `EmptyString`, emit `"content": ""` instead of omitting.

Anthropic / Bedrock / Ollama keep their own mappers and an effective dialect of
`None`/`Omit`; the `#[serde(skip)]` carrier guarantees nothing leaks through their
serde paths.

## 5. Data flow

```
Message.reasoning (persisted, PR-1)
   └─ to_chat ─────────────────► ChatMessage.reasoning  (#[serde(skip)] carrier)
ProviderConnection {kind,vendor,model}
   └─ build_provider ─► wire_dialect(...) ─► OpenAiProvider::with_dialect(d)
                                                   │
                                 chat_stream ─► message_to_wire(msg, self.dialect)
                                                   ├─ reasoning: inject correct key, conditionally
                                                   └─ tool_call_content: "" vs omit
```

## 6. Extensibility — how the next quirk lands

- **New OpenAI-compatible gateway** → one arm in `wire_dialect`.
- **`reasoning_details[]`** (structured thought signatures) → add a `ReasoningWire`
  variant + one injection branch. Carrier already exists.
- **Inline `<think>` for a *direct* GLM endpoint** (#374) → add a `ToolCallContent` /
  reasoning-shaping variant; same apply site. PR-2 deliberately does **not** add it
  because no shipping connection exhibits it.
- **Other tool-call wire quirks** (parallel-tool-calls toggle, `tool_choice` policy) →
  new field on `WireDialect`, selected in `wire_dialect`, applied in `message_to_wire`.

The invariant: **one selector function, one apply function, per-adapter injection.**

## 7. Testing

- `wire_dialect` mapping table (all kinds × vendor × GLM/non-GLM model) — pure unit test.
- `message_to_wire`:
  - injects `reasoning_content` / `reasoning` per dialect, only on assistant+tool_call+reasoning;
  - omits reasoning on a no-tool-call assistant turn and for `None`;
  - emits `content: ""` for `EmptyString`, omits otherwise;
  - **serde no-leak**: a `ChatMessage` with `reasoning: Some(..)` serializes with no
    `reasoning*` key via the plain serde path (anthropic/bedrock/ollama).
- `to_chat` carries `Message.reasoning` onto the wire message (assistant only).
- `wiremock` round-trip: assert the outgoing body for a SiliconFlow-style
  (`reasoning_content`) vs OpenRouter-style (`reasoning`) dialect.

## 8. Non-Goals

- FE display of persisted reasoning on reload (separate ticket).
- Per-provider reasoning-effort params (`reasoning_effort`, `thinking.type`).
- Structured `reasoning_details[]` round-trip (carrier ready; deferred).
- Inline `<think>` parsing/stripping for a direct-GLM endpoint — #374's domain; not
  exhibited by any shipping connection (SiliconFlow normalizes to `reasoning_content`).
- Making `WireDialect` user-overridable in settings (kept internal; revisit only if a
  real connection needs a manual override).
