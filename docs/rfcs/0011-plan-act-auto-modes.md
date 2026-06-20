# 0011 — Plan / Act / Auto Modes

- **Status:** Proposed
- **Milestone:** _M3 (UX hardening)_
- **Author:** tonytan4ever
- **Depends on:** RFC 0001 (system-prompt §4 hook), #229 (approval gate), #240 (tool scoping / `allowed` filter), #244 (inner loop)
- **Tracking issue:** #268 (epic)

## 1. Summary & Goals

Introduce three named **agent modes** — **Plan**, **Act**, and **Auto** — that let a
user dial in, per session, how much autonomy the agent has before it touches the
world. The switcher is a single pill in the composer toolbar (and a `--mode` CLI
flag) that the user flips mid-conversation.

The key realisation driving this design: **modes are not new machinery.** They are
named presets over two axes that already exist in FlowForge — *what tools the agent
can see* (`Safety` + the #240 `allowed` filter) and *how mutations get approved*
(#229 four-option gate + session allowlist + the CLI `ApprovalMode`). This RFC adds
a thin naming/UX layer over those axes; it does not add a planner agent, a node
graph, or a second execution loop.

Goals:

- Give the user an at-a-glance, one-click control over agent autonomy, mirroring the
  Plan/Act/Auto control they already rely on in Aki.
- Make **Plan** a genuinely safe "read and think only" mode: the agent literally
  cannot see mutating tools, so it cannot accidentally edit.
- Make **Auto** a low-friction "just do it" mode that still **never** auto-approves a
  Dangerous action (preserving the #229/#232 invariant).
- Keep mode **per-session / per-pane**, resolved the same way phenotype (#246) and
  workspace (#200) already are, with a persisted `defaultMode` user preference.

Non-goal up front: this is *not* an OS-level filesystem sandbox (see §12).

## 2. The three modes

| Mode | Capability (tools advertised) | Approval policy | Closest thing today |
|------|-------------------------------|-----------------|---------------------|
| **Plan** | ReadOnly tools only — Write/Dangerous hidden from the model | n/a — nothing can mutate | *new* |
| **Act** | full registry | Write -> #229 gate / session allowlist; **Dangerous always prompts** | the current default behaviour |
| **Auto** | full registry | Write auto-approved; **Dangerous always prompts** | `ApprovalMode::Yes` + the Dangerous carve-out |

## 3. Modes as two composable axes

FlowForge already has the two primitives a mode is built from:

1. **Capability** — every tool declares `Safety { ReadOnly, Write, Dangerous }`
   (`crates/ff-tools/src/registry.rs`). #240 added a `ToolContext.allowed:
   Option<HashSet<String>>` filter so a sub-agent can be handed a *subset* of the
   registry. Plan mode reuses that exact seam, keyed on `Safety` instead of an
   explicit name set: in Plan, only `Safety::ReadOnly` tools are advertised.

2. **Approval policy** — #229 introduced the four-option approval gate and a
   per-session allowlist; the CLI already exposes `ApprovalMode { Prompt, Yes, Deny }`.
   Act vs Auto differ *only* in whether a Write proceeds without a prompt.

A "mode" is therefore just a **named (capability, approval-policy) preset**:

```
Plan  = (ReadOnly-only registry, no-mutation)
Act   = (full registry,          prompt-on-Write,      always-prompt-on-Dangerous)
Auto  = (full registry,          auto-approve-Write,   always-prompt-on-Dangerous)
```

Because both axes already exist and are already plumbed through `ToolContext` and
`UiApprover`, the implementation is mostly wiring a `Mode` enum into those two points
plus a system-prompt steer — not building parallel subsystems.

## 4. Precise definitions

**Plan.** The registry advertises only `Safety::ReadOnly` tools when assembling the
tool schema for the model; Write and Dangerous tools are simply absent from the
function list the model sees. A per-mode system-prompt preamble (via the RFC 0001 §4
hook) instructs the agent to *investigate and produce a plan, not to edit*, and to
end its turn with a plan the user can review. Because the mutating tools are not even
in the schema, Plan is safe by construction rather than by the model's good behaviour.
MCP tools whose `Safety` is unknown/unannotated are treated as **non-ReadOnly** and are
therefore **excluded** in Plan (fail safe, not fail open).

**Act.** Identical to today's default behaviour. Full registry. A Write tool call
hits the #229 gate unless the session allowlist already covers it; **Dangerous tool
calls always prompt** and are never covered by the allowlist
(`AppState::allowlist_covers` never returns true for Dangerous).

**Auto.** Full registry. Write tool calls are **auto-approved** without a prompt
(equivalent to `ApprovalMode::Yes` for the Write tier). **Dangerous tool calls still
always prompt** — Auto deliberately does not extend auto-approval to Dangerous,
preserving the #229/#232 invariant. Auto leans on the existing safety rails: writes
are jailed to the session workspace root, and git is the undo button. This matches
Aki's own AUTO semantics.

## 5. Where mode lives

Mode is **per-session and per-pane**, resolved exactly the way phenotype (#246) and
workspace (#200) are today:

- A session carries an optional explicit mode (set by flipping the pill or `--mode`).
- If no explicit mode is set, the session **inherits the `defaultMode` preference**.
- The factory value of `defaultMode` is **Auto** — matching Aki's default and the
  user's stated preference. (This is a *decision*, not an open question.)
- Split panes (#148) keep independent per-session mode, just like they keep
  independent composer / workspace state.
- The resolved mode is persisted with the session so reopening restores it.

## 6. Backend delivery

- Add `Mode { Plan, Act, Auto }` to `ff-core` and a `mode` field on `ToolContext`.
- The registry, when building the tool schema for a turn, filters by `Safety` when
  `mode == Plan` (advertise ReadOnly only). This reuses the #240 `allowed` machinery;
  Plan is "the allowed set = all ReadOnly tool names".
- `UiApprover` consults the mode: in Auto, a Write resolves to auto-approve; Dangerous
  always falls through to the prompt path regardless of mode.
- A per-mode system-prompt preamble is injected through the RFC 0001 §4 hook (Plan
  gets the "produce a plan, do not edit" steer; Act/Auto get the normal preamble).
- `send_message` reads the **session-bound** mode the same way it already resolves the
  session phenotype via `state.session_phenotype(&session_id)` — so likely **no new
  argument** to `send_message`; the mode rides along with session state.
- This phase ships value on the CLI alone via `--mode plan|act|auto`.

## 7. Frontend switcher

- Add a **mode pill** to the `InputBar` bottom toolbar
  (`apps/desktop/src/components/input-bar.tsx`), left of Send and beside the
  `WorkspaceSelector`.
- Clicking the pill **cycles** Plan -> Act -> Auto; a keyboard shortcut cycles too.
- The pill is **per-pane** (independent per split, #148) and **persisted**.
- Colour-coded for instant legibility: Plan = blue/calm, Act = green, Auto =
  amber/caution.
- A Settings control sets `defaultMode` (lives in `usePrefsStore` next to
  `sendMessageKey`).
- `Mode` is exported to TypeScript via ts-rs bindings.

## 8. Plan -> Act handoff

v1 keeps this dead simple: a plan is just a normal assistant message (no new artifact
type, no new IPC). When the user is happy with the plan, they flip the pill to Act (or
Auto) and send "go". An optional polish (P4) adds a one-click **"Approve plan & switch
to Act"** affordance on a turn that ended in Plan, plus a Plan empty-state hint.

## 9. Relationship to ApprovalMode / #229

Modes are the **unifying UX** over the approval primitives, not a replacement:

- `ApprovalMode::Yes` ~= Auto (for the Write tier); `ApprovalMode::Prompt` ~= Act.
- The #229 gate and session allowlist are unchanged; mode only decides whether a Write
  consults them or auto-approves.
- The Dangerous-always-prompts rule is owned by the approval layer and is mode-independent.
- The CLI gains `--mode` as the user-facing front door to the same behaviour.

## 10. Data model

- `Mode { Plan, Act, Auto }` enum in `ff-core`, `#[derive(...)]` + ts-rs export.
- `ToolContext.mode: Mode`.
- Session-bound resolved mode (persisted with the session record).
- `defaultMode: Mode` preference in the prefs store; new sessions inherit it.
- No index/IPC schema churn beyond the single session field.

## 11. Phasing

| Phase | Label | Scope | Ships alone? |
|-------|-------|-------|--------------|
| **P1** | backend | `Mode` enum + Plan capability gating (Safety-filtered schema via #240 seam) + per-mode prompt preamble (RFC 0001 §4). Unit-tested in ff-agent/ff-tools. | Yes — CLI value |
| **P2** | backend | Approval policy (Auto auto-approves Write; Dangerous always prompts) + session-bound mode state (resolved like #246 phenotype) + `UiApprover`/`send_message` wiring + persist `defaultMode` + new-session inheritance + CLI `--mode`. | Yes |
| **P3** | frontend | Mode pill in `InputBar` (click + keyboard cycle, per-pane, persisted, colour-coded) + Settings `defaultMode` control + TS bindings. | Yes |
| **P4** | frontend | *(stretch)* "Approve plan & switch to Act" one-click + Plan empty-state hint. | Optional |

Dependency: **P1 -> P2/P3**; P4 last and optional.

## 12. Non-goals & open questions

**Non-goals:**

- A separate "planner agent" or second execution loop. Plan reuses the one
  `run_turn`; it just sees fewer tools.
- Node-graph / visual workflow builders.
- **Plan is not an OS-level filesystem sandbox.** It gates at the *tool layer* (the
  mutating tools are not advertised), not via a syscall jail. A model that found some
  other side channel is out of scope for Plan's guarantee. This is an honest, stated
  limitation, not an oversight.

**Open questions:**

- Should Plan, when it emits a plan, proactively suggest switching to Act (beyond the
  P4 button)?
- Do we want to surface the active mode in the transcript header per turn (so a scroll
  back shows which mode produced which turn)?

**Resolved:** the default mode is **Auto** (factory `defaultMode`). Not open.
