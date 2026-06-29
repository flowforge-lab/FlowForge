# 0003 — MCP Host & Supervisor

- **Status:** Proposed
- **Milestone:** M4
- **Author:** tonytan4ever
- **Depends on:** RFC 0001 (four-layer model, `mcp:` skill references, approval gate)
- **Tracking issue:** _M4: MCP Host & Supervisor_

## 1. Summary & Goals

M4 makes FlowForge a real **MCP host**. RFC 0001 reserved the `ff-mcp` crate as a
placeholder and the four-layer model (§2 of RFC 0001) named the **MCP server** as
"an external process exposing tools — runtime spawn (M4)." Skills already declare
an `mcp:` list that is *parsed but not enforced*. M4 closes that loop: FlowForge
spawns external MCP servers, supervises them, and bridges their tools into the
same `ToolRegistry` the built-in tools live in, behind the same approval gate.

This is also where the M3 epic's deferred promise of **unlimited external tools**
(issue #41, Phase 2) lands: built-in tools are compile-time and finite; MCP is the
extensibility seam that lets a user add capability without recompiling FlowForge.

The guiding goal mirrors M3's: **adding an external tool server should be as
low-friction as Claude Desktop / Cursor** — drop an entry in a JSON config, the
server comes up, its tools appear, and a failed server self-heals or surfaces a
clear error. The user never babysits a process.

Non-goals for M4 are listed in §10.

## 2. Where this sits in the Four-Layer Model

RFC 0001 §2 is unchanged; M4 only *activates* the second row:

| Layer | What it is | Crate | Lifecycle |
|-------|-----------|-------|-----------|
| **Tool** | A compiled Rust callable the model invokes | `ff-tools` | compile-time |
| **MCP server** | An external process exposing tools | `ff-mcp` | **runtime spawn (M4)** |
| **Skill** | Markdown instructions + tool/MCP references + read-only resources | `ff-skills` | runtime load / hot-reload |
| **Phenotype** | The active set of `{skills, tools, model, persona}` for a session | `ff-core` + config | per-session switch |

A skill's `mcp: [<server-id>]` becomes enforceable: activating a skill that
references a server the host has running makes that server's tools available to
the turn; referencing an absent server is a load-time warning (not a hard error,
to keep skills portable).

## 3. Configuration — `~/.flowforge/mcp.json`

> **Amended by [RFC 0018](0018-tiered-and-workspace-scoped-mcp.md) §3, §14.** The watched `mcp.json` is now the **Global tier** of a tiered desired set (global / phenotype / session), not the whole desired set. The shape and hot-reload discipline here are unchanged.

FlowForge adopts the de-facto Claude/Cursor `mcpServers` shape so existing server
definitions paste in unchanged:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/work"],
      "env": { "LOG_LEVEL": "info" },
      "disabled": false
    },
    "github": {
      "command": "github-mcp-server",
      "args": [],
      "env": { "GITHUB_TOKEN": "${env:GITHUB_TOKEN}" }
    }
  }
}
```

- **Hot-reload** reuses the `ff-skills` `notify` watcher pattern (RFC 0001 §4): a
  change to `mcp.json` diffs the desired set against the running set and
  reconciles — start new servers, stop removed ones, restart changed ones. As with
  skills, the active server set is **snapshotted at turn start** so a mid-turn
  reload never races.
- **`disabled: true`** keeps a definition without spawning it (parity with Claude
  Desktop), driving the FE enable/disable toggle (§7).
- `${env:VAR}` interpolation pulls secrets from the process environment rather than
  storing them in the config; keychain-backed secrets follow the rules being
  established for provider keys (issue #8) — see §9.

## 4. Client — `ff-mcp`

`ff-mcp` is a stub today (`//! Placeholder crate — implemented in a later
milestone.`). M4 makes it a real MCP **client** speaking JSON-RPC 2.0 over a
child process's stdio:

- `initialize` — handshake + capability/`serverInfo` exchange.
- `list_tools` — enumerate the server's tools (name, description, JSON-Schema
  input) → `McpToolInfo`.
- `call_tool` — invoke a tool by name with a JSON argument object; stream/collect
  the result content.
- Notifications (`tools/list_changed`) trigger a re-enumeration.

### Key decision — client implementation

> **This is the one load-bearing choice that binds M4.0.** Ratify before coding.

- **Recommended: `rmcp`, the official Rust MCP SDK.** Spec-tracked, gives us the
  JSON-RPC framing, transport, and typed `initialize`/`tools/list`/`tools/call`
  for free, and keeps us aligned as the protocol evolves (SSE/HTTP, auth — §10).
  Cost: an external dependency surface and its async-runtime assumptions.
- **Alternative: hand-rolled minimal JSON-RPC over stdio.** ~200 lines, zero new
  deps, total control of the framing and our `Safety`/jail integration. Cost: we
  re-implement and then maintain protocol surface that `rmcp` would track for us;
  higher risk of subtle spec drift.

The recommendation is **`rmcp`** — protocol-tracking outweighs the dependency for a
spec that is still moving. This is flagged, not yet ratified; M4.0 starts once the
choice is confirmed.

### `ff-core` types (with `ts-rs` bindings)

```rust
pub struct McpServerConfig {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub disabled: bool,
}

