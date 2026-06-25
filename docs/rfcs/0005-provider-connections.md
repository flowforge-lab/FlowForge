# 0005 — Provider Connections & Registry

- **Status:** Phases A-B landed; Phases C-D proposed (this revision)
- **Revision (2026-06-25):** Added three-tier model selection (§11) — per-session / per-phenotype / global model resolution, with capabilities derived from `(kind, model)`. Promotes Phase C from a one-line reservation to a full design and adds Phase D (UX + capability derivation).
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

/// A resolved (connection, model) pair — the unit of model selection at every
/// tier (session / phenotype / global). `connection` picks the endpoint+creds;
/// `model` is one of that connection's served models. Capabilities (e.g. vision)
/// are DERIVED from `(kind, model)` via ff-core model_specs, never stored here.
struct ModelSelection {
    connection: ConnectionId,
    model: String,
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

> Phase D extends this section with a per-connection **Model** picker and surfaces the
> resolved **(provider, model)** pair; see §11.4.

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
- **Phase C (backend, this revision):** add `Phenotype.provider: Option<ConnectionId>`
  beside the existing `model: Option<String>` override, and a three-tier resolver
  (session > phenotype > global) that produces a `ModelSelection` and routes the turn
  through `build_provider_for(connection)`. Fixes the latent cross-endpoint bug (§11.1).
- **Phase D (UX + capabilities, this revision):** `set_session_model_selection` + a
  per-pane model chip, Provider/Model rows in the phenotype editor, and capability
  derivation from `(kind, model)` — retiring the per-connection `supports_vision`
  field (§11.3 / §11.4).

## 10. Non-Goals

Keychain/secrets, hosted vendors, and Bedrock SigV4 were out of Phase A and landed in
Phase B. Phenotype-provider binding and per-session/per-phenotype model selection,
formerly deferred, are the subject of Phases C-D (§11). Still out of scope: multiple
*simultaneously active* connections (resolution always collapses to one `ModelSelection`
per turn), automatic model routing/failover, and a model marketplace/catalog beyond the
per-connection `list_models` already specified.

## 11. Three-Tier Model Selection (Phases C-D)

### 11.1 The bug Phase C fixes

A phenotype's `model` override is currently a bare string applied to whatever the
**global active connection** is. In `spawn_assistant_turn` the turn builds the active
provider's client and then swaps in `pheno.model` — so an override model can ride the
wrong endpoint (e.g. a SiliconFlow-only model name sent to a candle-vLLM client). The
override carries a model but no provider, so there is nothing to route it to its
intended connection.

### 11.2 The resolver

Model selection becomes a `ModelSelection { connection, model }` resolved at three
tiers, most specific wins:

```
session override   (set_session_model_selection, per pane)        -- highest
  else phenotype    (Phenotype.provider + Phenotype.model)
    else global      (registry.active + active connection's model) -- lowest
```

Each tier may specify a connection, a model, or both; unspecified fields inherit from
the next tier down. The resolved pair routes the turn through
`build_provider_for(connection)` (already present, state.rs) -> the override model can
never ride the wrong endpoint. This mirrors the autonomy-mode precedent (#265):
`get/set_default_*` (global) + `set_session_*` (per pane) + inherit-when-None.

### 11.3 Capabilities are derived, not stored

`supports_vision` is today a per-connection stored flag. Since #466 introduced a
data-driven `model_supports_vision(kind, model)` lookup in ff-core model_specs,
capability is a pure function of the resolved `(kind, model)`. Phase D derives it at
resolution time and **retires the stored `supports_vision` field**, removing a class of
stale-flag bugs (flag set on the connection but wrong for the chosen model).

### 11.4 UX (Phase D)

- A **model chip** on each pane shows the resolved model and opens a quick picker
  (connection -> model) for a **session-scoped** `ModelSelection`; clearing it falls
  back to phenotype/global.
- The phenotype editor gains **Provider** and **Model** rows (Provider = a connection
  combobox; Model = the connection's `list_models`, editable) writing `Phenotype.provider`
  / `Phenotype.model`.
- §6's Model section continues to own the **global** active connection + its model.

### 11.5 Migration

Additive and lossless. `Phenotype.provider: Option<ConnectionId>` is added beside the
unchanged `model: Option<String>`; existing phenotypes (provider = None) resolve exactly
as today via the global active connection. No registry or phenotype file rewrite is
required; the new field defaults to `None` on deserialize.
