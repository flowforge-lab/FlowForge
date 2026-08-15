# FlowForge architecture — crate map

The crate-level index for FlowForge: which crate owns what, and how they depend
on each other. This is **layer 2** of the navigation index — read it to decide
*which crate a change belongs in* and *whether it crosses a boundary*. The other
two layers:

- **Layer 1 — symbol graph** (`codegraph_explore`): callers/callees/impact of a
  specific symbol. Use it to drill *into* a crate once this map tells you which.
- **Layer 3 — behavioural contract** (`AGENTS.md`, `CONTRIBUTING.md`): which tool
  to reach for, the local-environment traps, the tool-registration pattern, and
  the `apps/cli` module-level code map.

> **Freshness contract.** The dependency-direction block below is **generated**
> from `cargo metadata` by `scripts/gen-arch-graph.sh` — never hand-edit it. CI
> runs `scripts/gen-arch-graph.sh --check` and fails if it drifts; regenerate
> with `scripts/gen-arch-graph.sh --write`. The prose responsibility table is
> maintained by hand and should be updated in the same PR as any crate whose
> role changes; per-crate contract details live in each crate's `lib.rs` `//!`
> header so they are reviewed alongside the code (see #1268, and #1267 for the
> doc-debt this discipline exists to prevent).

## Crate responsibilities

Ordered roughly by dependency depth: foundation first, then leaf services, the
tool/agent core, and finally the host/transport edges.

### Foundation

| Crate | Owns | Boundary — does **not** |
|---|---|---|
| **ff-core** | Domain types shared across every crate and exported to TypeScript via `ts-rs`; the IPC contract with the frontend. Core: `Message`, `Session`, `Skill`, `ReasoningEffort`. | Business logic — data types only; changing one is a breaking change for the frontend. |

### Leaf services (depend only on `ff-core`, or nothing)

