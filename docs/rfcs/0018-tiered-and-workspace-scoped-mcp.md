# 0018 — Tiered & Workspace-Scoped MCP Servers

- **Status:** Proposed
- **Milestone:** M4+ (MCP host hardening)
- **Author:** tonytan4ever
- **Depends on:** RFC 0003 (MCP host — supervisor, reconcile, bridge), RFC 0005 §11.2
  (three-tier model resolution — `resolve_model_selection`, the precedence pattern this
  mirrors), RFC 0001 (phenotype = a switchable working set), RFC 0012 (durable session
  persistence — the `ff-session` store pattern for the session tier)
- **Tracking issue:** #557 (reconcile global MCP with session-scoped workspace), with the
  decoupling win for #573 (Codon <-> codegraph tight coupling)
- **Supersedes:** the two-phase recommendation in #557 (drops its Phase 1 "Option C
  correctness guard" — see §11.3)

## 1. Summary & Goals

The MCP host (RFC 0003) gives FlowForge exactly **one** way to define a server (the
global `~/.flowforge/mcp.json`) and exactly **one** running instance per server id
(`handles: BTreeMap<String, ServerHandle>` in `crates/ff-mcp/src/supervisor.rs`). Two
independent problems fall out of that:

1. **No per-context provenance.** A server is either globally on or globally off. A
   phenotype that depends on a server (Codon needs codegraph) cannot *declare* it; the
   dependency is faked by seeding a disabled global entry
   (`apps/desktop/src-tauri/src/state.rs` `seed_codegraph_mcp_entry_if_absent`). A
   single session cannot add a project-specific server or disable a noisy global one
   for itself. This is the coupling #573 Problem 2 calls out: Codon is wired to
   codegraph through a global side-door, not an explicit phenotype declaration.

2. **No instance multiplicity.** codegraph is workspace-aware, but only via a
   process-cwd side-channel on the single global instance, re-pointed every turn
   (`state.rs` `align_codegraph_workspace` -> `SupervisorHandle::set_server_cwd`). With
   panes / concurrent sessions on different folders (#246/#265) this *thrashes* (restart
   + re-index every turn) and *corrupts* (a tool call in turn A on `/A` can land on a
   codegraph just re-pointed at `/B`). #557 documents the mechanics.

This RFC closes both by separating two axes that the current design conflates, then
defining how they compose:

| Axis | Question | Mirrors |
|------|----------|---------|
| **1. Config tier (provenance)** | *Which* servers exist for a turn, with what definition? | RFC 0005 §11.2 model resolution (`session > phenotype > global`) |
| **2. Instance scope (multiplicity)** | *How many* children run, keyed by what? | #557 Option B (workspace-keyed, ref-counted) |

Goals:
- **Consistent resolution philosophy.** MCP servers resolve through the *same*
  `session > phenotype > global` precedence the model already uses, so the mental model
  is one model, not two.
- **Correct under concurrency.** Two panes on two folders get two correct codegraph
  instances; two panes on the *same* folder share one; no thrash, no cross-session
  corruption.
- **Explicit dependencies.** A phenotype declares the servers it needs; a non-codegraph
  phenotype never spawns codegraph; the global seed retires.
- **Back-compatible.** An existing `mcp.json` with no `scope` field behaves exactly as
  today (one global instance per id).

Non-goals are in §12.

## 2. The Two Axes

The current design has one axis with one value on each: provenance is always *global
file*, multiplicity is always *one*. Generalizing them independently is what keeps this
tractable.

- **Axis 1 (provenance)** answers *where a server's definition comes from*. A turn's
  effective server set is the global file overlaid by the active phenotype's declared
  servers overlaid by the session's own overrides. This is pure config composition; it
  changes nothing about how a server runs.
