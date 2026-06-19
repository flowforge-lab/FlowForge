# 0009 — AWS Bedrock Provider & Multi-Mode Auth

- **Status:** Proposed
- **Milestone:** Settings UI (SET epic #125) · cashes in #8 (Bedrock slot reserved in RFC 0005 §10)
- **Author:** tonytan4ever
- **Depends on:** RFC 0005 (three-layer provider model: `ProviderKind` / vendor catalog / `ProviderRegistry`; `list_models`; keychain secrets in Phase B). RFC 0001 (phenotype provider binding, Phase C).
- **Tracking issue:** #8 (umbrella provider epic)

## 1. Summary & Goals

RFC 0005 gave FlowForge a three-layer provider model and reserved a slot for hosted,
keyed vendors. Its §10 explicitly deferred **Bedrock SigV4**. This RFC fills that slot.

The motivation is concrete: to let FlowForge stand in for Claude Desktop, users need
**Opus / Sonnet**, and the most direct path is **AWS Bedrock**. Bedrock does not fit the
`baseUrl + optional apiKey` shape every other kind shares. It needs:

1. A **SigV4 / bearer** transport with a **region-derived endpoint** (no `baseUrl`).
2. **Three auth modes** — local AWS **profile**, **IAM access keys**, or a **Bedrock API
   key** — which RFC 0005's single `has_key: bool` cannot describe (profile mode stores
   *no* secret at all).
3. A **region** as a first-class, non-secret connection field, with model listing that is
   **region-scoped** (cross-region inference profiles).

Goals:
- Add Bedrock as a new `ProviderKind` with **zero churn** to the existing keyless/keyed
  kinds, reusing RFC 0005's catalog + data-driven form + registry + `list_models`.
- Generalize the secret model from a single `has_key` boolean to an **auth descriptor**
  that the data-driven form already knows how to render conditionally.
- Specify the **model-selection UI flows** RFC 0005 left at contract level: catalog
  autocomplete for *adding*, a grouped list for *managing/switching*, and a Bedrock
  auth sub-form.

Non-goals are in §9.

