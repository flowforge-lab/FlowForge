# 0001 — Skills, Phenotypes & Skill Evolution

- **Status:** Proposed
- **Milestone:** M3
- **Author:** tonytan4ever
- **Supersedes:** —
- **Tracking issue:** _M3: Skills, Phenotypes & Skill Evolution_

## 1. Summary & Goals

M3 gives FlowForge a runtime **Skill** system, **Phenotypes** that bind a working
set of skills together, and a first-class **Skill Evolution** capability. The
guiding goal is that **discovering, installing, and improving skills is
effortless** — the friction Aki-style ecosystems usually push onto the user is
absorbed by the app.

Three high-level capabilities anchor the milestone (the original proposal):

1. **Install / "bootstrap" a skill** — unpack a skill from a GitHub URL, a local
   path, or a raw Markdown document, behind a hard approval gate.
2. **Discover a skill** — help the user find a skill that fits a need, via search
   over installed + (later) registry skills, surfaced through the ⌘K palette.
3. **Evolve a skill** — based on a skill's *actual usage patterns* (cost, turn
   count, success rate), generate a streamlined, cheaper, better version of it,
   reviewed and approved by the user before it replaces the original.

### A note on terminology

The container for a working set is a **Phenotype** (user-facing short form:
`pheno`). The name is deliberate: installed skills are the latent "genes"; a
Phenotype is the *expressed* set of skills + model + persona active in a given
context. **Skill Evolution** (§8) is then exactly what the metaphor implies —
improving the genes from how they express in practice. We avoid "Profile"
(overloaded / Aki-flavored) and infra-flavored terms like "Loadout".

Non-goals for M3 are listed in §11.

## 2. The Four-Layer Model

The system deliberately keeps four concepts separate. Conflating them is the
main design risk, so the boundary is stated up front and enforced.

| Layer | What it is | Crate | Lifecycle |
|-------|-----------|-------|-----------|
| **Tool** | A compiled Rust callable the model invokes | `ff-tools` | compile-time |
| **MCP server** | An external process exposing tools | `ff-mcp` | runtime spawn (M4) |
| **Skill** | Markdown instructions + declared tool/MCP references + read-only resources | `ff-skills` | runtime load / hot-reload |
| **Phenotype** | The active set of `{skills, tools, model, persona}` for a session | `ff-core` + config | per-session switch |

A Skill never *contains* a Tool; it *references* tools by name. A Phenotype never
*contains* a Skill body; it *selects* skills by name.

## 3. Skill Data Model & `SKILL.md` Schema

A skill is a directory under `~/.flowforge/skills/<name>/` with a `SKILL.md`
entry point and optional **read-only** resource files. **M3 skills carry no
executable scripts** (see §9) — they are instructions plus references only.

```markdown
---
name: rust-debugging
description: Systematic Rust debugging — read errors, isolate, fix root cause.
version: 0.1.0
author: tonytan4ever
tools: [bash, view, edit]        # must resolve in the ToolRegistry at load
mcp: []                          # MCP server ids — declared, enforced in M4
keywords: [rust, debug, cargo]   # used by discovery / search
---

# Rust Debugging

When the user reports a Rust failure:
1. Run the failing command and read the full error output verbatim.
2. ...
```

- **`description`** is short and is *always* injected into the system prompt
  (cheap — roughly one line per installed skill).
- The **body** (everything after the frontmatter) is injected only when the
  skill is **active** in the current phenotype.
- `resources/*.md` are optional reference files the body may point at; they are
  read-only and never executed.

### `ff-core` types (with `ts-rs` bindings)

```rust
pub struct SkillManifest {
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: Option<String>,
    pub tools: Vec<String>,
    pub mcp: Vec<String>,
    pub keywords: Vec<String>,
}

pub struct Skill {
    pub manifest: SkillManifest,
    pub body: String,
    pub path: PathBuf,
}

pub struct Phenotype {
    pub name: String,
    pub skills: Vec<String>,     // active skill names
    pub model: Option<String>,   // overrides DEFAULT_MODEL
    pub persona: Option<String>, // extra system-prompt preamble
}
```

## 4. `ff-skills` Engine

Today `ff-skills` is a stub. M3 makes it real:

- **Parser** — split frontmatter (YAML) from body; validate required fields;
  verify every `tools:` entry exists in the registry; reject any executable
  files in the bundle.
- **Loader** — scan `~/.flowforge/skills/`, build a `SkillRegistry`
  (`name -> Skill`).
- **Hot-reload** — a `notify` filesystem watcher rebuilds the registry on change.
  The agent loop **snapshots the active skill set at turn start** so a mid-turn
  reload never races (§9, risk 1).

### System-prompt injection (`ff-agent`)

`run_turn` currently sends `to_chat(&history)` with no system message. M3 adds a
single, well-tested insertion point that prepends a system message built from:

1. the active phenotype's `persona` (if any),
2. the `description` of every installed skill (always),
3. the full `body` of every **active** skill.

## 5. Installer — `install_skill(source)`

Installing is a **core capability**, not a skill — it must be deterministic and
cannot depend on an LLM following instructions, and it solves the chicken-and-egg
problem of installing the first skill.

- Exposed as both a Tauri command and an agent-callable tool.
- **M3 sources:** git URL, local path, raw Markdown / gist. (A
  `flowforge://skill/<ns>/<name>@<ver>` registry scheme is deferred to M4.)
- **Flow:** fetch into a temp dir → validate manifest + reject executables →
  present declared tools / permissions through the **M2 approval gate** (same
  Approve/Deny UX as a dangerous tool call) → on approval, move into
  `~/.flowforge/skills/`.
- `uninstall_skill(name)` removes the directory and updates any referencing
  phenotypes.

