# 0019 — Permission Matrix & Sensitive Tier

- **Status:** Proposed
- **Milestone:** 0.2.0
- **Author:** tonytan4ever
- **Depends on:** RFC 0011 (Plan/Act/Auto modes), #229 (approval gate)
- **Tracking issue:** #682
- **Supersedes / extends:** RFC 0011 §3 (modes as two composable axes)

## 1. Summary & Goals

Extend the three-tier tool Safety model (`ReadOnly / Write / Dangerous`) with a
fourth tier — **`Sensitive`** — covering externally-visible side effects (network
egress, git push, remote MCP mutations, sub-agent spawn). Surface the resulting
**Mode x Tier** permission matrix as a first-class editable Settings panel ("Control"),
add per-tool **Custom Overrides** (Denied / Require-Approval / Allowed), and align
FlowForge's permission UX with the reference control panel.

Goals:

- **Separate "local disk write" from "remote side effect"** — today both are `Write`,
  so Auto mode either auto-approves network egress (too risky for autonomous loops)
  or gates everything including local edits (too slow for interactive coding). The
  Sensitive tier lets Auto mode auto-approve local writes while still asking before
  touching the network.
- **Make the permission model data-driven** — replace the hardcoded if/match
  branches in `mode.rs` / `lib.rs` with a default matrix (each cell =
  Allow/Ask/Deny) that the user can override. Plan/Auto/Act become named presets
  over that matrix; a future "Custom" mode can express any cell combination.
- **Ship the "Control" settings panel** — an editable matrix matching the reference
  screenshot (rows = tiers, columns = modes, cells = ✓/Ask/✗) plus collapsible
  Custom Override lists.
