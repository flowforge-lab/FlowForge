# Research Findings: Agent Client Protocol (ACP)

**Date:** 2026-07-31
**Source Org:** https://github.com/agentclientprotocol
**Researcher:** OpenCode

---

## 1. What is the Agent Client Protocol (ACP)?

The **Agent Client Protocol (ACP)** is an open protocol that standardizes communication between *code editors* (interactive programs for viewing and editing source code) and *coding agents* (programs that use generative AI to autonomously modify code) [^org-readme] [^main-repo-readme]. It is designed to solve the tight coupling between AI coding agents and editors, allowing any ACP-compatible agent to work with any ACP-compatible editor without custom per-editor integrations [^website-intro].

The protocol is analogous to the Language Server Protocol (LSP), but for agent-editor communication rather than language-server integration [^website-intro].

**Key property:** Agents that implement ACP work with any compatible editor, and editors that support ACP gain access to the entire ecosystem of ACP-compatible agents [^website-intro].

---

## 2. Purpose and Problem Statement

ACP was created to address three specific problems in the AI coding tool ecosystem [^website-intro]:

1. **Integration overhead:** Every new agent-editor combination previously required custom engineering work.
2. **Limited compatibility:** Agents worked with only a subset of available editors.
3. **Developer lock-in:** Choosing an agent often meant accepting its available interfaces.

By providing a standardized protocol, ACP allows both sides (agents and editors) to innovate independently while giving developers freedom to choose the best tools for their workflow [^website-intro].

---

## 3. Architecture and Communication Model

### 3.1 Core Roles

The protocol defines two primary roles [^protocol-overview]:

- **Agent:** A program that uses generative AI to autonomously modify code. Agents typically run as subprocesses of the Client.
- **Client:** The interface between users and agents. Clients are typically code editors (IDEs, text editors) but can also be other UIs. Clients manage the environment, handle user interactions, and control access to resources.

### 3.2 Transport and Framing

ACP follows the **JSON-RPC 2.0** specification for all messages [^protocol-overview]. Communication can occur over multiple transports:

- **Local agents:** Run as sub-processes of the code editor, communicating via JSON-RPC over **stdio** [^website-intro] [^architecture-doc].
- **Remote agents:** Can be hosted in the cloud or on separate infrastructure, communicating over **HTTP** or **WebSocket** [^website-intro] [^rust-sdk-readme].

> **Note:** Full support for remote agents is described as a work in progress, with active collaboration with agentic platforms to address cloud-hosted requirements [^website-intro].

### 3.3 Message Types

The protocol uses two JSON-RPC message types [^protocol-overview]:

- **Methods:** Request-response pairs that expect a `result` or `error`.
- **Notifications:** One-way messages that do not expect a response.

ACP makes heavy use of JSON-RPC notifications to allow the agent to stream real-time updates to the client UI, and uses bidirectional requests so the agent can make requests of the code editor (e.g., requesting permissions for a tool call) [^architecture-doc].

### 3.4 Typical Message Flow

A standard interaction follows this lifecycle [^protocol-overview]:

1. **Initialization Phase**
   - Client → Agent: `initialize` (negotiate protocol version and capabilities)
   - Client → Agent: `authenticate` (if required by the Agent)

2. **Session Setup**
   - Client → Agent: `session/new` (create a new session)
   - OR Client → Agent: `session/load` (resume an existing session, if supported)

3. **Prompt Turn**
   - Client → Agent: `session/prompt` (send user message)
   - Agent → Client: `session/update` notifications (progress updates, message chunks, tool calls, plans)
   - Agent → Client: File operations or permission requests as needed
   - Client → Agent: `session/cancel` (to interrupt processing if needed)
   - Turn ends when the Agent sends the `session/prompt` response with a `stopReason`

Each connection can support several concurrent sessions [^architecture-doc].

---

## 4. Key Protocol Specifications and Standards

### 4.1 Protocol Versions

- **Current stable ACP protocol version:** `1` [^main-repo-readme].
- **Experimental draft:** ACP v2 is available in draft form. Its wire protocol and APIs may change incompatibly in any SDK release [^ts-sdk-readme].

