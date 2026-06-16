# 0005 — Provider Connections & Registry

- **Status:** Proposed
- **Milestone:** Settings UI (SET epic #125) · cashes in #8
- **Author:** tonytan4ever
- **Depends on:** RFC 0001 (phenotype = `{skills, tools, model, persona}`; per-session switch), #49 (landed Phase-1 `ProviderConfig` contract)
- **Tracking issue:** #8 (umbrella) · #126 (SET.3 Model section, FE)

## 1. Summary & Goals

#8 / #49 gave FlowForge a **single, runtime-switchable** LLM provider: one global
`ProviderConfig` persisted to `provider.json`, rebuilt per turn. That unblocked
candle-vLLM <-> Ollama switching at the contract level but leaves three gaps:

1. **Switching is destructive.** The config is a singleton, so moving candle-vLLM ->
   Ollama overwrites candle-vLLM's `baseUrl`/`model`; toggling back means re-entering
   them. Ergonomic switching needs *both* configs persisted.
2. **No vendor identity.** The market has 10+ providers (OpenAI, OpenRouter,
   SiliconFlow, Groq, Together, ...). Most speak **one** protocol (OpenAI-compatible),
   but users must still see and pick them as *distinct* vendors — distinct logos,
   docs, and (later) per-phenotype bindings. A singleton with a 2-variant enum cannot
   express this.
3. **No path to per-phenotype providers.** RFC 0001 lets a phenotype override the
   *model* but not the *provider*. Binding a phenotype to a provider requires
   providers to be **named, addressable entities**.

This RFC introduces a **three-layer provider model** and a **connection registry**
that closes all three while keeping the backend collapsed on a small set of
protocol implementations.

The guiding goal: **adding a new vendor should be a one-row data edit with zero Rust**,
and **switching providers should never lose a configuration**.

Non-goals are listed in section 10.

## 2. The Three-Layer Model

The prior design conflated *protocol* with *identity*. We split them into three layers:

| Layer | What it is | Lives | Growth |
|-------|-----------|-------|--------|
| **`ProviderKind`** | Protocol / transport — selects the `ff-llm` impl and the field schema | `ff-core` enum + `ff-llm` | rarely (~4 total) |
| **Vendor descriptor** | A catalog entry — the user-facing *identity* (label, logo, base URL, field set) | FE data (`lib/provider-catalog.ts`) | freely, data-only |
| **Provider connection** | A user's configured, named, persisted instance | registry (`provider-registry.json`) | per user |

- One **kind** (`openAiCompatible`) backs many **descriptors** (OpenAI, OpenRouter,
  SiliconFlow...) -> no duplicate Rust.
- Each **descriptor** is a distinct catalog row -> distinct logo/spot/docs, and is
  independently selectable in the autocomplete.
- Each **connection** has a stable `id` -> bindable to a phenotype (Phase C), and two
  connections may share a kind/vendor (e.g. two OpenRouter keys).

## 3. Data Model (`ff-core`)

```rust
/// Protocol/transport. Picks the `ff-llm` Provider impl and the connection's field
/// schema. Small and slow-growing — vendors are NOT enum variants.
enum ProviderKind {
    CandleVllm,                 // local, OpenAI-compatible, no key
    Ollama,                     // local, native NDJSON
    // Phase B: OpenAiCompatible (hosted, keyed) ; Phase C+: Bedrock, Anthropic
}

type ConnectionId = String;     // stable slug: "candle-vllm", "ollama", "openrouter-1"

/// A user's configured provider instance. No secret material — keys live in the OS
/// keychain (Phase B) and surface only as `has_key`.
struct ProviderConnection {
    id: ConnectionId,
    kind: ProviderKind,
    display_name: String,       // "candle-vLLM (local)", "OpenRouter"
    vendor: Option<String>,     // catalog descriptor id; None = fully custom endpoint
    base_url: Option<String>,   // None = ProviderKind::default_base_url
    model: String,
    has_key: bool,              // keychain presence; always false until Phase B
}

/// The persisted set of connections plus which one is active.
struct ProviderRegistry {
    connections: Vec<ProviderConnection>,   // >= 1
    active: ConnectionId,
}
```

All four types derive `Serialize`/`Deserialize`/`TS` and export to
`apps/desktop/src/bindings/`. `ProviderConfig` (the singleton) is retained as a
**read-through view of the active connection** during migration (section 7) and removed
once the FE fully consumes the registry.

## 4. Vendor Catalog (Layer 2, FE-first)

The descriptor catalog is **frontend data** — pure, serializable rows. It is the long,
ever-growing list, surfaced via autocomplete (section 6). It moves into `ff-core` only if
the backend ever needs vendor knowledge (it does not today).

```ts
interface ProviderField {
  key: "baseUrl" | "model" | "apiKey" | string;
  label: string;
  required: boolean;
  secret?: boolean;            // routed to keychain, never persisted in the registry
  placeholder?: string;
}

interface VendorDescriptor {
  id: string;                  // "candle-vllm", "ollama", "openrouter"
  label: string;
  kind: ProviderKind;
  baseUrl?: string;            // prefilled default for this vendor
  needsKey: boolean;
  logo: string;                // key into the ProviderLogo seam (section 5)
  docsUrl?: string;
  group: "local" | "hosted";
  fields: ProviderField[];     // drives the data-driven connection form
}
```

Phase A seeds only the **keyless** rows (`candle-vllm`, `ollama`). Hosted/keyed rows
ship hidden and unhide in Phase B.

## 5. Logos

Each vendor has a brand logo via **`@lobehub/icons`** (MIT) — a maintained icon set
purpose-built for AI providers (mono + color variants, dark/light aware). The
descriptor's `logo` is a **string key**, not a component, so the catalog stays pure
data. A single `components/provider-logo.tsx` seam maps `logo` key -> icon component,
applies size and variant, and renders a **monogram fallback** (display-name initial)
for unknown keys or fully-custom connections.

Convention: **mono variant in lists/combobox rows, color variant on the selected
detail card** — consistent with the monochrome settings UI.

## 6. Frontend (Phase A — SET.3 / #126)

- **`components/settings/combobox.tsx`** — reusable filterable, free-text-allowed,
  keyboard-navigable picker. Used for BOTH the vendor picker and the **model field**
  (model is an *editable* combobox: free text + `list_models` suggestions, never a
  locked dropdown — candle-vLLM serves whatever single model is loaded).
- **`lib/provider-catalog.ts`** — the descriptors (section 4).
- **`store/provider-registry.ts`** — connections + active + actions; mirrors
  `store/search-config.ts`.
- **`settings/model-section.tsx`** — **Active provider** combobox over configured
  connections + an **Add provider** affordance (catalog autocomplete -> data-driven
  field form) + #126's Thinking / Effort / Threshold controls + reset.

## 7. Commands & Migration

```
get_provider_registry() -> ProviderRegistry
set_active_connection(id)                       // the candle-vLLM <-> Ollama switch
upsert_connection(conn) -> ProviderConnection   // add or edit; returns resolved id
remove_connection(id)                           // cannot remove the last connection
list_models(id?) -> string[]                    // per-connection, best-effort
// Phase B:
set_connection_secret(id, secret)               // write-only, OS keychain
```

`build_provider` becomes `build_provider(active_connection)` — the same per-turn
mechanism and the same `kind` match; no architectural change.

**Migration (one-time, lossless):** on load, if `provider.json` exists and
`provider-registry.json` does not, wrap the singleton as one connection, **seed the
second keyless vendor** (so switching works out of the box), set `active` to the
migrated one, and write the registry file. `get`/`set_provider_config` remain as
active-connection shims until the FE cuts over.

## 8. Mock Parity

`mock.ts` implements the new commands with an in-memory registry seeded with
candle-vLLM + Ollama, per-connection canned model lists, and stateful
add/edit/remove/switch — so the Model section is fully buildable under
`VITE_FF_MOCK=1` before any Rust lands. Tests mirror `mock.search.test.ts`.

## 9. Phasing

- **Phase A (now):** registry + active pointer + Combobox + catalog + data-driven form
  + logos, for the two **keyless** vendors. Ergonomic candle-vLLM <-> Ollama switching.
  No keychain.
- **Phase B (#8 Phase 2):** OS-keychain secrets + `set_connection_secret`; add the
  `OpenAiCompatible` kind and unhide hosted/keyed descriptors (OpenAI, OpenRouter,
  SiliconFlow...). Catalog + form already support them.
- **Phase C:** `Phenotype.provider: Option<ConnectionId>` beside the existing `model`
  override; per-turn resolution = phenotype's connection else `registry.active`.

## 10. Non-Goals

Keychain/secrets, hosted vendors, Bedrock SigV4, phenotype-provider binding, and
multiple connections of the same keyed vendor are explicitly out of Phase A. The data
model reserves the slots for them so the contract does not churn twice.
