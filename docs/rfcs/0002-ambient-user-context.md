# 0002 — Ambient User Context (Time & Location)

- **Status:** Proposed
- **Milestone:** time → M3.1b; location → post-M3 (own track, path to M8)
- **Author:** tonytan4ever
- **Depends on:** RFC 0001 (system-prompt injection hook)

## 1. Summary & Goals

FlowForge should know **when** and **where** the user is, so the agent reasons
about the present instead of its training-cutoff past. We call this **ambient
context** — deliberately *not* "tracking."

The differentiator is not *having* time/location (phone assistants do). It is
that FlowForge is a **local-first desktop app**: it can obtain *precise,
consented, OS-level* context **without a cloud round-trip**, surface exactly what
it holds, and let the user override it. No cloud chat tool can match that privacy
posture.

Goals:
- The agent always knows the current local date/time + timezone.
- The agent can know the user's coarse location **when the user opts in**.
- The user can **explicitly override** either, and the override always wins.
- Everything is local-first, transparent, and coarse by default.

## 2. Time and Location Are Different

| | Time | Location |
|---|---|---|
| Permission | none | opt-in |
| Privacy cost | zero | real |
| Default | **on** | **off** |
| Delivery | ambient (always in prompt) | ambient *when available* + tools to manage |
| Risk | none | feels like surveillance if mishandled |

**Time** is a correctness fix (LLMs assume the cutoff date) — ship it now.
**Location** is the differentiated feature — scope it carefully.

## 3. Data Model

```rust
pub struct UserContext {
    /// Always present — current instant + IANA timezone.
    pub now: Instant,            // unix millis + tz id (e.g. "America/Chicago")
    /// Present only when the user has opted in or set it explicitly.
    pub location: Option<Location>,
}

pub struct Location {
    pub label: String,           // "Austin, TX, US" — coarse, human-readable
    pub timezone: String,        // IANA tz
    pub lat: Option<f64>,        // only when a task needs precision
    pub long: Option<f64>,
    pub accuracy: Accuracy,      // Region | City | Precise
    pub source: ContextSource,   // provenance — drives override semantics
}

pub enum ContextSource {
    UserOverride,                // explicitly set; auto must not clobber
    OsGeolocation,
    TimezoneInferred,
    None,
}
```

`UserContext` is the IPC contract for the status indicator; it is `ts-rs`-exported.

## 4. Acquisition Strategy (preference order)

1. **User-declared** — settings field / `set_location` tool. Always available,
   explicit, zero-permission. **Primary mechanism.**
2. **Timezone inference** — derive a coarse region from the OS timezone
   (`America/Chicago` → Central US). Zero-permission, fully local, low precision.
   **Default auto-source.**
3. **OS geolocation** — macOS CoreLocation / Windows Geolocation. Precise,
   permission-gated. Tauri's geolocation plugin is mobile-first; desktop support
   is limited, so this is a **later enhancement**, not phase 1.
4. **IP geolocation** — **rejected.** Coarse *and* requires a third-party network
   call, which breaks local-first.

## 5. Delivery: Ambient Injection + Tools (hybrid)

- **Ambient injection.** A compact `UserContext` block is prepended to the system
  prompt through the **same hook RFC 0001 §4 builds for skill descriptions**
  (M3.1b). Time is always included; location is included only when present.
  Rationale: models rarely think to *call* a tool to check the date/place — it
  must be in front of them.
- **Tools** for the active cases (agent- and user-invokable):
  - `set_location(label, ...)` → `source = UserOverride`.
  - `refresh_location()` → re-run OS geolocation (phase 2+).
  - `clear_location()` → drop to `Auto` / `None`.
  - `get_user_context()` → read current `UserContext` (rarely needed given
    ambient injection; useful for explicit "where am I?" flows).

## 6. Override Semantics

- An explicit `set_location` sets `source = UserOverride` and **sticks** — auto
  sources never clobber a user override.
- A "use my real location" toggle clears the override and re-enables auto.
- Time has no override in phase 1 (a fake-clock/travel mode is a possible future
  testing aid, out of scope).

## 7. Privacy Model (non-negotiable defaults)

1. **Time on by default; location opt-in.**
2. **Local-first** — timezone inference + OS geolocation only; never an IP
   service; never sent to a remote LLM by default (the default provider is local
   candle-vllm regardless).
3. **Coarse by default** — city/region, not GPS coordinates, unless a task
   explicitly needs precision (`Accuracy::Precise`).
4. **Transparency** — a always-visible indicator shows exactly what the agent
   holds (`📍 Austin, TX · 🕒 2:14 PM CDT`), click to edit or clear.
5. **Override always wins.**

## 8. NeuroForge / Signals Synergy

Time and location are textbook **situational signals** for NeuroForge (M8):
late-night sessions → gentler pacing; elapsed-time awareness → break nudges;
location → locale-aware defaults. `UserContext` therefore doubles as an
`ff-signals` source — this "small" feature feeds the cognitive-health thesis.

## 9. Phasing

| Phase | Scope | Where |
|---|---|---|
| **Time** | current local datetime + timezone, ambient-injected | **M3.1b (#25)** |
| **Location P1** | user-declared + timezone-inferred coarse location; status indicator; override | post-M3, own track |
| **Location P2** | OS geolocation (CoreLocation/Win), precise opt-in | later |
| **Signals** | `UserContext` as an `ff-signals` source | M8 (NeuroForge) |

## 10. Non-Goals & Open Questions

**Non-goals:**
- IP-based geolocation (ever).
- Background/continuous location polling — context is sampled on demand / session
  start, not streamed.
- A location *history* — we hold the *current* context only, not a trail.

**Open questions:**
- Status-indicator home: titlebar strip, settings, or ⌘K? (Lean: a small,
  always-visible strip.)
- Timezone-inference granularity — region label only, or attempt a representative
  city? (Lean: region only, to stay honestly coarse.)
- Does precise location ever leave the device? Proposed answer: only inside a
  tool call the user can see, never silently in the system prompt.
