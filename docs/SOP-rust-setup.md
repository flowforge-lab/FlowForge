# SOP — FlowForge Dev Setup & Operations

**Audience:** FlowForge contributors
**Owner (backend):** Tony (ytonytan)
**Owner (frontend):** Abid
**Status:** Living document — update when the IPC contract, workspace layout, or the
local install/update loops change

**Scope:** the front/back split + IPC contract (§0–§6), the historical M1 bootstrap
(§2, §7 — done, kept for reference), and **running & updating your local install** (§8 —
the day-to-day dogfood loop, RFC 0014).

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

> **Historical (M1 — done).** The workspace already exists; this section is kept as the
> record of how it was built. New contributors do **not** need to redo any of it.

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
| `respond_approval` | `{ callId, approved }` | `void` | ff-agent (wakes the awaiting approver) |

### 3.2 Events (backend → frontend, streaming)
Emitted via `app_handle.emit()`. The chat streams over events, not command return values.
| Event | Payload | When |
|-------|---------|------|
| `turn:token` | `{ sessionId, messageId, delta }` | each streamed LLM token (non-empty deltas only) |
| `tool:call` | `{ sessionId, messageId, callId, tool, args }` | agent invokes a tool |
| `tool:approval-request` | `{ sessionId, messageId, callId, tool, args, safety }` | a write/dangerous tool needs user approval; backend awaits `respond_approval` |
| `tool:result` | `{ sessionId, messageId, callId, success, result }` | tool completes (or was denied/cancelled) |
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

# Local production bundle (no signing key needed)
pnpm build:local            # .app/.dmg without the signed updater artifact

# Production build
# Release/CI build (signs the updater artifact)
pnpm tauri build            # requires TAURI_SIGNING_PRIVATE_KEY (see below)
# To bundle the CLI sidecar, pass the bundle overlay:
#   pnpm tauri build --config src-tauri/tauri.bundle.conf.json
# The dev scripts (scripts/dev-*.sh) do this for you.
```

> **Heads-up:** `tauri.conf.json` sets `bundle.createUpdaterArtifacts: true` with a
> committed updater `pubkey` (RFC 0014 self-update). Because of that, a bare
> `pnpm tauri build` *signs* the `.app.tar.gz` updater artifact and fails with
> *"A public key has been found, but no private key… set `TAURI_SIGNING_PRIVATE_KEY`"*
> unless that release secret is in your env. For everyday local builds use
> **`pnpm build:local`** (passes `--config src-tauri/tauri.no-updater-sign.conf.json`,
> the same overlay `dev-install.sh` uses) — it produces a runnable bundle with no key.
> A file-path `--config` is used instead of an inline JSON string so the command is
> shell-quoting-safe on Windows (`cmd.exe`/PowerShell don't strip single quotes the
> way bash does).
> Only the release path (CI) and the D1 update-feed loop (§8.3) need the signing key.

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

> **Historical (M1 — done).** All boxes below are checked; kept as the definition-of-done
> record for the initial setup milestone.


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

---

## 8. Running & Updating Your Local Install (RFC 0014)

`pnpm tauri dev` (§5) runs the app against the dev server — great for editing, but not how
you *use* FlowForge day to day. This section covers running the **installed** app and
keeping it current. Local state (`~/.flowforge`, `~/.config/flowforge`) lives in your home
directory, **not** the app bundle, so it survives every reinstall and update for free.

### 8.1 First install

- **From a release (once one exists):** download the `.dmg` from the GitHub Release and
  drag the app to `/Applications`. The build is not yet Apple-notarized (RFC 0014 §9), so
  Gatekeeper flags the first open: right-click → **Open**, or
  `xattr -dr com.apple.quarantine /Applications/FlowForge.app`.
- **From source:** run the D2 loop below — it builds and installs in one step.

### 8.2 D2 — direct reinstall (the daily loop)

The everyday loop: build locally and replace the installed app. No updater, no server.

```bash
./scripts/dev-install.sh
```

It runs `pnpm tauri build`, replaces `/Applications/FlowForge.app` with the fresh bundle,
clears the quarantine flag, **ad-hoc codesigns** the app, and tells you to relaunch. Use
this for almost all dogfooding.

> **Do not stop at `pnpm build:local` alone** if you want a runnable installed app.
> `build:local` produces the `.app`/`.dmg` and (on macOS) ad-hoc codesigns the bundle under
> `target/release/bundle/macos/`. Opening an *unsigned* copy — or one still marked with
> Gatekeeper quarantine — is a common cause of a blank charcoal window on macOS 26 even
> though `pnpm tauri dev` works. Prefer `./scripts/dev-install.sh`, or after `build:local`:
>
> ```bash
> rm -rf /Applications/FlowForge.app
> cp -R target/release/bundle/macos/FlowForge.app /Applications/
> xattr -dr com.apple.quarantine /Applications/FlowForge.app
> codesign --force --deep --sign - /Applications/FlowForge.app
> open /Applications/FlowForge.app
> ```
>
> If the window is still blank, clear webview + app caches (state under `$HOME` is separate
> from Tony’s Vite race advice — that only applies to `tauri dev`):
>
> ```bash
> rm -rf ~/Library/WebKit/ai.flowforge.desktop
> rm -rf ~/Library/Caches/ai.flowforge.desktop
> # optional hard reset of session DB (destructive):
> # rm -rf ~/Library/Application\ Support/flowforge/
> ```

### 8.3 D1 — local update feed (optional, exercises "Update now")

Use this only when you want to test the in-app **Settings → About → Update now** button
against a local build instead of a real GitHub release.

#### One-time setup: your own dev signing key (#1047)

The updater refuses an unsigned update, and the **production** signing key is held by one
maintainer and not shared — so every other developer signs with their own throwaway key.
The catch is that the updater trusts only the pubkey **compiled into the app**, so you
also have to build the app with your dev **pub**key, or your own bundle fails verification.

```bash
# 1. Generate a personal keypair (private key stays in ~/.tauri, never committed).
pnpm -C apps/desktop tauri signer generate -w ~/.tauri/flowforge-dev.key