> **Dependency note.** RFC 0005's Phase-A FE (the connections combobox + catalog +
> data-driven add form, §6) is **not yet built** — the shipped `model-section.tsx`
> (#144) is still a 2-kind `SegmentedControl` over the `get/setProviderConfig` shims.
> This RFC assumes that registry-consuming FE lands first (or alongside); Bedrock plugs
> into it rather than replacing it.

## 2. The three auth shapes

Every provider collapses to one of three **auth shapes**. The shape — not the vendor or
the kind — decides what the connection form collects and what (if anything) reaches the
keychain.

| Shape | Providers | Form collects | Keychain secret |
|-------|-----------|---------------|-----------------|
| **A — none** | candle-vLLM, Ollama, llama.cpp, LM Studio (local) | host (optional) | none |
| **B — key + host** | OpenAI, Groq, OpenRouter, DeepSeek (hosted OAI-compat) | host (fixed or editable) + API key | api key |
| **C — Bedrock** | AWS Bedrock | region + one of {profile} / {IAM keys} / {bedrock api key} | mode-dependent (none for profile) |

"OpenAI-compatible" is a **wire protocol**, not an auth class: a *local* OAI-compat
endpoint is shape A, a *hosted* one is shape B. Auth is orthogonal to transport.

## 3. Data model (`ff-core`)

RFC 0005's `ProviderConnection` is extended **additively** with two non-secret fields and
an auth descriptor. `has_key` becomes **derived** (kept for FE compatibility).

```rust
/// Protocol/transport. RFC 0005 reserved the Bedrock slot here.
enum ProviderKind {
    CandleVllm,        // local OAI-compatible SSE
    Ollama,            // local native NDJSON
    OpenAiCompatible,  // hosted OAI-compatible (RFC 0005 Phase B)
    Bedrock,           // NEW: AWS Bedrock, SigV4/bearer, region-derived endpoint
}

/// How a connection authenticates. Supersedes the bare `has_key` boolean; `has_key`
/// is now derived (= mode needs a secret AND the keychain has one).
enum AuthMode {
    None,            // shape A — local, no creds
    ApiKey,          // shape B — secret = api key
    BedrockProfile,  // shape C — secret = NONE; uses `profile_name` + default chain
    BedrockIamKeys,  // shape C — secret = secret access key (+ session token)
    BedrockApiKey,   // shape C — secret = bearer token
}

struct ProviderConnection {
    id: ConnectionId,
    kind: ProviderKind,
    display_name: String,
    vendor: Option<String>,        // catalog descriptor id
    base_url: Option<String>,      // unused for Bedrock (endpoint is region-derived)
    model: String,
    thinking: bool,                // shipped field (#181)
    // --- NEW ---
    auth_mode: AuthMode,           // see migration below
    region: Option<String>,        // first-class; required for non-profile Bedrock (§5)
    profile_name: Option<String>,  // non-secret; only for BedrockProfile
    // has_key is now a derived getter, not a stored field
}
```

**What is and isn't a secret (registry vs keychain):**

| Stored in registry JSON (non-secret) | Stored in OS keychain (Phase B, write-only) |
|--------------------------------------|---------------------------------------------|
| `auth_mode`, `region`, `profile_name`, `base_url`, `model`, identity | api key; Bedrock bearer token; IAM bundle `{access_key_id, secret_access_key, session_token?}` as one blob |

The IAM `access_key_id` is bundled with the secret (the pair is sensitive together) — the
registry never holds any part of it.

**Migration (additive, lossless).** Connections persisted before this RFC have no
`auth_mode`/`region`. On load, infer: `auth_mode = None` for local kinds, `ApiKey` if the
legacy `has_key` was true; `region = None`. No file rewrite until the next mutation
(matches RFC 0005 §7 lazy-persistence). `#[serde(default)]` on the new fields keeps old
files deserializable.

## 4. Vendor catalog & the data-driven form (extends RFC 0005 §4/§6)

RFC 0005's `VendorDescriptor.fields: ProviderField[]` already drives a data-driven form.
Bedrock needs **conditional** fields (the auth-mode branch), so `ProviderField` gains a
**type** tag, a **conditional-visibility** predicate, and **conditional-required**:

```ts
type FieldType = "text" | "secret" | "model" | "region" | "authMode";

interface ProviderField {
  key: string;
  label: string;
  type: FieldType;
  required: boolean | { whenAuthMode: AuthMode };  // conditional
  secret?: boolean;                                  // routes to keychain
  placeholder?: string;
  options?: string[];                                // region / authMode choices
  showWhen?: { authMode: AuthMode[] };               // conditional visibility
}
```

The Bedrock descriptor row (catalog data, no Rust):

```ts
{
  id: "bedrock", label: "AWS Bedrock", kind: "bedrock",
  needsKey: true, logo: "bedrock", group: "hosted",
  fields: [
    { key: "region", label: "Region", type: "region", required: true,
      options: ["us-east-1","us-west-2","eu-central-1","ap-northeast-1"] /* + free text */ },
    { key: "authMode", label: "Authentication", type: "authMode", required: true,
      options: ["bedrockProfile","bedrockIamKeys","bedrockApiKey"] },
    { key: "profileName", label: "AWS profile", type: "text",
      required: { whenAuthMode: "bedrockProfile" },
      showWhen: { authMode: ["bedrockProfile"] }, placeholder: "default" },
    { key: "accessKeyId", label: "Access key ID", type: "text",
      required: { whenAuthMode: "bedrockIamKeys" }, showWhen: { authMode: ["bedrockIamKeys"] } },
    { key: "secretAccessKey", label: "Secret access key", type: "secret", secret: true,
      required: { whenAuthMode: "bedrockIamKeys" }, showWhen: { authMode: ["bedrockIamKeys"] } },
    { key: "sessionToken", label: "Session token (optional)", type: "secret", secret: true,
      showWhen: { authMode: ["bedrockIamKeys"] } },
    { key: "bedrockApiKey", label: "Bedrock API key", type: "secret", secret: true,
      required: { whenAuthMode: "bedrockApiKey" }, showWhen: { authMode: ["bedrockApiKey"] } },
    { key: "model", label: "Model", type: "model", required: true },
  ],
}
```

The `authMode` field renders as RFC 0005's reusable control set — concretely the existing
`components/settings/segmented-control.tsx` for the 3-way mode toggle — and swaps the
visible field group **inline** (no multi-page wizard).

## 5. Region (first-class)

Bedrock has no `baseUrl`; the endpoint is `bedrock-runtime.<region>.amazonaws.com`, so
**region is the Bedrock analogue of host** and is required across all three modes —
but with different fallback semantics:

| Auth mode | Region required? | Blank-region fallback |
|-----------|------------------|------------------------|
| BedrockProfile | optional | profile's `region` in `~/.aws/config` -> `AWS_REGION` -> `AWS_DEFAULT_REGION` -> error |
| BedrockIamKeys | **required** | none (keys carry no region) |
| BedrockApiKey  | **required** | none (bearer carries no region) |

**Region <-> inference-profile coupling.** Cross-region inference-profile IDs are region-
*family* scoped: `us.anthropic.claude-...` only invokes from a `us-*` region, `eu.` from
`eu-*`, `apac.` from `ap-*`. Therefore:

- the model list (§6) is fetched **after** region is set and is **region-dependent** —
  changing region invalidates the cached list;
- the form warns when a chosen profile-id prefix and the region family disagree.

UI: a **combobox of common Bedrock regions with free text** (Bedrock availability is
non-uniform and shifts), never a closed `<select>`.

## 6. Model listing (extends RFC 0005 `list_models`)

RFC 0005's editable model combobox (free text + `list_models` suggestions, never locked)
carries over. Two Bedrock-specific corrections plus one contract gap:

- **Source = `ListInferenceProfiles` + on-demand `ListFoundationModels`,** not
  `ListFoundationModels` alone. Opus/Sonnet are typically invocable only via a
  cross-region **inference-profile ID** (`us.anthropic.claude-...`), not the base model id.
- **Listing != access.** Models must be enabled in the account; an access error surfaces a
  clear "enable model access in the AWS console" hint rather than an empty list.
- **Always keep the free-text escape hatch** (the shipped "include current model even if
  unlisted" behaviour generalizes to an editable combobox).

**Contract gap — probing before save.** `list_models(id?)` takes a *persisted* connection
id, but the add-flow must probe **before** committing a half-built connection, and the
probe needs region + resolved creds. Resolution (the chosen lean):

- **Draft-then-probe.** `upsert_connection` accepts a connection flagged `draft: true`;
  the FE writes any secret via `set_connection_secret(draftId, ...)` (Phase B), calls a
  new **`test_connection(id) -> Result<Vec<String>, String>`** that validates creds +
  returns the region-scoped model list (or a typed error), then either finalizes the
  draft or `remove_connection(draftId)` on cancel.
- `remove_connection` is extended to **also delete the connection's keychain entry**, so a
  cancelled draft leaves nothing behind.

`test_connection` doubles as the "Test connection & fetch models" button — the one place a
gated step is justified (Bedrock fails distinctly on region / access / profile-id).

## 7. Transport (`ff-llm`)

A new `BedrockProvider` implements the existing `Provider` trait:

- **Endpoint:** region-derived; no `base_url`.
- **API:** Bedrock **Converse / ConverseStream** (unified messages API across vendors,
  native tool-use + streaming) rather than per-vendor `InvokeModel` bodies — keeps the
  provider's request/response mapping close to the OpenAI-compat one.
- **Credentials:** the AWS SDK for Rust credential chain.
  - `BedrockProfile` -> `aws-config` profile provider (`profile_name`) + region resolution.
  - `BedrockIamKeys` -> static credentials from the keychain bundle (+ optional session token).
  - `BedrockApiKey` -> bearer auth (`AWS_BEARER_TOKEN_BEDROCK` / SDK bearer token).
  - SigV4 signing handled by the SDK.
- **Crates:** `aws-config`, `aws-sdk-bedrockruntime` (Converse), `aws-sdk-bedrock`
  (ListInferenceProfiles / ListFoundationModels), `aws-credential-types`.

`build_provider` gains a `ProviderKind::Bedrock` arm; everything else (per-turn rebuild,
registry resolution) is unchanged from RFC 0005 §7.

## 8. Mock parity & phasing

**Mock (`mock.ts`):** seed a Bedrock connection per auth mode, canned region-scoped model
lists keyed by region family, a `test_connection` that echoes success/typed-error per
mode, and stateful draft create/finalize/cancel — so the Bedrock form + model picker build
under `VITE_FF_MOCK=1` before any Rust lands (mirrors RFC 0005 §8).

**Phasing** (relative to RFC 0005 Phase B = keychain):

- **D1 — profile-only, no keychain.** `ProviderKind::Bedrock` + `BedrockProvider`
  (Converse) + region + `BedrockProfile` mode + `ListInferenceProfiles` listing + catalog
  row + conditional-field form machinery + mock parity. Ships **without** Phase B because
  profile mode stores no secret.
- **D2 — keyed modes.** `BedrockIamKeys` + `BedrockApiKey` (require Phase B keychain) +
  `test_connection` draft-probe + `remove_connection` keychain cleanup.

**Manage/switch surface (applies to all vendors, not just Bedrock):** a **grouped flat
list** (vendor section headers -> connections, active one marked), **not** a collapsible
tree — switching is the hot path and a tree adds an expand step per switch. Promote a
single vendor's group to collapsible only if it exceeds N connections (Bedrock-across-
regions is the likely first case). The catalog **autocomplete** is the *add* entry point;
the grouped list is the steady state.

## 9. Non-goals

- **Anthropic-direct** (native `api.anthropic.com`) — a separate kind later; Bedrock is
  the Opus/Sonnet path here.
- **SSO device-flow UI, assume-role chains, MFA prompts** — defer to the user running
  `aws sso login` / configuring the profile externally; we only consume the profile.
- **Provisioning model access, Guardrails, provisioned-throughput ARNs** from inside the
  app.
- **Phenotype -> Bedrock-connection binding** — inherits RFC 0005 Phase C unchanged once
  Bedrock connections exist.
