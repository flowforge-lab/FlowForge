# SOP — Rust Backend Setup & Frontend/Backend Split

**Audience:** FlowForge contributors
**Owner (backend):** Tony (ytonytan)
**Owner (frontend):** Abid
**Status:** Living document — update when the IPC contract or workspace layout changes

---

## 0. The Split — Who Owns What

The **Tauri IPC boundary** is the contract between the two halves. Everything behind
`invoke()` / Tauri events is backend (Tony). Everything in the Webview is frontend (Abid).

```
┌──────────────────────────────┐         ┌──────────────────────────────┐
│         FRONTEND (Abid)       │  IPC    │         BACKEND (Tony)        │
│                              │ ──────▶ │                              │
│  React 18 + TS + Vite        │ invoke  │  Rust crates (ff-*)          │
│  Zustand, TanStack Query     │         │  Tauri commands + events     │
│  Tailwind + shadcn/ui        │ ◀────── │  Agent loop, LLM, MCP,       │
│  apps/desktop/src/           │ events  │  memory, tools, signals      │
│                              │         │  apps/desktop/src-tauri/     │
│                              │         │  crates/ff-*                 │
└──────────────────────────────┘         └──────────────────────────────┘
                  │                                       │
                  └──── shared contract (TS types) ───────┘
                        generated from Rust via ts-rs
```

**Rule of thumb:** if it touches the filesystem, network, LLM, or a subprocess → backend.
If it renders pixels or handles input → frontend. The contract file is the only place
they meet.

---

## 1. Prerequisites (Backend Workstation)

| Tool | Version | Install |
|------|---------|---------|
| Rust | 1.95+ (stable) | `rustup toolchain install stable` |
| Node | 24 (pinned in `mise.toml`) | `mise install` |
| pnpm | 10+ | `corepack enable && corepack prepare pnpm@latest` |
| Tauri CLI | 2.11+ | `pnpm install` (already a devDependency) |

System deps (macOS): Xcode CLT (`xcode-select --install`). Tauri uses the OS WebView
(WKWebView), so no extra webkit install needed on macOS.

Verify:
```bash
rustc --version          # 1.95.0+
cargo --version
pnpm --version           # 10+
pnpm tauri --version     # invokes the local @tauri-apps/cli
```

> Note: this shell aliases `node` → bun. Always use `pnpm`/`cargo`, never call `node` directly.

---

## 2. Restructure: Single Crate → Workspace

The stock scaffold ships one crate (`appsdesktop`). Before real work, convert to the
workspace from the README so the engine is testable headlessly (no Tauri needed for unit tests).

### 2.1 Target layout
```
flowforge/
├── Cargo.toml                  # [workspace] — root
├── crates/
│   ├── ff-core/                # domain types (Message, Turn, Session, Skill, Profile)
│   ├── ff-agent/               # agent loop + tool dispatch
│   ├── ff-llm/                 # provider trait (OpenAI-compat candle-vllm + Ollama-native, then Bedrock)
│   ├── ff-mcp/                 # MCP client + supervisor
│   ├── ff-memory/              # SQLite + embeddings
│   ├── ff-signals/             # intention/outcome event bus
│   ├── ff-skills/              # skill discovery + hot-reload
│   ├── ff-tools/               # bash, edit, view, web_fetch, glob, rg
│   └── ff-workflow/            # DAG executor
└── apps/desktop/
    └── src-tauri/              # thin Tauri shell — depends on ff-* crates,
                                # contains ONLY command/event glue (no business logic)
```

### 2.2 Root `Cargo.toml`
```toml
[workspace]
resolver = "2"
members = ["crates/*", "apps/desktop/src-tauri"]

[workspace.package]
edition = "2021"
license = "MIT"
repository = "https://github.com/flowforge-lab/flowforge"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
anyhow = "1"
thiserror = "2"
tracing = "0.1"
ts-rs = "10"
```

### 2.3 Rename the Tauri crate
The scaffold name `appsdesktop` is auto-generated and ugly. Rename to `flowforge-desktop`:
- `apps/desktop/src-tauri/Cargo.toml`: `name = "flowforge-desktop"`, lib name `flowforge_desktop_lib`
- `apps/desktop/src-tauri/src/main.rs`: `flowforge_desktop_lib::run()`
- Make `src-tauri` deps point at workspace crates: `ff-agent = { path = "../../../crates/ff-agent" }`, etc.

### 2.4 Create crates
```bash
for c in ff-core ff-agent ff-llm ff-mcp ff-memory ff-signals ff-skills ff-tools ff-workflow; do
  cargo new --lib "crates/$c"
done
```
Each crate's `Cargo.toml` inherits with `edition.workspace = true`, etc.

**Architectural rule:** `src-tauri` is a *thin shell*. Business logic lives in `ff-*`.
A command handler should be ≤10 lines: deserialize → call into ff-crate → return.
This keeps the engine unit-testable without spinning up a window.

---

## 3. The IPC Contract — The Handoff Interface

This is the single most important artifact for splitting work. Define it FIRST so Abid
can build the entire frontend against a mock while the real Rust lands incrementally.