- **Enable safe autonomous goal mode** — this is the trust substrate that makes
  persistent self-running loops (#74) viable: local changes fly through, external
  pushes gate, destructive commands hard-deny. Without this, goal mode is
  "autonomous with no guardrails."

Non-goals:

- OS-level sandboxing (RFC 0011 §12 stands).
- Per-**workspace** or per-skill permission scoping — a different trust axis (which
  project), deferred to a future RFC. Note this is distinct from the per-**argument**
  scoped rules in §9, which this RFC does cover (which path/command, within the
  active workspace).
- The "Prompts" / "Team" / "UI" tabs visible in the reference panel (separate scope).

## 2. The four safety tiers

| Tier | Semantic | Examples |
|------|----------|----------|
| **ReadOnly** | Pure observation, no side effects | `view`, `grep`, `glob`, `tree`, `memory_search` |
| **Write** | Mutates local state (files, packages, git local ops) | `write`, `edit`, `bash` (local), `apply_patch` |
| **Sensitive** | Externally-visible side effects — network egress, remote mutations | `web_fetch`, `web_search`, `git push`, external MCP tools, `agent_spawn` |
| **Dangerous** | Irreversible or catastrophic — data loss, broad system changes | `bash rm -rf`, `python` (arbitrary exec), destructive CLI |

### 2.1 Reclassification

Tools currently classified as `Write` that become `Sensitive`:
- `web_fetch` (network egress)
- `web_search` (network egress)
- `agent_spawn` / sub-agent delegation (spawns work outside the current turn)
- MCP bridge calls flagged as external by the MCP server manifest (#18 tiering)

The `bash` and `python` tools already use a **per-invocation safety hint** from the
model (the `safety` field in their JSON schema). This remains: a bash call classified
`write` stays Write; one classified `sensitive` promotes to Sensitive. The schema
description gains the new tier name.

## 3. Default permission matrix

|                    | Plan (Read Only) | Auto (Balanced) | Act (Full Access) |
|--------------------|:---:|:---:|:---:|
| **ReadOnly**       | ✓ Allow | ✓ Allow | ✓ Allow |
| **Write**          | ✗ Deny (tools hidden) | ✓ Allow | ✓ Allow |
| **Sensitive**      | ✗ Deny (tools hidden) | ⚠ Ask | ✓ Allow |
| **Dangerous**      | ✗ Deny (tools hidden) | ✗ Deny | ⚠ Ask |

Key changes from RFC 0011:
- Auto no longer auto-approves everything above ReadOnly — Write is still
  auto-approved, but Sensitive gates and Dangerous hard-denies.
- Act now auto-approves Sensitive (external changes fly through) but still prompts
  on Dangerous (preserving the #229/#232 invariant).
- Plan is unchanged (only ReadOnly tools visible).

### 3.1 Matrix as data

```rust
/// One cell in the permission matrix.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub enum PermissionCell {
    Allow,
    Ask,
    Deny,
}

/// The full mode x tier matrix, serializable to `~/.flowforge/permissions.json`.
pub struct PermissionMatrix {
    pub plan:  [PermissionCell; 4],  // indexed by Safety ordinal
    pub auto:  [PermissionCell; 4],
    pub act:   [PermissionCell; 4],
}
```

The runtime resolves tool visibility + approval policy from a single matrix lookup:
- `Deny` in Plan/Act/Auto hides the tool entirely (same as Plan-hides-Write today).
- `Ask` shows the tool but routes through the approval gate before execution.
- `Allow` auto-executes.

## 4. Custom Overrides

Three per-tool lists, persisted in `~/.flowforge/permissions.json`, that override
the matrix for any mode:

| List | Semantic |
|------|----------|
| **Denied** | Tool is never callable regardless of mode or tier |
| **Require Approval** | Tool always prompts regardless of its tier's cell |
| **Allowed** | Tool auto-executes regardless of its tier's cell (existing `always_approved`) |

Overrides take precedence: Denied > Require Approval > Allowed > Matrix cell.

## 5. Settings UI ("Control" panel)

A new Settings section (or tab alongside Model/Skills/Memory/Scheduled):
- Top: the matrix table, directly editable (click a cell to cycle Allow/Ask/Deny).
  The currently active mode column is highlighted.
- Bottom: collapsible Custom Override lists (Denied / Require Approval / Allowed),
  each showing tool names with add/remove.
- "Reset to defaults" button restores the §3 default matrix.
- Changes persist immediately to `permissions.json`; active sessions see the new
  policy on the next tool call.

## 6. Migration from RFC 0011

- Existing `tool_permissions.json` (`always_approved` list) migrates into the
  "Allowed" override list.
- Existing `default_mode` preference continues to work; the matrix just gives it
  finer-grained meaning.
- The Safety enum gains `Sensitive` (ordinal between Write and Dangerous).

## 7. Impact on goal mode

Goal mode (#74) runs under the active mode's column. The recommended pairing:

| Goal mode scenario | Recommended mode | Effect |
|--------------------|-----------------|--------|
| Autonomous self-dev (FlowForge on FlowForge) | Auto | reads+writes fly; push/PR gates; rm -rf denied |
| Research loop (web retrieval + summarize) | Auto | web_fetch Ask; local writes fly |
| Full trust CI/release | Act | everything except Dangerous flies |

This RFC is a **hard prerequisite** for shipping goal mode to users. Without it,
Auto-mode goal loops auto-approve network egress with no gate.

## 8. Implementation plan (high level)

1. Add `Safety::Sensitive` to `ff-core`; update ts-rs bindings.
2. Reclassify `web_fetch`, `web_search`, MCP external, `agent_spawn`.
3. Introduce `PermissionMatrix` struct + `permissions.json` load/save/migrate.
4. Replace hardcoded mode logic in `ff-agent` (tool filtering + approval gate) with
   matrix lookup.
5. Ship the Settings → Control panel (FE).
6. Write tests covering each cell x override combination.
7. Update system-prompt mode preamble to describe the new Sensitive tier.
8. Add scoped permission rules (§9): `PermissionRule` + matchers, evaluated before
   overrides, with the Allow/Deny asymmetry (§9.3) and workspace-anchoring rails
   (§9.4), plus the Control-panel "Rules" list. Lands after per-tool overrides.

## 9. Scoped permission rules (path / command matchers)

The matrix (§3) and overrides (§4) are keyed by **tool name + tier** only. That
cannot express *"auto-approve a Write **when the path is under `~/workspaces/**`**,
but keep asking elsewhere"* or *"auto-approve `bash` **when the command is
`brazil-build …`**, but gate every other command"* — the exact rules that make an
unattended long-running loop (#74) stop stalling on confirmation dialogs. This
section adds an **argument-scoped** layer evaluated *before* overrides and the matrix.

### 9.1 The rule

```rust
/// An ordered, first-match-wins rule evaluated before overrides + matrix.
pub struct PermissionRule {
    pub effect: RuleEffect,   // Allow | Deny
    pub tool: String,         // tool id the rule applies to (e.g. "bash", "write")
    pub matcher: ArgMatcher,  // what argument shape it matches
}

pub enum RuleEffect { Allow, Deny }

/// Per-tool-kind argument matcher. The runtime picks the field to test from the
/// tool: path-taking tools match on their path arg; `bash` matches on `command`.
pub enum ArgMatcher {
    /// Glob against the tool's path argument (view/edit/write/apply_patch, and any
    /// filesystem tool). Workspace-anchored — see §9.4.
    PathGlob(String),        // e.g. "~/workspaces/**", "src/**/*.rs"
    /// Prefix match against `bash`'s `command` (after trimming), token-aware so
    /// "brazil-build" does not match "brazil-build-evil".
    CommandPrefix(String),   // e.g. "brazil-build"
    /// Anchored regex against `bash`'s `command`, for the deny backstop.
    CommandRegex(String),    // e.g. "^rm\\s+-rf\\b", "git\\s+push\\s+.*--force"
}
```

Rules persist in the same `~/.flowforge/permissions.json` under a `rules: [...]`
array, `#[serde(default)]` so existing configs (matrix + overrides only) load
unchanged.

### 9.2 Precedence

Extends §4's chain — a rule sits **above** the per-tool override:

```
Deny rule  >  Allow rule  >  override (Denied/RequireApproval/Allowed)  >  matrix cell
```

First matching rule in file order wins. A rule that does not match falls through to
the next layer unchanged. This makes the deny backstop absolute: a `Deny` rule
(e.g. `rm -rf`, `git push --force`) fires regardless of mode, override, or tier.

### 9.3 The Allow / Deny asymmetry (a hard invariant)

- A **Deny rule may veto any tier, including `Dangerous`.** Backstops must be
  unconditional.
- An **Allow rule may NEVER auto-clear `Dangerous`.** It can auto-approve at most a
  `Sensitive` call (mirroring today's `allowlist_covers`, which already refuses to
  cover `Dangerous`, state.rs). An Allow rule matching a `Dangerous` call degrades
  to `Ask`, never `Allow` — preserving the #229/#232 invariant that no configuration
  path auto-runs a Dangerous action.

### 9.4 Safety rails (this is the unattended path — rails are load-bearing)

- **Allow rules apply only in Auto/Act, never Plan.** Plan stays read-only by
  construction; a stray Allow rule cannot re-open it.
- **Path globs are workspace-anchored.** A glob resolves against the session's cwd /
  the configured allowed roots; a pattern that escapes it (via `..` or an absolute
  path outside the roots) is rejected at rule-save time, not silently honored. An
  Allow rule can only *widen auto-approval within* the workspace, never outside it.
- **Every rule-driven auto-approve is logged** (`tracing`) with the rule + resolved
  argument, so an unattended run leaves an audit trail of exactly what flew through.
- **`bash` still consults its per-invocation safety hint (§2.1) first.** A command
  the model itself flags `dangerous` cannot be auto-approved by a `CommandPrefix`
  Allow rule — the rule matches, but §9.3 degrades it to `Ask`.

### 9.5 Settings UI

The Control panel (§5) gains a **"Rules"** list below the Custom Overrides:
- Each row: effect (Allow/Deny) · tool · matcher, reorderable (order = precedence).
- Add/edit/remove; a glob/regex is validated on save (workspace-anchoring for globs,
  compile check for regexes).
- "Reset to defaults" clears user rules and restores a small built-in **deny
  backstop set** (e.g. `rm -rf` on `/` or `~`, `git push --force`, disk-format
  commands) so the destructive-command floor exists out of the box.

## 10. Open questions

- Should MCP tools default to `Sensitive` (fail safe) or `Write`? Proposed: default
  `Sensitive` for external MCP servers; `Write` for local/stdio MCP.
- Should the user be able to define per-workspace overrides (different project =
  different trust)? Deferred to a follow-up RFC.
- Should scoped rules (§9) support a per-workspace *ruleset* (rules that only apply
  when the session cwd is under a given root), or stay global with workspace-anchored
  globs? Proposed: global rules with workspace-anchored globs for 0.2.0; per-workspace
  rulesets fold into the same follow-up RFC as per-workspace overrides.
- Should "Dangerous" in Act remain Ask or become Allow? Proposed: keep Ask (the
  #229/#232 invariant: no mode ever auto-approves Dangerous).