## 6. Discovery — `search_skills` + ⌘K backend

The ⌘K command palette UI is owned by the palette work (issue #11/#16). M3
delivers the **backend** so that FE wires onto a stable contract:

- `search_skills(query)` tool — ranks installed skills by keyword + description
  match (semantic match via `ff-memory` embeddings comes with the M4 registry).
- `list_skills` / `activate_skill` / `deactivate_skill` Tauri commands returning
  stable DTOs.
- Events documented in the M3.3 PR so the palette can bind without churn.

## 7. Phenotypes

- Stored as `~/.flowforge/phenos/<name>.toml`.
- A phenotype selects active skills, an optional model override, and an optional
  persona preamble.
- A `switch_phenotype(name)` command changes the active set and persists it across
  restarts. A default phenotype ships with the built-in tools and no skills.
- User-facing surfaces (palette, CLI) use the short form **`pheno`** (e.g.
  `pheno switch rust`) to keep the spelling friction low.

## 8. Skill Evolution

This is the headline capability: a skill that **improves other skills** from
their real usage patterns. M3 ships the complete *manual* loop; the *autonomous
trigger* is the only deferred piece (§8.1).

### Telemetry substrate (`ff-signals`)

`ff-signals` becomes a real event bus. Per skill it records:

- `SkillActivated { skill, session_id }`
- `SkillCompleted { skill, tokens, latency_ms, turns, success }`

Aggregates (rolling token cost, mean turns, success rate) are persisted per skill
so evolution has patterns to learn from. The `IntentionSignal` / `OutcomeSignal`
types already in `ff-core::events` are extended rather than replaced.

### Manual optimize flow

1. User invokes "evolve / optimize `<skill>`" (command or conversational skill).
2. The system gathers the skill body + its aggregate telemetry + a sample of
   recent transcripts where it was active.
3. The model proposes a streamlined rewrite aimed at fewer tokens / fewer turns
   while preserving behavior.
4. The user sees a **before → after diff with a cost estimate**.
5. Acceptance goes through the **M2 approval gate** → on approval the skill is
   **version-bumped**; the previous version is retained for **rollback**.

A skill is never silently overwritten.

### 8.1 Deferred: autonomous trigger (M4)

The system deciding *on its own*, unprompted, that a skill is worth evolving
requires accumulated data plus guardrails (cost ceilings, regression detection,
rollback automation). It is explicitly out of scope for M3 and sketched here only
so the telemetry schema is forward-compatible.

## 9. Security Model

Installing skills means ingesting third-party content, so:

1. **No executables in M3.** The installer rejects bundles containing executable
   files (`.sh`, `.py`, files with the execute bit). M3 skills are instructions +
   tool/MCP references + read-only resources only. This removes the largest
   attack surface from the first release.
2. **Validate before placement.** Git/remote sources are fetched into a temp dir
   and fully validated *before* anything moves into `~/.flowforge/skills/`.
3. **Approval gate on install and on evolution.** Reuses the M2 approver — the
   user sees declared tools/permissions before anything is trusted.
4. **Tools still run jailed.** When a skill drives tool calls, those calls go
   through the existing `ff-tools` jail; skills gain no new execution privilege.

### Risks / watch-items

1. **Hot-reload race** — snapshot active skills at turn start.
2. **Git install** — shallow clone to temp, validate + reject executables before
   moving into place, then approval gate.
3. **System-prompt bloat** — inject descriptions always, bodies only for active
   skills; cap active-skill count per phenotype.
4. **Palette coordination** — M3.3 exposes stable DTOs + events, documented so
   Abid's ⌘K FE binds without churn.

## 10. PR Breakdown & Acceptance Criteria

Each PR: `cargo fmt --check` + `clippy -D warnings` + `cargo test --workspace`,
plus `pnpm typecheck && lint && build`, and regenerated `ts-rs` bindings — all
green before merge. One squashed commit per PR (CONTRIBUTING).

| PR | Scope | Acceptance |
|----|-------|-----------|
| **M3.0** | `ff-core` `SkillManifest` / `Skill` / `Phenotype` + bindings | types compile, bindings export, serde round-trip tests |
| **M3.1** | `ff-skills` parser + dir loader + `SkillRegistry` + `notify` hot-reload | parses valid/invalid manifests, rejects executables, hot-reload test |
| **M3.1b** | skill-description injection into `ff-agent` system prompt | system msg carries active descriptions; existing turn tests stay green |
| **M3.2** | `install_skill(source)` (git/path/MD) + validation + M2 approval gate + `uninstall_skill` | install from path + git; approval event fires; bad bundle rejected |
| **M3.3** | `search_skills` tool + `list/activate/deactivate_skill` commands + events (Abid wires ⌘K FE) | search ranks by keyword/description; commands return DTOs |
| **M3.4** | Phenotypes: config model, `~/.flowforge/phenos/*.toml`, switch + persist | switch changes active skills; persists across restart |
| **M3.5** | Skill Evolution: `ff-signals` telemetry + aggregates + manual optimize (propose → diff → approve → version + rollback) | signals recorded per skill; optimize proposes a rewrite gated by approval |

## 11. Open Questions & Non-Goals

**Non-goals (M3):**
- Skill **registry** / `flowforge://` addressing — M4.
- **Executable** skill bundles — later, behind a stronger sandbox.
- **Autonomous** evolution trigger — M4 (§8.1).
- **MCP** enforcement — `mcp:` is parsed and declared in M3, enforced in M4.

**Open questions:**
- Cap on active skills per phenotype (proposed: soft cap + warning).
- Whether evolution telemetry persists in `ff-memory`'s store or a dedicated
  signals store.