| Crate | Owns | Boundary |
|---|---|---|
| **ff-llm** | LLM provider abstraction behind one `Provider` trait: OpenAI-compatible SSE, Ollama-native NDJSON, and Bedrock backends. Core: `Provider`, `OpenAiProvider`, `OllamaProvider`, `BedrockProvider`. | — |
| **ff-session** | Session and message persistence backed by SQLite, mirroring `ff-memory`'s `FlushLedger`. Core: `SessionStore`, `SearchHit`, `TurnPreheat`. | — |
| **ff-memory** | Durable local-first user memory as plain user-owned Markdown under `~/.flowforge/memory/`, with embeddings, strata, and decay (RFC 0006). Core: `Memory`, `MemoryChunk`, `EmbeddingProvider`. | Not an opaque DB — memory is Markdown the user owns. |
| **ff-skills** | Skill discovery, manifest parsing, and filesystem hot-reload from `~/.flowforge/skills/<name>/SKILL.md` into a registry (M3). Core: `SkillRegistry`, `SkillManifest`, `Skill`. | — |
| **ff-signals** | Signal bus folding per-turn skill telemetry into rolling per-skill aggregates, persisted for NeuroForge (RFC 0001 §8). Core: `Signal`, `SignalStore`, `SkillAggregate`. | — |
| **ff-logging** | Shared `tracing-subscriber` install, extracted so CLI and desktop share one setup (#1118, #1060). | Installs the subscriber only — no app logic. |
| **ff-workflow** | Placeholder DAG executor for multi-agent orchestration (M7). | Not yet implemented — explicit placeholder deferred to a later milestone. |

### Tool & agent core

| Crate | Owns | Boundary |
|---|---|---|
| **ff-tools** | Built-in agent-callable tools: `bash`, `python`, `view`, `edit`, `write`, `apply_patch`, `grep`, `glob`, `tree`, `todo`, `web_fetch`, `web_search`, `ask_user`, plus the registry. Core: `ToolRegistry`, `AgentTool`. | Depends on `ff-core`, `ff-memory`, `ff-session`. |
| **ff-mcp** | MCP host: a JSON-RPC 2.0 client over child-process stdio that bridges discovered MCP server tools into FlowForge (RFC 0003, M4). Core: `McpClient`, `McpBridgedTool`, `McpConfigWatcher`. | — |
| **ff-observer** | Session-scoped observer framework whose supervisor owns background watchers that wake the agent (#891). Core: `ObserverSupervisor`, `ObserverSpec`, `ObserverSource`. | — |
| **ff-agent** | The multi-step turn loop — builds history advertising tool schemas, streams the model, drives tool calls under the approval policy. The convergence point. Core: `AgentEvent`, `ToolContext`, `Approver`, `CancelToken`. | Depends on core/llm/memory/session/skills/tools. |

### Host & transport edges (top of the graph)

| Crate | Owns | Boundary |
|---|---|---|
| **ff-scheduled** | Durable SQLite store and cron derivation for scheduled tasks, mirroring `ff-session` (RFC 0017, #539). Core: `ScheduledStore`, `TaskRunner`. | Wire types live in `ff-core`; this only persists tasks and derives cron. |
| **ff-transport** | Transport abstraction defining the `MessageTransport` trait external platforms implement (#911, RFC 0021). Core: `MessageTransport`. | Defines the abstraction, not any specific adapter. |
| **ff-transport-slack** | Slack Socket Mode adapter implementing the transport for Slack (RFC 0021 §5.1). Core: `SlackTransport`, `SlackApi`, `SlackApprover`. | Slack-specific transport only. |
| **ff-acp** | FlowForge ↔ Agent Client Protocol mapping layer. Core: `AcpError`, `AcpServerState`, `Inbound`. | Wire types come from the official `agent_client_protocol` crate, re-exported. |

### Applications / hosts

| Package | Owns | Boundary |
|---|---|---|
| **ff-cli** (`apps/cli`) | Headless CLI driving the same `ff_agent::run_turn` loop as desktop, rendering agent events to the terminal (RFC 0004). | Terminal renderer, not a webview. |
| **flowforge-desktop** (`apps/desktop/src-tauri`) | Thin Tauri shell: command/event glue that deserializes, calls into the `ff-*` crates, and streams responses out as Tauri events. | Holds no business logic — all of it lives in `ff-*`, per the SOP. |

## Dependency direction

Read top-to-bottom as "depends on". `ff-core` is the root everything shares;
`ff-agent` is the convergence point; the transport/host crates sit at the top.

<!-- BEGIN GENERATED DEP GRAPH -->
```
# crate  ->  its ff-* dependencies (workspace members, normal deps only)
# Generated by scripts/gen-arch-graph.sh — do not edit by hand.

ff-acp               -> ff-agent ff-core ff-llm ff-session ff-tools
ff-agent             -> ff-core ff-llm ff-memory ff-session ff-skills ff-tools
ff-cli               -> ff-acp ff-agent ff-core ff-llm ff-logging ff-mcp ff-memory ff-scheduled ff-session ff-skills ff-tools ff-transport ff-transport-slack
ff-core              -> (no ff-* deps — leaf/root)
ff-llm               -> ff-core
ff-logging           -> (no ff-* deps — leaf/root)
ff-mcp               -> ff-core ff-tools
ff-memory            -> (no ff-* deps — leaf/root)
ff-observer          -> ff-core ff-tools
ff-scheduled         -> ff-agent ff-core ff-tools
ff-session           -> ff-core
ff-signals           -> ff-core
ff-skills            -> ff-core
ff-tools             -> ff-core ff-memory ff-session
ff-transport         -> ff-agent ff-core ff-llm ff-session ff-tools
ff-transport-slack   -> ff-agent ff-core ff-transport
ff-workflow          -> (no ff-* deps — leaf/root)

# apps / hosts:
flowforge-desktop    -> ff-agent ff-core ff-llm ff-logging ff-mcp ff-memory ff-observer ff-scheduled ff-session ff-signals ff-skills ff-tools
```
<!-- END GENERATED DEP GRAPH -->

> Note: `ff-observer` and `ff-signals` are compiled into the desktop host but are
> not (yet) wired through the CLI; `flowforge-desktop` is the widest consumer.