pub enum McpServerState {
    Starting,
    Running,
    Restarting,
    Failed,   // carries last error in McpServerStatus
    Disabled,
}

pub struct McpServerStatus {
    pub id: String,
    pub state: McpServerState,
    pub tool_count: usize,
    pub last_error: Option<String>,
    pub restarts: u32,
    pub pid: Option<u32>,
}

pub struct McpToolInfo {
    pub server: String,
    pub name: String,        // bare tool name as the server reports it
    pub description: String,
    pub input_schema: serde_json::Value,
}
```

## 5. Supervisor

> **Amended by [RFC 0018](0018-tiered-and-workspace-scoped-mcp.md) §4.2, §14.** Supervisor handles are keyed by `(id, ScopeKey)` rather than `server_id` alone, so a workspace-scoped server runs one instance per distinct workspace root. Global-scoped servers keep the one-instance-per-id semantics described here; the spawn / health / backoff / graceful-shutdown rules below are unchanged and reused by ref-count eviction.

The supervisor owns process lifecycle so the rest of the app only ever sees
`McpServerStatus`:

- **Spawn** each enabled server as a child process with an **isolated env** — only
  the keys in its `env` map plus an explicit allowlist, never the full inherited
  environment (§9).
- **Health** — a server is healthy once `initialize` succeeds and it answers
  `list_tools`; liveness is the child still running + responsive to a periodic
  lightweight request.
- **Auto-restart with backoff** — on crash/exit, restart with exponential backoff
  (capped); after N consecutive failures the server parks in `Failed` with the
  last error surfaced to the UI rather than thrashing.
- **Env isolation** — see §9.
- **Graceful shutdown** — on app exit (and on config-driven stop) send the MCP
  shutdown handshake, then SIGTERM, then SIGKILL after a grace period, so no
  zombie/orphan processes survive FlowForge (§9, risk 1).

## 6. Tool Bridge — into the existing `ToolRegistry`

MCP tools become first-class `ff-tools` citizens so the agent loop, approval gate,
and tool-step UI need **no special-casing**:

- On a server reaching `Running`, its tools are **dynamically registered** into the
  `ToolRegistry` under a namespaced id **`mcp__<server>__<tool>`** (double-underscore,
  matching the Claude convention) to prevent collisions with built-ins and
  across servers. On stop/failure they are **unregistered**.
- Each bridged tool defaults to **`Safety::Write`** → it is **approval-gated**,
  reusing the exact M2 approval round-trip and tool-step UI that built-in tools and
  skill installs already use (RFC 0001 §5, §9). External code touching the user's
  machine is never auto-run.
- The JSON-Schema `input_schema` from `list_tools` is carried straight onto the
  registered tool so the model gets accurate argument typing.
- A skill's `mcp:` reference (RFC 0001 §3) gates *visibility*: tools from a
  referenced-and-running server are offered to that skill's turns.

## 7. Frontend — Server-Status Panel

The reworded M4 roadmap line ("server-status UI") delivers, in settings:

- A **server list** with live state badges (running / failed / restarting /
  disabled), each server's **tool count**, and its **last error** when failed.
- Per-server actions: **manual restart**, **enable/disable** (writes `disabled`
  back to `mcp.json`), and **add/remove** a server definition.
- ⌘K visibility: servers and their bridged tools are discoverable from the palette,
  consistent with how skills surface (RFC 0001 §6).
- Status flows over the existing IPC seam (`apps/desktop/src/lib/ipc.ts`) with
  ts-rs-generated DTOs; the mock backend gets parity so pure-FE work needs no Rust.

## 8. PR Breakdown & Acceptance Criteria

Each PR: `cargo fmt --check` + `clippy -D warnings` + `cargo test --workspace`,
plus `pnpm typecheck && lint && format:check && build`, and regenerated `ts-rs`
bindings — all green before merge. One squashed commit per PR (CONTRIBUTING).

| PR | Scope | Acceptance |
|----|-------|-----------|
| **M4.0** | `ff-mcp` stdio JSON-RPC client (`initialize` / `list_tools` / `call_tool`) + `ff-core` `McpServerConfig` / `McpServerState` / `McpServerStatus` / `McpToolInfo` + bindings | client handshakes a reference server, lists + calls a tool; types compile + export; serde round-trip tests |
| **M4.1** | `mcp.json` loader + validation + `notify` hot-reload reconcile | parses valid/invalid configs; add/remove/change diffs reconcile; hot-reload test |
| **M4.2** | Supervisor: lifecycle, health, auto-restart + backoff, env isolation, graceful shutdown | crash auto-restarts with backoff; N failures parks in `Failed`; no orphan processes after shutdown (test) |
| **M4.3** | Tool bridge: dynamic register/unregister, `mcp__<server>__<tool>` namespacing, `Safety::Write` approval-gated | running server's tools appear in registry + fire approval on call; stop unregisters |
| **M4.4** | FE server-status panel: list + state + tool counts + last error + restart/enable/disable + add/remove + ⌘K, mock parity | panel renders live status; actions round-trip; mock backend parity |

## 9. Security Model

External processes are a larger trust surface than in-process tools, so:

1. **No zombie/orphan processes.** Graceful shutdown handshake → SIGTERM → SIGKILL
   after grace, on both app exit and config-driven stop; the supervisor tracks
   every child PID (§5).
2. **Env isolation.** A server receives only its declared `env` keys plus an
   explicit allowlist — never FlowForge's full inherited environment. This prevents
   a third-party server from harvesting unrelated secrets from the host env.
3. **Secrets via keychain, not config.** `${env:VAR}` keeps tokens out of
   `mcp.json`; first-class secret storage mirrors the keychain rules being set for
   LLM provider keys (issue #8) rather than inventing a parallel scheme.
4. **Approval gate on every MCP tool call.** Bridged tools default to
   `Safety::Write` and route through the M2 approver (§6); the user sees the server,
   tool, and arguments before anything runs.
5. **Validate before trust.** `initialize` + `list_tools` must succeed before any
   tool is registered; a server that fails the handshake never exposes tools.

### Risks / watch-items

1. **Orphan processes** — supervisor must reap on every exit path incl. panic /
   app crash; covered by §5 graceful shutdown + a shutdown test.
2. **Secret leakage via env** — env isolation (§9.2) + keychain (§9.3).
3. **Config hot-reload race** — snapshot the running server set at turn start, same
   discipline as skills (RFC 0001 §4).
4. **Restart thrash** — capped exponential backoff + park-in-`Failed` (§5).
5. **Tool-name collisions** — `mcp__<server>__<tool>` namespacing (§6).

## 10. Open Questions & Non-Goals

**Non-goals (M4):**
- **SSE / streamable-HTTP transports** — stdio only in v1; remote transports are a
  follow-on once the host/supervisor shape is proven.
- **Remote-server authentication** (OAuth / bearer flows) — deferred with SSE/HTTP.
- **Per-tool Safety overrides** — every bridged tool is `Safety::Write` in v1; a
  manifest- or config-driven downgrade to `ReadOnly` for known-safe tools is future
  work.
- **Autonomous skill-evolution trigger** — still M-later (RFC 0001 §8.1); unrelated
  to MCP but commonly conflated.

**Open questions:**
- Client implementation: **`rmcp` vs hand-rolled** (§4) — the one decision to
  ratify before M4.0.
- Whether `disabled` servers should still appear in ⌘K (proposed: yes, greyed).
- Default backoff schedule + failure cap (proposed: 1s→30s, park after 5).