### 3.1 Commands (frontend → backend, request/response)
Declared as `#[tauri::command]` in `src-tauri`, thin wrappers over ff-crates.

M1–M2 contract (minimum to unblock Abid):
| Command | Args | Returns | Backing crate |
|---------|------|---------|---------------|
| `send_message` | `{ sessionId, content }` | `MessageId` | ff-agent |
| `list_sessions` | `{}` | `Session[]` | ff-memory |
| `create_session` | `{ goal?: string }` | `Session` | ff-memory |
| `get_messages` | `{ sessionId }` | `Message[]` | ff-memory |
| `cancel_turn` | `{ sessionId }` | `void` | ff-agent |

### 3.2 Events (backend → frontend, streaming)
Emitted via `app_handle.emit()`. The chat streams over events, not command return values.
| Event | Payload | When |
|-------|---------|------|
| `turn:token` | `{ sessionId, messageId, delta }` | each streamed LLM token |
| `turn:tool_call` | `{ sessionId, tool, args }` | agent invokes a tool |
| `turn:tool_result` | `{ sessionId, tool, result }` | tool completes |
| `turn:done` | `{ sessionId, messageId }` | turn complete |
| `turn:error` | `{ sessionId, message }` | failure |
| `signal:intention` | `{ sessionId, goal }` | session goal set (NeuroForge) |
| `signal:outcome` | `{ sessionId, status }` | done/abandoned (NeuroForge) |

### 3.3 Shared types — single source of truth
Rust types in `ff-core` derive `ts-rs::TS`. Run `cargo test` to regenerate the `.ts`
bindings into `apps/desktop/src/bindings/`. **Never hand-write these TS types.**

```rust
// crates/ff-core/src/message.rs
use ts_rs::TS;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../apps/desktop/src/bindings/")]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub role: Role,
    pub content: String,
    pub created_at: i64,
}
```

Abid imports: `import type { Message } from "@/bindings/Message";`

### 3.4 Mock backend for frontend dev
So Abid never blocks on Rust: provide a Vite-side mock that fulfills the same contract.
`apps/desktop/src/lib/ipc.ts` wraps every `invoke`/`listen`; when `VITE_FF_MOCK=1`,
it returns canned data + replays a fake token stream. Abid develops against the mock;
Tony swaps in the real commands. Same TS types on both sides → zero drift.

---

## 4. Backend Conventions

- **Error handling:** `thiserror` for library crates, `anyhow` only at the `src-tauri` boundary.
  Commands return `Result<T, String>` (Tauri serializes the err string to the frontend).
- **Async:** `tokio`. Long agent turns run on a spawned task; cancellation via `CancellationToken`.
- **Logging:** `tracing` + `tracing-subscriber`. No `println!` in committed code.
- **No business logic in `src-tauri`** (see §2.4).
- **Comments:** explain *why*, not *what*. No doc-comment noise on obvious signatures.
- **Secrets:** never logged, never crossing IPC. Provider keys live in OS keychain via
  `tauri-plugin-store` / keyring, resolved backend-side only.

---

## 5. Build / Test / Lint (mandatory before every push)

```bash
# Backend (run from repo root)
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace            # also regenerates ts-rs bindings

# Full app (Tauri dev — needs frontend deps installed)
pnpm install
pnpm tauri dev                    # hot-reloads both Rust and React

# Production build
pnpm tauri build
```

Definition of done for a backend change: `fmt` clean, `clippy` zero warnings,
`test` green, bindings committed if any `ff-core` type changed.

---

## 6. Collaboration Workflow

- **Branches:** `backend/<topic>` (Tony), `frontend/<topic>` (Abid). Short-lived, PR to `main`.
- **The contract is sacred:** changing a command signature or event payload = a PR that
  touches `ff-core` types + bindings + the mock, with Abid tagged as reviewer. Never
  silently break the seam.
- **Ownership in PRs:**
  - Files under `crates/**` and `src-tauri/**` → Tony approves
  - Files under `apps/desktop/src/**` (except `bindings/`) → Abid approves
  - `bindings/` is generated — regenerate, don't edit; conflicts resolved by re-running `cargo test`
- **CI gates (to add):** `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`,
  `pnpm typecheck`, `pnpm lint`. PR blocked until green.

---

## 7. M1 Acceptance (what "Rust setup done" means)

- [ ] Workspace compiles: `cargo build --workspace`
- [ ] Tauri crate renamed to `flowforge-desktop`, depends on `ff-*`
- [ ] `ff-core` defines `Message`, `Session`, `Role` with `ts-rs` export
- [ ] `ff-llm` has a `Provider` trait + OpenAI-compatible (candle-vllm) and Ollama-native impls (no creds needed)
- [ ] `send_message` command streams `turn:token` events end-to-end against candle-vllm
- [ ] TS bindings generated into `apps/desktop/src/bindings/`
- [ ] Mock IPC layer in place so Abid can run `VITE_FF_MOCK=1 pnpm dev` standalone
- [ ] `cargo clippy -D warnings` and `cargo test` green

Once §7 is checked, Abid is fully unblocked on the chat UI, command palette, and theming
against the typed contract + mock.
