# FlowForge 🔗🧠

A fast, open-source AI interface built for flow-state interaction.  
FlowForge is the **open UI layer** of the NeuroForge ecosystem — keyboard-native, local-first, and designed to make AI collaboration feel like a natural extension of your thoughts.

> 🚀 Not another agent wrapper. A personal interface tuned to the biology of focus.

> 📜 All contributions follow our [Engineering Principles](./PRINCIPLES.md) — read the charter before you build.

## ⚡ Core Principles
| Principle | Implementation |
|-----------|----------------|
| **Flow-First UX** | `<200ms` cold start, `Cmd/Ctrl+K` keyboard-native invocation, zero-modal workflow |
| **Local-First Architecture** | All logs, session state, and intention signals live in SQLite. Cloud sync is strictly opt-in |
| **Failure-Friendly Design** | Abandoned tasks are auto-detected via context switches and surfaced as neutral learning signals — never guilt |
| **Intention-Aware Sessions** | Every session begins with a stated goal and ends with an outcome signal. FlowForge closes the loop so downstream systems (NeuroForge) can learn |

## 🛠️ Tech Stack
- **Runtime:** Tauri 2 (Rust backend + OS Webview)
- **Frontend:** React 19 + TypeScript + Vite
- **State & Storage:** Zustand + `tauri-plugin-sql` (SQLite)
- **AI Layer:** candle-vllm (local Rust inference, OpenAI-compatible) + Amazon Bedrock (cloud route)
- **Styling:** Tailwind CSS + shadcn/ui

## 🧠 NeuroForge Integration

FlowForge and NeuroForge are separate but complementary systems:

```
┌──────────────┐         ┌──────────────────┐
│  FlowForge   │ signals │    NeuroForge    │
│  (this repo) │────────▶│  (plugin system) │
│              │◀────────│                  │
│  UI + Agent  │ insights│  RPE models,     │
│  + Tools     │         │  flow scoring,   │
│              │         │  adaptive pacing  │
└──────────────┘         └──────────────────┘
```

- **FlowForge generates the signal** — intention→outcome pairs, session timing, context-switch events, task abandonment patterns
- **NeuroForge consumes via plugin API** — computes RPE (reward prediction error), models aMCC activation, scores flow states
- **FlowForge surfaces the insights** — inline, non-intrusive feedback calibrated to reinforce sustainable cognitive habits

FlowForge is fully functional without NeuroForge. The integration unlocks a closed-loop cognitive feedback system for users who opt in.

## 📦 Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      FlowForge Desktop                       │
│                      (Tauri 2 Shell)                         │
├─────────────────────────────────────────────────────────────┤
│  ┌───────────────────────────────────────────────────────┐  │
│  │              React Frontend (Webview)                  │  │
│  │                                                       │  │
│  │  ┌─────────────┐  ┌──────────────┐  ┌────────────┐  │  │
│  │  │  Chat View  │  │ Flow Canvas  │  │  Cmd+K Bar │  │  │
│  │  │  (streams)  │  │ (workflows)  │  │  (palette) │  │  │
│  │  └─────────────┘  └──────────────┘  └────────────┘  │  │
│  │                                                       │  │
│  │  ┌─────────────────────────────────────────────────┐  │  │
│  │  │         Zustand Store + TanStack Query          │  │  │
│  │  └─────────────────────────────────────────────────┘  │  │
│  └───────────────────────┬───────────────────────────────┘  │
│                          │ Tauri IPC (invoke/events)         │
├──────────────────────────┼──────────────────────────────────┤
│  ┌───────────────────────┴───────────────────────────────┐  │
│  │              Rust Backend (src-tauri)                  │  │
│  │                                                       │  │
│  │  ┌───────────┐  ┌───────────┐  ┌──────────────────┐  │  │
│  │  │ ff-agent  │  │  ff-llm   │  │    ff-memory     │  │  │
│  │  │ (loop &   │  │ (Bedrock, │  │ (SQLite + vector │  │  │
│  │  │  tools)   │  │  candle)  │  │  embeddings)     │  │  │
│  │  └───────────┘  └───────────┘  └──────────────────┘  │  │
│  │                                                       │  │
│  │  ┌───────────┐  ┌───────────┐  ┌──────────────────┐  │  │
│  │  │ff-signals │  │  ff-mcp   │  │   ff-skills      │  │  │
│  │  │(intention │  │ (MCP host │  │ (discovery,      │  │  │
│  │  │ + outcome)│  │  + supvr) │  │  hot-reload)     │  │  │
│  │  └───────────┘  └───────────┘  └──────────────────┘  │  │
│  └───────────────────────────────────────────────────────┘  │
├─────────────────────────────────────────────────────────────┤
│                    Local Storage Layer                        │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │   SQLite DB  │  │  ~/.flowforge│  │  Skills (MD +    │  │
│  │ (sessions,   │  │  /memory/    │  │  YAML manifests) │  │
│  │  signals)    │  │  (flat files)│  │                  │  │
│  └──────────────┘  └──────────────┘  └──────────────────┘  │
└─────────────────────────────────────────────────────────────┘
         │                    │                    │
         ▼                    ▼                    ▼