Version negotiation happens during `initialize` via the `protocolVersion` field. The version is a single integer representing a **MAJOR** protocol version, only incremented for breaking changes [^initialization-doc].

### 4.2 Capabilities

Capabilities describe optional features supported by the Client and Agent. All capabilities in `initialize` are optional, and implementations must treat omitted capabilities as unsupported [^initialization-doc].

**Client Capabilities** include [^initialization-doc]:
- `fs.readTextFile` / `fs.writeTextFile` — File system access
- `terminal` — Terminal creation and management
- `elicitation` — Structured user input modes (form, URL)
- `session.configOptions.boolean` — Boolean session configuration options

**Agent Capabilities** include [^initialization-doc]:
- `loadSession` — Support for `session/load`
- `promptCapabilities` — Support for `image`, `audio`, `embeddedContext` in prompts
- `auth` — Authentication-related capabilities (e.g., `logout`)
- `mcpCapabilities` — Support for connecting to MCP servers over HTTP/SSE
- `delete` — Support for `session/delete`
- `additionalDirectories` — Support for additional workspace roots

### 4.3 Content Blocks

Content blocks represent displayable information in ACP and are compatible with the **Model Context Protocol (MCP)** where possible [^schema-json]. Supported block types include [^protocol-overview] [^schema-json]:

- `text` — Plain text or Markdown (all agents MUST support this)
- `image` — Base64-encoded images
- `audio` — Base64-encoded audio
- `resource_link` — References to accessible resources
- `resource` — Complete embedded resource contents (text or binary)

The default format for user-readable text is **Markdown** [^website-intro].

### 4.4 Tool Calls

Agents report tool execution via `ToolCallUpdate` objects. Tool calls progress through statuses: `pending`, `in_progress`, `completed`, `failed` [^schema-json].

Tool kinds categorize operations for UI rendering: `read`, `edit`, `delete`, `move`, `search`, `execute`, `think`, `fetch`, `switch_mode`, `other` [^schema-json].

Tool call content can include [^schema-json]:
- Standard content blocks
- File diffs (`diff` type)
- Embedded terminals (`terminal` type)

### 4.5 Sessions

Sessions maintain independent context, conversation history, and state [^schema-json]. Key session methods include [^protocol-overview]:

- `session/new` — Create a new session
- `session/load` — Resume an existing session (optional)
- `session/prompt` — Send a user prompt
- `session/cancel` — Cancel an ongoing operation (notification)
- `session/set_mode` — Switch agent operating modes (optional)
- `session/delete` — Remove a session from history (optional)
- `session/list` — Discover existing sessions (optional)

### 4.6 Extensibility

ACP provides built-in extensibility mechanisms [^protocol-overview]:

- **`_meta` fields:** Reserved for attaching additional metadata without breaking compatibility.
- **Custom methods:** Prefixed with underscore (`_`) to avoid collisions.
- **Custom capabilities:** Advertised during initialization via `_meta`.

### 4.7 JSON Schema

The canonical schema is published as JSON Schema files in the main repository under `schema/v1/` and `schema/v2/` [^main-repo-readme]. The v1 schema is approximately 5,700+ lines and formally defines every request, response, notification, and type [^schema-json]. Generated schema artifacts are attached to GitHub releases (`schema-v*`) [^main-repo-readme].

---

## 5. Ecosystem: SDKs, Adapters, and Registry

### 5.1 Official SDKs

The `agentclientprotocol` organization maintains official SDKs in multiple languages [^org-readme] [^main-repo-readme]:

| Language | Package / Repo | Notes |
|----------|----------------|-------|
| **Rust** | [`agent-client-protocol`](https://crates.io/crates/agent-client-protocol) (runtime) + [`agent-client-protocol-schema`](https://crates.io/crates/agent-client-protocol-schema) (types) | [rust-sdk repo](https://github.com/agentclientprotocol/rust-sdk) [^rust-sdk-readme] |
| **TypeScript** | [`@agentclientprotocol/sdk`](https://www.npmjs.com/package/@agentclientprotocol/sdk) | [typescript-sdk repo](https://github.com/agentclientprotocol/typescript-sdk) [^ts-sdk-readme] |
| **Python** | [`agent-client-protocol`](https://pypi.org/project/agent-client-protocol/) | [python-sdk repo](https://github.com/agentclientprotocol/python-sdk) [^python-sdk-readme] |
| **Kotlin** | `com.agentclientprotocol:acp` | [kotlin-sdk repo](https://github.com/agentclientprotocol/kotlin-sdk) [^kotlin-sdk-readme] |
| **Java** | — | [java-sdk repo](https://github.com/agentclientprotocol/java-sdk) |

**SDK Features (common across languages):**
- Generated typed models tracking the upstream ACP schema.
- Async runtime support (e.g., asyncio in Python, async/await in TS, async Rust).
- stdio JSON-RPC plumbing and lifecycle helpers.
- Content-block and tool-call builders.

### 5.2 Agent Adapters

The organization also maintains reference adapter implementations that bridge popular AI agents to ACP:

- **codex-acp:** An ACP server implementation that exposes OpenAI Codex CLI functionality [^codex-acp-readme].
  - Supports ChatGPT/API key auth, model configuration, text prompts, images, resource links, slash commands, MCP servers, and subagent launches.
  - Published as `@agentclientprotocol/codex-acp` on npm.

- **claude-agent-acp:** An ACP adapter for the Claude Agent SDK [^claude-agent-acp-readme].
  - Supports context @-mentions, images, tool calls with permission requests, edit review, TODO lists, nested subagent transcripts, interactive terminals, custom slash commands, and client MCP servers.
  - Published as `@agentclientprotocol/claude-agent-acp` on npm.

### 5.3 ACP Registry

The [`registry`](https://github.com/agentclientprotocol/registry) repository maintains a curated list of agents implementing ACP [^registry-readme].

- **Requirement:** All registered agents must support user authentication and are CI-verified to return valid `authMethods` in the ACP handshake.
- **Registry index:** `https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json`
- **Auto-updates:** Agent versions are updated hourly via a cron job checking npm, PyPI, and GitHub releases.
- Contains entries for dozens of agents including Codex, Claude, Gemini, GitHub Copilot, Cursor, Devin, Goose, Kimi, and many others [^registry-readme].

---

## 6. Governance and Contribution Model

### 6.1 License

All core ACP repositories are licensed under the **Apache License 2.0** [^main-repo-readme] [^python-sdk-readme] [^registry-readme]. The project does **not** require a Contributor License Agreement (CLA); contributions are accepted under the Apache 2.0 terms [^contributing-doc].

### 6.2 Change Process (RFDs)

Significant changes to the protocol follow an **RFD (Request for Dialog)** process [^contributing-doc] [^llms-txt]. Before implementing major changes, contributors open an RFD to gather feedback and ensure alignment with project goals [^contributing-doc].

Stable features are introduced behind `unstable` feature flags in the Rust crate before stabilization [^contributing-doc].

### 6.3 Community

- **Zulip Chat:** `https://agentclientprotocol.zulipchat.com/` [^python-sdk-readme] [^contributing-doc]
- **GitHub Discussions:** Used for protocol suggestions and questions.
- **Governance model:** Documented at `https://agentclientprotocol.com/community/governance` [^governance-doc].

### 6.4 Development Standards

- **Rust:** `rustfmt` and `clippy` enforced in CI.
- **Docs/Schema:** `prettier` for formatting.
- **Testing:** `cargo test` for Rust; pytest for Python.
- **Schema generation:** The JSON Schema files in `/schema` are generated from Rust source code via `npm run generate` [^contributing-doc].

---

## 7. Relationship to MCP

ACP is closely related to the **Model Context Protocol (MCP)** but serves a distinct purpose [^architecture-doc]:

- **MCP** standardizes how models access tools and context.
- **ACP** standardizes how code editors and coding agents communicate.

ACP re-uses MCP JSON representations where possible (e.g., content blocks, resource embeddings) to avoid redundant representations [^website-intro] [^architecture-doc]. Clients can provide MCP server configurations to agents during session setup, and agents may connect directly to those MCP servers [^architecture-doc].

The Rust SDK also includes an `agent-client-protocol-rmcp` crate for integration with the `rmcp` MCP SDK [^rust-sdk-readme].

---

## 8. Sub-repositories and Org Structure

The `agentclientprotocol` GitHub organization contains **14 public repositories** as of this research [^org-page]:

1. **`agent-client-protocol`** — Canonical schema, Rust types, docs, and protocol spec (3.8k stars).
2. **`rust-sdk`** — Official Rust SDK with runtime, HTTP/WebSocket transports, conductor, cookbook, and trace viewer.
3. **`typescript-sdk`** — Official TypeScript SDK (`@agentclientprotocol/sdk`).
4. **`python-sdk`** — Official Python SDK with Pydantic models and asyncio transports.
5. **`kotlin-sdk`** — Official Kotlin SDK (JVM target; JS/Native/Wasm planned).
6. **`java-sdk`** — Official Java SDK.
7. **`registry`** — Curated registry of ACP-compatible agents.
8. **`codex-acp`** — ACP adapter for OpenAI Codex CLI.
9. **`claude-agent-acp`** — ACP adapter for Claude Agent SDK.
10. **`meetings`** — Meeting notes from ACP group members.

(Plus additional repos not detailed in this summary.)

---

## 9. Versioning Nuances

The project uses **two separate versioning concepts** [^main-repo-readme]:

1. **Artifact versions** (Rust crate version, JSON Schema release version) — Describe the crate/schema artifacts themselves and follow semantic compatibility for downstream code generators.
2. **ACP wire protocol version** (`protocolVersion`) — Negotiated during `initialize`; determines actual message compatibility between clients and agents.

This means two JSON Schema releases can describe the same wire-compatible protocol version while having different schema structure for SDK generators [^main-repo-readme].

---

## 10. Key Takeaways

- **ACP is a JSON-RPC 2.0 protocol** standardizing editor-to-agent communication for AI coding assistants.
- **Stable version is v1**; v2 is in active draft.
- **Ecosystem is broad:** Official SDKs for Rust, TypeScript, Python, Kotlin, and Java; adapters for Codex and Claude; a public registry of 40+ agents.
- **MCP-friendly** but distinct: ACP sits between the editor and the agent, while MCP sits between the agent and its tools.
- **Governed openly** under Apache 2.0 with an RFD process, Zulip community, and no CLA requirement.
- **Remote transports** (HTTP/WebSocket) are a work in progress, with a dedicated Transports Working Group.

---

## Inline Citations

[^org-readme]: `https://github.com/agentclientprotocol` — Organization profile README. States: "The Agent Client Protocol (ACP) standardizes communication between code editors (interactive programs for viewing and editing source code) and coding agents (programs that use generative AI to autonomously modify code)."

[^main-repo-readme]: `https://github.com/agentclientprotocol/agent-client-protocol/blob/main/README.md` — Main repository README. Details protocol version, crate/schema artifacts, versioning policy, and integrations.

[^website-intro]: `https://agentclientprotocol.com/` — Official website introduction. Explains the problem ACP solves (integration overhead, limited compatibility, developer lock-in), local vs remote agent support, and Markdown as the default text format.

[^protocol-overview]: `https://agentclientprotocol.com/protocol/v1/overview.md` — Protocol v1 Overview. Defines Agents, Clients, message flow (initialize → authenticate → session/new → prompt turn), methods, notifications, and extensibility.

[^architecture-doc]: `https://agentclientprotocol.com/get-started/architecture.md` — Architecture page. Describes MCP-friendly design, UX-first principles, stdio setup, concurrent sessions, JSON-RPC streaming, and MCP server/proxy patterns.

[^initialization-doc]: `https://agentclientprotocol.com/protocol/v1/initialization.md` — Initialization documentation. Covers protocol version negotiation, client capabilities (fs, terminal, elicitation, boolean config), agent capabilities (loadSession, promptCapabilities, auth, MCP), and implementation info.

[^schema-json]: `https://github.com/agentclientprotocol/agent-client-protocol/blob/main/schema/v1/schema.json` — Canonical JSON Schema v1 (~5,749 lines). Defines all requests, responses, notifications, content blocks, tool calls, sessions, and types.

[^rust-sdk-readme]: `https://github.com/agentclientprotocol/rust-sdk/blob/main/README.md` — Rust SDK README. Lists crates (agent-client-protocol, HTTP, rmcp, derive, conductor, polyfill, trace-viewer, cookbook, test, yopo), documentation links, and integration details.

[^ts-sdk-readme]: `https://github.com/agentclientprotocol/typescript-sdk/blob/main/README.md` — TypeScript SDK README. Describes installation, experimental v2 support, agent/client builder APIs, and links to examples and production implementations (Gemini CLI).

[^python-sdk-readme]: `https://github.com/agentclientprotocol/python-sdk/blob/main/README.md` — Python SDK README. Describes Pydantic models, asyncio transports, helper builders, examples, and community channels.

[^kotlin-sdk-readme]: `https://github.com/agentclientprotocol/kotlin-sdk/blob/master/README.md` — Kotlin SDK README. Describes modules (acp-model, acp, acp-ktor, acp-ktor-client, acp-ktor-server, acp-ktor-test), architecture diagram, and sample projects.

[^codex-acp-readme]: `https://github.com/agentclientprotocol/codex-acp/blob/main/README.md` — Codex ACP adapter README. Describes features, authentication methods, runtime options, and development commands.

[^claude-agent-acp-readme]: `https://github.com/agentclientprotocol/claude-agent-acp/blob/main/README.md` — Claude Agent ACP adapter README. Describes supported features including nested subagent transcripts, tool calls, terminals, slash commands, and client MCP servers.

[^registry-readme]: `https://github.com/agentclientprotocol/registry/blob/main/README.md` — Registry README. Defines the registry's purpose, authentication requirement, registry index URL, automatic version updates, and agent list.

[^contributing-doc]: `https://github.com/agentclientprotocol/agent-client-protocol/blob/main/CONTRIBUTING.md` — Contributing guide. Describes ways to contribute, coding standards (rustfmt, clippy, prettier), RFD process, pull request process, and community channels (Zulip).

[^governance-doc]: `https://github.com/agentclientprotocol/agent-client-protocol/blob/main/GOVERNANCE.md` — Governance file. Points to `https://agentclientprotocol.com/community/governance`.

[^llms-txt]: `https://agentclientprotocol.com/llms.txt` — Complete documentation index. Lists all announcements, protocol docs, RFDs, and library pages.

[^org-page]: `https://github.com/agentclientprotocol` — Organization page showing 14 repositories, pinned repos, and follower count (531).

---

## 11. FlowForge Integration Analysis (Q1–Q4)

**Date:** 2026-08-03
**Analyst:** OpenCode
**Schema source:** `https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/main/schema/v1/schema.json` (5,749 lines, fetched 2026-08-03)

---

### Q1 — Permission round-trip → PermissionMatrix

#### What ACP defines

The permission mechanism in ACP v1 is a single JSON-RPC request/response pair:

- **Method name:** `session/request_permission` — sent by the **Agent** to the **Client** [schema.json line 378].
- **Request payload (`RequestPermissionRequest`):**
  - `sessionId`: string
  - `toolCall`: `ToolCallUpdate` object (contains `toolCallId`, `kind`, `status`, `title`, `content`, `locations`, `rawInput`, `rawOutput`) [schema.json lines 342–379]
  - `options`: array of `PermissionOption` [schema.json line 363]
- **Response payload (`RequestPermissionResponse`):**
  - `outcome`: `RequestPermissionOutcome` [schema.json lines 5295–5316]

The `PermissionOption` shape is [schema.json lines 1089–1120]:
- `optionId`: string (opaque ID)
- `name`: human-readable label
- `kind`: `PermissionOptionKind` — one of `allow_once`, `allow_always`, `reject_once`, `reject_always` [schema.json lines 1122–1138]

The response `RequestPermissionOutcome` is a tagged union [schema.json lines 5318–5351]:
- `{ "outcome": "cancelled" }` — when the client sent `session/cancel`
- `{ "outcome": "selected", "optionId": "..." }` — user picked one of the options

#### What ACP does **not** define

- **No safety tier.** The `ToolCallUpdate` carries a `kind` enum (`read`, `edit`, `delete`, `move`, `search`, `execute`, `think`, `fetch`, `switch_mode`, `other`) [schema.json lines 460–513], but this is purely a UI hint ("helps clients choose appropriate icons"). It does **not** drive any permission semantics.
- **No permission matrix.** There is no concept of a 2-D or N-D grid mapping mode × safety → decision.
- **No `Deny` equivalent.** ACP has no mechanism to hide a tool from the model entirely. The protocol assumes the agent already decided to invoke the tool and is now asking the user for approval. The worst outcome is `reject_always` for that specific call.

#### Expressiveness gap against FlowForge

FlowForge's [`PermissionMatrix`](crates/ff-core/src/permission.rs:201) resolves `Mode` {Plan, Act, Auto} × `Safety` {ReadOnly, Write, Sensitive, Dangerous, Publish} → `PermissionCell` {Allow, Ask, Deny} — 15 distinct cells. FlowForge's [`Deny`](crates/ff-core/src/permission.rs:51) means the tool is **hidden from the model entirely** (the tool is not advertised in the registry), not "prompt and refuse."

ACP can express **at most 4 of FlowForge's 15 cells**, and even then only at the granularity of a single tool call:

| FlowForge cell | ACP equivalent |
|---|---|
| `Allow` | `allow_once` / `allow_always` |
| `Ask` | The entire `session/request_permission` round-trip |
| `Deny` | **None** — ACP has no pre-call filtering concept |

**Conclusion:** ACP's permission model is an opaque per-call "approve this?" with four answer variants. It cannot represent FlowForge's multi-dimensional matrix, and it has no equivalent to `Deny` (tool hidden from model). If FlowForge advertises an ACP `session/request_permission` capability, it would need to map its internal `Ask` cells to ACP permission requests, while its `Deny` cells would simply be implemented by *not advertising* the tool to the ACP agent — a behavior outside the ACP spec.

---

### Q2 — Filesystem capability vs ff-tools

#### ACP filesystem methods

ACP v1 defines **exactly two** filesystem methods, both served by the **Client** (the editor/IDE, e.g. Zed) [schema.json lines 266–340]:

| Method | Direction | Capability gate |
|---|---|---|
| `fs/read_text_file` | Agent → Client | `fs.readTextFile` |
| `fs/write_text_file` | Agent → Client | `fs.writeTextFile` |

There is **no** `fs/readDirectory`, `fs/glob`, `fs/stat`, or any other filesystem method in the v1 schema. The `FileSystemCapabilities` object only has `readTextFile` and `writeTextFile` booleans [schema.json lines 4457–4472].

#### When FlowForge acts as an ACP agent (Zed calls us)

In this direction, Zed is the Client and FlowForge is the Agent. If Zed advertises `fs.readTextFile: true`, the FlowForge agent **may** call `fs/read_text_file` on Zed instead of using its own `view` tool. This means:

- FlowForge's built-in `view`/`edit`/`write` tools (in `ff-tools`) could be bypassed for file operations.
- The permission check for those file operations would move from FlowForge's [`PermissionMatrix::effective_cell`](crates/ff-core/src/permission.rs:322) to Zed's own UI/policy.
- However, ACP does **not** require the agent to use client filesystem methods. The agent can use its own internal tools and only access the client filesystem if it chooses to. The capability is optional on both sides.

#### Which path owns the permission check?

- **If the agent uses ACP `fs/*` methods:** The **Client** (Zed) owns the permission check. FlowForge's `PermissionMatrix` is irrelevant for that call.
- **If the agent uses its own tools:** FlowForge owns the permission check via `PermissionMatrix`.
- **There is no conflict resolution rule in ACP.** The two paths are independent. An agent could theoretically read a file via `fs/read_text_file` (client-served) and then write it via its own `write` tool (agent-served), with permission checks split across two systems.

**Open question:** Does ACP v2 add `fs/readDirectory` or similar? The v1 schema does not contain it, and the v2 draft is noted as unstable. This would be settled by reading the v2 schema or RFDs.

---

### Q3 — The asymmetry between the two directions

#### FlowForge **as agent** (Zed calls us)

FlowForge must implement the **Agent** side of the protocol — i.e., respond to client requests and send agent notifications. The v1 schema enumerates these explicitly:

**Agent requests we must respond to** [schema.json lines 122–243]:
1. `initialize`
2. `authenticate`
3. `session/new`
4. `session/load` (optional, gated by `loadSession` capability)
5. `logout` (optional, gated by `auth.logout` capability)
6. `session/set_mode` (optional)
7. `session/list` (optional, gated by `sessionCapabilities.list`)
8. `session/delete` (optional, gated by `sessionCapabilities.delete`)
9. `session/resume` (optional, gated by `sessionCapabilities.resume`)
10. `session/close` (optional, gated by `sessionCapabilities.close`)
11. `session/set_config_option` (optional, gated by `session.configOptions.boolean`)
12. `session/prompt` (the core turn handler)

**Agent notifications we must send** [schema.json lines 3502–3700]:
- `session/update` (streams progress, tool calls, message chunks, plans)
- `elicitation/complete` (for URL-based elicitation)
- `$/cancel_request` (protocol-level)

Additionally, when acting as an agent, FlowForge **may call** client methods:
- `fs/read_text_file`
- `fs/write_text_file`
- `session/request_permission`
- `terminal/create`, `terminal/output`, `terminal/release`, `terminal/wait_for_exit`, `terminal/kill`
- `elicitation/create`

#### FlowForge **as client** (we call Codex/Gemini/other ACP agents)

FlowForge must implement the **Client** side — i.e., send client requests and respond to agent requests/notifications. The v1 schema enumerates:

**Client requests we must send** [schema.json lines 4206–4405]:
- `initialize`
- `authenticate`
- `session/new`
- `session/load`
- `logout`
- `session/set_mode`
- `session/list`
- `session/delete`
- `session/resume`
- `session/close`
- `session/set_config_option`
- `session/prompt`

**Client notifications we must send** [schema.json lines 5659–5749]:
- `session/cancel`

**Client requests we must respond to** (when the agent calls us) [schema.json lines 5121–5320]:
1. `fs/read_text_file`
2. `fs/write_text_file`
3. `session/request_permission`
4. `terminal/create`
5. `terminal/output`
6. `terminal/release`
7. `terminal/wait_for_exit`
8. `terminal/kill`
9. `elicitation/create`

#### Distance between the two lists

The two method sets are **almost completely disjoint**:

- **Agent-only methods:** `initialize`, `authenticate`, `session/new`, `session/prompt`, `session/load`, `logout`, `session/set_mode`, `session/list`, `session/delete`, `session/resume`, `session/close`, `session/set_config_option`
- **Client-only methods:** `fs/read_text_file`, `fs/write_text_file`, `session/request_permission`, `terminal/*`, `elicitation/create`
- **Shared:** `session/cancel` (notification, client → agent), `session/update` (notification, agent → client)

Because the lists are disjoint and both require full JSON-RPC plumbing, the implementation naturally splits into **two tickets minimum**, but likely **three to five** when considering:
1. **Agent-side server** (handle all agent requests, emit `session/update`)
2. **Client-side caller** (send all client requests, handle all client responses)
3. **Client-side handler** (respond to `fs/*`, `terminal/*`, `elicitation/*`, `session/request_permission`)
4. **Permission mapping layer** (FlowForge `PermissionMatrix` ↔ ACP `session/request_permission`)
5. **Tool registry bridging** (decide when to use ACP client fs methods vs. FlowForge's own `ff-tools`)

Note: `ff-mcp` already has JSON-RPC plumbing (`crates/ff-mcp/`, client/supervisor, with `bridge.rs` at 242 lines bridging external tools into our registry). This plumbing can likely be reused for the JSON-RPC transport layer, but the ACP message types are different from MCP and must be handled separately.

---

### Q4 — Vendor the Rust crate, or hand-write the types?

#### Maturity signals

| Signal | Observation |
|---|---|
| **Runtime crate** | `agent-client-protocol` = `2.0.0` on crates.io [fetched 2026-08-03] |
| **Schema crate** | `agent-client-protocol-schema` = `1.6.0` on crates.io [fetched 2026-08-03] |
| **Version drift** | Runtime (2.0.0) and schema (1.6.0) are on different version tracks, suggesting the schema artifact and runtime artifact evolve independently. The README notes that "Artifact versions … follow semantic compatibility for downstream code generators" while the "ACP wire protocol version (`protocolVersion`) … determines actual message compatibility." |
| **Unstable features** | The Rust SDK README explicitly documents `unstable_mcp_over_acp`, `unstable_protocol_v2`, and draft v2 session builders. Stable features are introduced behind feature flags before stabilization. |
| **Breaking-change history** | The protocol itself is at v1 (wire-stable), but the crate has reached v2.0.0, implying at least one major breaking change in the Rust API surface since the protocol stabilized. |
| **Downstream dependents** | `cargo search` shows a small but growing ecosystem (~20+ crates mentioning ACP), including adapters like `deepseek-acp-adapter` and `acp-llm-adapter`. The core crate is the official SDK. |

#### Type surface fit

The schema JSON contains extensive custom serde attributes that the generated Rust types preserve:
- `x-deserialize-default-on-error: true`
- `x-deserialize-skip-invalid-items: true`
- `x-side`, `x-method` metadata fields
- Complex tagged unions with `discriminator` fields

FlowForge's existing serde shapes are comparatively simple (e.g., `PermissionMatrix` uses standard `#[serde(default, skip_serializing_if = ...)]`). The ACP crate types would require conversion at every boundary — especially for the permission system, where ACP's `PermissionOptionKind` (4 variants) maps poorly onto FlowForge's `PermissionCell` (3 variants) and `PermissionMatrix` (15 cells).

#### Version-pinning risk

Because the crate version is decoupled from the wire protocol version, pinning the crate does not pin the protocol. An upstream `agent-client-protocol` v2.1.0 could change generated struct field names or add new mandatory fields while still targeting ACP wire v1. This creates a maintenance burden: we would need to track both crate changelog and wire spec.

#### Recommendation: **Hand-write the types for the subset we need.**

Rationale:
1. **Control over serde boundaries.** FlowForge already has strong conventions for IPC types (`ts-rs` exports, `PermissionMatrix` custom deserialization with 4→5 column migration). Hand-writing lets us keep those conventions without fighting the crate's generated attributes.
2. **Compile-time cost.** The AGENTS.md guidelines explicitly warn against adding dependencies that trigger full desktop rebuilds. The `agent-client-protocol` crate is a large, multi-feature runtime. Even the schema crate pulls in a non-trivial type tree.
3. **Subset sufficiency.** We do not need all 5,749 lines of schema. FlowForge's ACP integration will likely start with a small surface (e.g., `initialize`, `session/new`, `session/prompt`, `session/update`, `session/request_permission`). Hand-writing ~20–30 structs is less work than vendoring a crate and writing conversion layers.
4. **Maturity uncertainty.** The version drift (2.0.0 runtime vs. 1.6.0 schema) and prominent unstable features suggest the Rust API is still settling. Hand-writing insulates us from upstream churn.
5. **Existing plumbing.** `ff-mcp` already has JSON-RPC client/supervisor plumbing. We can reuse that transport layer and only add ACP-specific message routing.

**Escape hatch:** If the ACP surface grows beyond ~50 structs, or if v2 introduces complex session lifecycle types, re-evaluate vendoring `agent-client-protocol-schema` (the types-only crate) and writing our own runtime. Do **not** vendor the full runtime crate until the version drift stabilizes and the unstable features graduate.

---

## 12. Open Questions

1. **ACP v2 filesystem methods:** Does the draft v2 protocol add `fs/readDirectory` or binary file operations? Settled by reading `schema/v2/schema.json` or the relevant RFD.
2. **Permission granularity in practice:** When a real ACP client (e.g. Zed) receives `session/request_permission`, does it implement any persistent allow-listing, or is it strictly per-call? Settled by inspecting Zed's ACP client source or testing against it.
3. **Tool call vs. MCP tool bridging:** If FlowForge acts as an ACP agent and receives a `session/prompt` that triggers an MCP tool call, should the tool execution be reported as an ACP `tool_call_update` or as a native MCP result forwarded through ACP? Settled by reading the `agent-client-protocol-rmcp` integration crate or the ACP spec's MCP-over-ACP section.
4. **Session mode mapping:** ACP has `session/set_mode` and `availableModes`. FlowForge's `Mode` is {Plan, Act, Auto}. Is there a standard ACP mode vocabulary, or is it agent-defined? Settled by reading the session-modes protocol doc.