# 2. Tell the local build to trust it. This file is git-ignored on purpose — no one
#    developer's key belongs in a committed config.
cat > apps/desktop/src-tauri/tauri.dev-local.conf.json <<JSON
{
  "\$schema": "https://schema.tauri.app/config/2",
  "plugins": { "updater": { "pubkey": "$(cat ~/.tauri/flowforge-dev.key.pub)" } }
}
JSON
```

`dev-release.sh` layers that overlay automatically when it exists, and warns when it
doesn't (in which case the build keeps the production pubkey and will reject your bundle).

Because the running app must already trust your dev key, **install a build made with the
overlay first** (D2, §8.2, or the first `dev-release.sh` bundle copied into
`/Applications`) — an app installed from a production-pubkey build will refuse every
dev-signed update no matter how the feed is configured.

#### Running the loop

```bash
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/flowforge-dev.key)"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="…"   # empty string if you set no password
./scripts/dev-release.sh 8787
```

`dev-release.sh` builds a signed updater bundle with a bumped `0.0.0-dev.<timestamp>`
version (so the updater always sees it as newer), writes a `latest.json` pointing at
`http://localhost:8787/FlowForge.app.tar.gz`, and serves the bundle directory. In another
terminal, launch the install pointed at the local feed:

```bash
FF_UPDATER_ENDPOINT="http://localhost:8787/latest.json" \
  /Applications/FlowForge.app/Contents/MacOS/FlowForge
```

Then click **Settings → About → Check for updates → Update now**. The dev-only config
patch (`apps/desktop/src-tauri/tauri.local.conf.json`) supplies the localhost endpoint and
`dangerousInsecureTransportProtocol: true` so the dev build trusts a plain-HTTP feed. It is
applied only via `tauri build --config` and is **never shipped** — `tauri.conf.json` stays
strict-HTTPS for prod/CI. The same is true of your `tauri.dev-local.conf.json` pubkey
overlay: it is local, git-ignored, and an app built with it must never be distributed,
since it trusts your personal key.

#### Troubleshooting

| Symptom | Cause |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY is not set` | Export it (see above) — a dev key is fine, you don't need the production key. |
| Update downloads, then fails to install | The running app was built with a **different** pubkey than the one that signed the bundle. Reinstall an app built with your overlay. |
| Banner never appears | Feed unreachable (is `dev-release.sh` still serving?) or the `localUpdateChannel` experimental flag is off. |

### 8.4 Which loop?

| You want to… | Use |
|---|---|
| Run the latest local code as the installed app | **D2** (`dev-install.sh`) |
| Verify the in-app updater / "Update now" end to end | **D1** (`dev-release.sh` + local feed) |
| Edit with hot-reload | `pnpm tauri dev` (§5) |

> The publish flow (tag → CI → signed GitHub Release → `latest.json`) is a separate
> milestone (RFC 0014 P2, `release.yml` + `RELEASING.md`) and is out of scope here.