┌─────────────────┐  ┌────────────────┐  ┌────────────────────┐
│  LLM Providers  │  │  MCP Servers   │  │    NeuroForge      │
│  (Bedrock,      │  │  (stdio/SSE,   │  │  (plugin system,   │
│   candle,       │  │   external     │  │   RPE engine,      │
│   Anthropic)    │  │   tools)       │  │   flow scoring)    │
└─────────────────┘  └────────────────┘  └────────────────────┘
```

### Crate Map

| Crate | Role |
|-------|------|
| `ff-core` | Domain types — Message, Turn, Skill, Profile, Session |
| `ff-agent` | Agent loop (research → plan → implement → verify), tool dispatch |
| `ff-llm` | Provider trait + implementations (candle-vllm, Bedrock, Anthropic) |
| `ff-mcp` | MCP client & supervisor — health monitoring, auto-restart, env isolation |
| `ff-memory` | Markdown-owned durable memory (`MEMORY.md` + daily logs) + SQLite FTS5 recall; optional local embeddings + BM25 fusion ([RFC 0006](docs/rfcs/0006-memory.md)) |
| `ff-signals` | Intention/outcome event emitter — lightweight signal bus for NeuroForge integration |
| `ff-skills` | Skill discovery, YAML manifest parsing, hot-reload via filesystem watcher |
| `ff-tools` | Built-in tools: bash, edit, view, web_fetch, glob, rg |
| `ff-workflow` | DAG executor for multi-agent orchestration, retries, partial replay |

### Open-Core Model

- **Open (this repo):** The full desktop app, agent loop, tool system, skill engine, signal emitter, and local-first storage. You own your data, your workflows, your memory.
- **Open (NeuroForge SDK):** The plugin API and reference implementations for consuming FlowForge signals. Build your own cognitive feedback plugins.
- **Closed (NeuroForge Cloud — optional):** Advanced neuro-calibration models, cross-device sync, team collaboration, and hosted LLM routing with cost optimization.

The open layer is fully functional standalone — cloud features unlock convenience, never gatekeep capability.

## 🧪 Development

```bash
# Prerequisites: Rust 1.80+, Node 20+, pnpm 9+
git clone https://github.com/flowforge-lab/flowforge.git
cd flowforge

# Install frontend deps
pnpm install

# Run in development (Tauri hot-reload, real Rust backend + local LLM)
cargo tauri dev

# UI-only: run the frontend against the in-browser mock backend
# (no Rust build, no LLM required — great for pure UI/styling work)
pnpm --dir apps/desktop dev:mock

# Build for production
cargo tauri build
```

## 💻 Headless / CLI

FlowForge ships a CLI binary (`flowforge`) for scripting, CI, and headless use
— same agent loop, same tools, no GUI.

```bash
# One-shot: run a single turn and print the result
flowforge run "summarize the contents of src/"

# Non-interactive: auto-approve writes, stream JSON events to stdout
flowforge run --json --yes "add a README section for the new memory crate"

# Read-only analysis: deny all writes (safe default for CI)
flowforge run --deny "audit src/ for unused dependencies"

# Interactive REPL (default when no subcommand is given)
flowforge
```

### Exit codes

The `run` subcommand follows a scripting-friendly contract:

- **0** — the turn completed successfully.
- **non-zero** — an agent error occurred (e.g. LLM failure), or a required
  tool approval was denied (by `--deny`, by the piped-no-policy rule, or by
  answering `N` at a prompt).

The interactive REPL always exits **0** on clean shutdown (EOF / `exit`);
per-turn failures are printed inline and do not terminate the session.

### Approval in CI

When stdin is not a terminal and no `--yes` or `--deny` flag is provided, every
write/dangerous tool call is **loudly denied** rather than silently run. This
makes `--deny` the safe default for read-only CI pipelines, and `--yes` the
explicit opt-in for autonomous runs.

## 🗺️ Roadmap

- [x] Repository bootstrap & architecture definition
- [x] **M1** — Tauri 2 shell + React chat UI + first LLM call (candle-vllm)
- [x] **M2** — Tool calling (bash, view, edit) + streaming render + interactive approval
- [x] **M3** — Skills + phenotypes + command palette
- [x] **M4** — MCP host & supervisor — external tool servers (stdio/SSE), lifecycle + server-status UI
- [x] **M5** — Memory system — Markdown source-of-truth + local SQLite FTS5 recall (optional embeddings) ([RFC 0006](docs/rfcs/0006-memory.md))
- [ ] **M6** — Cold-start optimization (<200ms)
- [ ] **M7** — Workflow canvas (visual multi-agent DAGs)
- [ ] **M8** — NeuroForge integration (intention signals + inline feedback)

## 🌐 Ecosystem

| Project | Role | Status |
|---------|------|--------|
| **FlowForge** (this repo) | Open AI interface — keyboard-native, local-first | Active |
| **NeuroForge** | Cognitive health plugin system — RPE models, flow scoring, adaptive pacing | Planned |
| **NeuroForge Cloud** | Cross-device sync, team features, hosted inference, advanced models | Future |

## 📄 License

[MIT](./LICENSE) — use it, fork it, ship it.