- **Axis 2 (multiplicity)** answers *how a resolved server is instanced*. A `global`
  server is a shared singleton (today's behavior). A `workspace` server gets one child
  per distinct workspace path, reference-counted across sessions on that path.

They are orthogonal: a server declared at *any* tier may be `global` or `workspace`.
codegraph happens to be a phenotype-tier, workspace-scoped server; a `github` server is
a global-tier, global-scoped server. Neither property implies the other.

## 3. Axis 1 — Tiered Config Resolution

### 3.1 Data model

The server definition type gains nothing for Axis 1 (it gains `scope` for Axis 2, §4.1).
What changes is *where a `Vec<McpServerConfig>` can come from*:

```rust
// crates/ff-core/src/skill.rs — Phenotype gains a server-definition list.
pub struct Phenotype {
    pub name: String,
    pub skills: Vec<String>,
    pub model: Option<String>,
    pub provider: Option<ConnectionId>,
    pub persona: Option<String>,
    pub max_iterations: Option<usize>,
    /// Servers this phenotype brings with it (the phenotype tier). Overrides a
    /// global entry of the same id; suppressed for a turn whose session overrides
    /// the same id. Default empty -> phenotype contributes nothing (today's behavior).
    pub mcp_servers: Vec<McpServerConfig>,   // NEW
}
```

The **session tier** is persisted exactly like the existing per-session model pin
(`ff-session` `set_session_model`/`session_model`, store column `model`): a new nullable
`mcp_servers` JSON column with `set_session_mcp_servers(session_id, Vec<McpServerConfig>)`
/ `session_mcp_servers(session_id)`. No new table; the migration mirrors the v5->v6 model
column add.

`SkillManifest.mcp: Vec<String>` (dependency *ids*) is unchanged and keeps its current
job: the phenotype-unavailable check (#301/#573 2c) still asks "are the ids my active
skills depend on resolvable and running?" — it just asks against the *resolved* set
(§3.2), not the raw global file.

### 3.2 Resolution algorithm

A per-turn `AppState::resolve_mcp_servers(session_id) -> Vec<McpServerConfig>`, the exact
sibling of `resolve_model_selection` (`state.rs:1465`):

```
let global   = load(~/.flowforge/mcp.json);          // tier 3
let pheno    = session_phenotype(session_id).mcp_servers; // tier 2
let session  = store.session_mcp_servers(session_id);     // tier 1 (top)

// Compose by id; later tier wins WHOLE-RECORD (no field-level merge — see §11.5).
// A tier may set `disabled: true` to suppress an inherited server for this turn.
let mut by_id: IndexMap<String, McpServerConfig> = global;
for s in pheno  { by_id.insert(s.id, s); }   // override-by-id
for s in session { by_id.insert(s.id, s); }
by_id.values().filter(|s| !s.disabled).collect()
```

This matches the model resolver's "the resolved record is coherent; we never half-merge
two tiers" rule — a phenotype's codegraph entry wins as a unit (command + args + env +
scope), never a Frankenstein of global command with phenotype args.

### 3.3 Why this mirrors model resolution (and why that matters)

The model already resolves `session > phenotype > global` at turn start, once, into a
coherent `(connection, model)` (RFC 0005 §11.2). Making MCP resolve the same way means a
user reasons about *both* the same way: "my session pin wins; else my phenotype; else the
global default." It also reuses the same turn-start seam — `resolve_mcp_servers` is called
right where `resolve_model_selection` already is, in the turn's setup.

## 4. Axis 2 — Instance Scope & Keying

### 4.1 The `scope` field

```rust
// crates/ff-core/src/mcp.rs
#[derive(Default, ...)]   // ts-rs export -> apps/desktop/src/bindings/
#[serde(rename_all = "lowercase")]
pub enum McpScope {
    #[default]
    Global,      // one shared instance for the whole app (today's behavior)
    Workspace,   // one instance per distinct workspace path, ref-counted
}

pub struct McpServerConfig {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub disabled: bool,
    #[serde(default)]
    pub scope: McpScope,     // NEW; absent in JSON => Global => back-compat
}
```

### 4.2 Instance key

The supervisor's handle map re-keys from `server_id` to a composite:

```rust
struct InstanceKey { id: String, scope_key: ScopeKey }
enum ScopeKey { Global, Workspace(PathBuf /* canonicalized */) }
handles: BTreeMap<InstanceKey, ServerHandle>   // was BTreeMap<String, ServerHandle>
```

- A `Global`-scope server -> `InstanceKey { id, Global }`. Exactly one, as today.
- A `Workspace`-scope server resolved for a session on `/path` ->
  `InstanceKey { id, Workspace(canonical("/path")) }`. Two sessions on the same canonical
  path share one instance (ref-count > 1); two on different paths get two.

`reconcile`, `restart`, `set_server_cwd` (retired — see §4.4), health probes, and
`call_tool` all key by `InstanceKey`. The `McpServerStatus` snapshot gains the
`scope_key` so the UI can show "codegraph (/A)" vs "codegraph (/B)".

### 4.3 Desired set & ref-counting

The supervisor no longer derives its desired set solely from the watched file. The
desktop computes it and pushes it (the supervisor already supports a pushed
`SharedConfig` + `reconcile_now`):

```
desired = {}
for each LIVE session s:
    for srv in resolve_mcp_servers(s.id):          // §3.2
        key = match srv.scope {
            Global    => InstanceKey{ srv.id, Global },
            Workspace => InstanceKey{ srv.id, Workspace(canonical(s.workspace)) },
        }
        desired.entry(key).or_default().push(s.id)   // ref-list
```

A `Workspace` instance is **evicted when its ref-list empties** — i.e. no live session
references that path (session closed, or its workspace changed). `Global` instances
declared by the global tier are always-on, as today. The existing idle health-probe /
clean-exit reaping (RFC 0003 §5, `supervisor.rs` module docs) is unchanged and composes:
an idle-exiting codegraph still reconnects on demand.

### 4.4 roots/rootUri, not process cwd (#557 Finding 1)

#556 pointed codegraph at the workspace via `cmd.current_dir(dir)` only
(`crates/ff-mcp/src/client.rs` `connect`), and the client advertises **no** roots
capability (`ListChangedFlag` is a bare handler). codegraph's CLI documents the intended
channel: `-p, --path <path> (optional for MCP mode, uses rootUri from client)`. So:

- A `Workspace`-scoped server advertises its resolved path as an **MCP root (rootUri)**
  via a `ClientHandler` that implements `list_roots`, set at connect from the
  `InstanceKey`'s `Workspace(path)`. Process cwd may still be set as a belt-and-braces
  fallback, but the contract a workspace-aware server keys on is the root.
- **A workspace-scoped server MUST NOT be configured with a hard-coded `--path`.** A
  pinned `--path` silently defeats per-turn alignment (verified in #557 Finding 1 — the
  server serves that fixed path forever). The RFC documents this; C3's example codon
  config omits `--path`, and we add a lint/warn when a `scope: workspace` server's args
  contain `--path`.

`set_server_cwd` (the #556 side-channel) is **retired**: the workspace is now part of the
`InstanceKey` and carried as a root, so there is no mutable per-server cwd to thrash.

### 4.5 Proactive turn-start (re)start (#557 Finding 2)

Turn-start alignment **(re)starts the resolved instance if it is not currently
`Running`**, even when nothing about its key changed. This is the one place that revives
a codegraph parked in terminal `Failed` (`failures >= max_failures` sets
`next_retry_at = None`; `on_tick` never retries `Failed`). The old `set_server_cwd`
early-returned on unchanged cwd *before* any restart, so a same-workspace next turn could
never rescue a dead instance (#557 Finding 2). The new align is: "ensure the resolved
`InstanceKey` exists and is `Running` for this turn."

This is why **Option C is dropped** (§11.3): C's guard *drops* codegraph tools more often
(whenever cwd != session_root or it is restarting), making the "unknown tool" experience
*more* frequent — the opposite of what we want. Proactive restart fixes the same race by
*healing* the instance, not by hiding it.

### 4.6 Bridge routing under concurrency

The model-facing tool name stays stable: `mcp__<id>__<tool>` (`bridge.rs`
`namespaced_name`). What changes is binding: a bridged tool built for a turn is bound to
that turn's resolved `InstanceKey`, so a `mcp__codegraph__context` call in turn A routes
to `InstanceKey{codegraph, Workspace(/A)}` and the same-named call in concurrent turn B
routes to `Workspace(/B)`. The per-turn `ToolRegistry` (`state.rs` `build_tool_registry`)
is already built per turn, so the binding is naturally per-turn — we thread the resolved
`session.workspace` into the bridge build.

## 5. Worked example — codegraph end to end

After C1-C3, with Codon active in a session on `/Users/me/projA`:

1. `resolve_mcp_servers(session)` -> global file (say `{github}`) overlaid by Codon's
   phenotype tier (`{codegraph @ scope:workspace, command:"codegraph", args:["serve","--mcp"]}`)
   -> effective `{github (global), codegraph (workspace)}`.
2. Desired set: `InstanceKey{github, Global}` (always-on) and
   `InstanceKey{codegraph, Workspace(/Users/me/projA)}` (ref by this session).
3. Supervisor ensures both are `Running`; codegraph connects with rootUri
   `file:///Users/me/projA`, attaches to that checkout's index.
4. A second pane opens Codon on `/Users/me/projB` -> a *second* codegraph instance,
   `Workspace(/projB)`. The two never collide.
5. A third pane opens Codon on `/projA` again -> ref-count on `Workspace(/projA)` goes to
   2; **no second child**, no re-index.
6. Concurrent turns in panes 1 and 2 each bridge their own instance; tool calls route by
   `InstanceKey`. No corruption.
7. Pane 1 closes; `Workspace(/projA)` ref-count -> 1 (pane 3 still there); stays up. Both
   /projA panes close -> evicted.

## 6. Decoupling codegraph from the global seed (the #573 win)

Today `seed_codegraph_mcp_entry_if_absent` writes a disabled `codegraph` into the global
`mcp.json` on first run. With the phenotype tier, **codegraph's definition moves into the
codon phenotype** (`docs/examples/codon/phenos/codon.toml` gains an `mcp_servers` entry,
`scope: workspace`, no `--path`). Consequences:

- The dependency is explicit and local to the phenotype that needs it (#573 Problem 2).
- A non-codon phenotype never spawns codegraph.
- The global seed retires; an upgrade migration removes a *disabled, unmodified* seeded
  entry (never a user-edited one — same write-if-absent caution as the seed).

This is delivered in C3, not the RFC.

## 7. Migration & back-compat

- **`scope` absent** in `mcp.json` -> `Global` -> identical to today.
- **No phenotype `mcp_servers`** -> phenotype contributes nothing -> identical to today.
- **No session `mcp_servers`** -> session tier empty -> identical to today.
- **Existing global codegraph entry:** if a user has manually configured codegraph in
  their global `mcp.json` (the #573 workaround uses an absolute `command`), it keeps
  working as a global-tier entry; the codon phenotype entry overrides by id only when
  codon is active. C3's seed-retirement only removes an *unmodified disabled seed*.
- **ts-rs bindings** regenerate for `McpScope`, the extended `McpServerConfig`,
  `Phenotype`, and the enriched `McpServerStatus`.

## 8. Lifecycle summary

| Event | Behavior |
|-------|----------|
| Turn start | `resolve_mcp_servers`; ensure resolved instances exist + `Running` (proactive restart); build per-turn bridge bound to `InstanceKey`s |
| Two sessions, same workspace, workspace-scoped server | one shared instance, ref-count 2 |
| Session workspace changes | old `Workspace(key)` ref released (evict if 0), new ensured |
| Session closes | refs released; orphaned workspace instances evicted |
| Idle clean exit (codegraph) | existing health-probe reaping; reconnect on next turn's proactive start |
| Terminal `Failed` | revived by next turn's proactive restart (no longer permanent) |

## 9. Implementation plan (sequenced PRs, separate from this RFC)

| PR | Scope | Crates | Risk |
|----|-------|--------|------|
| **C1** | `McpScope` field on `McpServerConfig` (default `Global`) + bindings + round-trip. No behavior change. | `ff-core`, `ff-mcp` | low |
| **C2** | `InstanceKey` re-keying, ref-counting, roots/rootUri + retire `set_server_cwd`, proactive turn-start restart, bridge routing by key, desktop computes desired set. codegraph stays global-tier with `scope:workspace`. Fixes #557. | `ff-mcp`, `flowforge-desktop` | medium-high |
| **C3** | `Phenotype.mcp_servers` + session-tier `mcp_servers` (persisted) + `resolve_mcp_servers` + feed C2's desired set; move codegraph -> codon phenotype; retire global seed. | `ff-core`, `ff-session`, `ff-skills`, `flowforge-desktop` | medium |

Order rationale: C2 delivers the live correctness fix on the global tier alone and builds
the "desktop computes desired set + instance keys" plumbing; C3 enriches *provenance*
(tiers) on that stable base without re-touching the supervisor. C1 -> C2 -> C3 is a clean
chain; C1 may fold into C2 if a contract-only PR is not wanted.

## 10. Test strategy

- **C1:** `McpServerConfig` round-trips with/without `scope`; default is `Global`;
  bindings regen check.
- **C2:** two workspace roots -> two distinct `Workspace` instances; two sessions same
  canonical path -> one shared (ref-count); a `Failed`-parked instance is revived by a
  turn-start proactive restart; the #557 regression — concurrent turns A(`/A`)/B(`/B`)
  each route to their own child; rootUri sent on connect; a `scope:workspace` server with
  a `--path` arg warns.
- **C3:** tier-precedence table mirroring the model-resolution tests
  (`state.rs` `unbound_session_resolves_*` / `session_override_*`): global-only,
  phenotype overrides, session overrides, `disabled` suppresses; codon resolves codegraph
  at `scope:workspace`; the codegraph skill/persona drift guard (RFC for #573, PR #579)
  stays green; seed retirement removes only an unmodified disabled seed.

## 11. Resolved decisions

Five questions were open at planning; the chosen answers and rationale:

### 11.1 Lead with correctness (C2) or philosophy (C3)?
**C2 first.** #557 is a live correctness bug (cross-session corruption); the tier model
is additive. C2 also creates the desired-set plumbing C3 builds on. The user's tiered ask
lands in C3 on a stable base.

### 11.2 Eviction policy for workspace instances?
**Ref-count on session lifecycle** (evict when no live session references the path),
composed with the existing idle health-probe reaping. Rejected idle-LRU-only: it would
evict a still-referenced instance during a quiet stretch and force a re-index on the next
turn, reintroducing the thrash this RFC removes.

### 11.3 Confirm dropping #557 Option C?
**Dropped.** Per #557 Finding 2, C's guard drops codegraph tools *more* often and makes
"unknown tool" more frequent. Proactive restart (§4.5) fixes the same race by healing the
instance instead of hiding it.

### 11.4 Session-tier MCP UI surface in C3?
**Backend + IPC first**, no settings UI in C3 — mirroring how the per-session model pin
shipped backend-first (`set_session_model`) ahead of its UI. A session-MCP settings panel
is a later, FE-only follow-up.

### 11.5 Merge granularity?
**Whole-record override-by-id**, not field-level merge of `args`/`env`. Matches the model
resolver's "resolved record is coherent" rule and avoids a Frankenstein config (global
`command` + phenotype `args`). A tier that wants to tweak one field restates the record —
the same trade the model tier makes.

## 12. Non-goals

- Per-session settings UI for MCP (deferred; §11.4).
- Remote/SSE MCP transports — stdio child only, as RFC 0003.
- Changing the bridged tool naming scheme (`mcp__<id>__<tool>` stays).
- A general "merge two partial configs" facility (explicitly rejected; §11.5).
- Multi-root servers (one workspace root per workspace instance for now).

## 13. Blast radius

| Area | Touch | Risk |
|------|-------|------|
| `McpServerConfig` + `McpScope` | `ff-core` + ts-rs bindings | low (additive, defaulted) |
| Supervisor instance keying / lifecycle | `ff-mcp` `supervisor.rs`, `client.rs` (roots) | medium-high (concurrency, lifecycle) |
| Bridge routing | `ff-mcp` `bridge.rs` | medium |
| Tier resolution + session persistence | `ff-core`, `ff-session`, `flowforge-desktop` | medium |
| codegraph decoupling | `docs/examples/codon`, seed retirement | low |

Refs #557, #573, #548, #556.
RFCEOF;echo "RFC written"; wc -l docs/rfcs/0018-tiered-and-workspace-scoped-mcp.md; git -C /Users/ytonytan/projects/flowforge status --short | tee "/Users/ytonytan/.aki/.tasks/tool-toolu_bdrk_013DqJZffEGsESyJFh2EfLDx/pipe_full.log" | head