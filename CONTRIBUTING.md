# Contributing to FlowForge

Thanks for your interest in FlowForge! 🔗🧠

## Before You Start

All contributions — code, docs, and design — follow our
**[Engineering Principles](./PRINCIPLES.md)**. Please read the charter first;
it is short, opinionated, and binding. Every pull request is reviewed against
its four pillars:

1. **Flow for the User, First**
2. **Efficiency: Footprint & Latency**
3. **Adaptive & Migratable**
4. **Code the Zen Way**

## Workflow

1. **Open an issue** describing the change before large work, so we can align early.
2. **Branch** from `main`.
3. **Implement** — match existing patterns, keep modules flat, handle errors explicitly.
4. **Verify** before opening a PR:
   - `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`
   - `cargo test --workspace`
   - `pnpm typecheck && pnpm lint && pnpm test`
5. **Squash to a single commit** before opening (or updating) a PR. Every PR
   must contain exactly **one** well-described commit — squash with
   `git rebase -i main` or `git reset --soft main && git commit`. This keeps
   `main` linear and each change atomically revertable (Pillar 3).
6. **Open a PR** with a clear *why* in the description. If you can't explain the
   implementation in a few sentences, reconsider the design (Pillar 4).

## Quick Start

See the [Development section in the README](./README.md#-development) for setup.

## Questions

Open a discussion or an issue. We'd rather talk early than guess later —
*in the face of ambiguity, refuse the temptation to guess.*

## Adding a New Tool

Tools are agent-callable functions that expose filesystem or workspace capabilities. Each new tool must implement the [`Tool` trait](crates/ff-tools/src/registry.rs) and register itself in the tool registry. Here's the pattern:

### 1. Create the tool file

Add a new module in `crates/ff-tools/src/<tool_name>.rs`. At minimum, you need:

- A zero-sized struct (e.g. `pub struct MyTool;`) — state lives on the caller or in Tauri state for registry-backed tools.
- An `impl Tool` block with five required methods:

| Method | Purpose | Notes |
|--------|---------|-------|
| `name()` | Unique identifier used by the agent loop | Lowercase, snake_case |
| `description()` | Shown to the model as the function description | Be precise about inputs and output format |
| `parameters()` | JSON Schema (the `parameters` field of an OpenAI function) | Document each field's type and description; list required fields |
| `safety()` | Classify invocation safety level — see below | Defaults to `Safety::Write` |
| `run(args, root)` | Execute the tool | Never panic or propagate transport errors — return `ToolOutcome::error` instead |

### 2. Choose a safety level

From [`crates/ff-tools/src/registry.rs`](crates/ff-tools/src/registry.rs):

- **`ReadOnly`** — auto-runs (no approval gate). Use when the tool only reads or enumerates state.
- **`Write`** — routes through the host's approval policy. For file creation, modification, or deletion.
- **`Dangerous`** — same approval gate plus higher scrutiny. For irreversible operations or external side-effects (e.g. installing skills).

Prefer `ReadOnly` whenever you can prove the tool doesn't mutate state; this gives the model more autonomy inside its turn.

### 3. Jail all file paths

**Never join raw user input onto the workspace root.** Always use:

```rust
use crate::jail::resolve_in_root; // for existing-file reads
// or
use crate::jail::resolve_for_create; // for new files whose parents may not exist yet
```

These functions canonicalize both sides and reject any path that escapes `root` — including `..` traversal and symlink-anchored escapes. See [jail.rs](crates/ff-tools/src/jail.rs) for the full invariants.

### 4. Register the tool

Add your module to `crates/ff-tools/src/lib.rs` (the `pub mod` line), then register it in:

```rust
ToolRegistry::with_defaults() // in crates/ff-tools/src/registry.rs
```

### 5. Where to put registry-backed tools

If your tool needs access to the live skill registry (`SharedRegistry`) or other Tauri state, **do not** put it in `crates/ff-tools`. Instead:

- Put the file in [`apps/desktop/src-tauri/src/tools.rs`](apps/desktop/src-tauri/src/tools.rs) (the pattern for install/uninstall skill tools is there).
- Register it via Tauri's command/state system rather than through `ToolRegistry`.

### 6. Output conventions

- **Cap output size.** Use a constant like `MAX_PATHS` (see `glob.rs`) to bound results. Push a `(truncated at N)` sentinel into the output when you hit the ceiling.
- **Deterministic ordering.** Sort any collection of paths or identifiers before joining — models reason better from stable output.
- **Return `ToolOutcome::ok("")` for empty results** with a short human-friendly message (e.g. `"(no matches)"`) rather than an empty string, so the model doesn't confuse it with an error.

### 7. Testing

Every tool must have:

1. **Functional unit tests** verifying core behavior (matching, edge cases). Use `tempfile::tempdir()` for isolated filesystem fixtures.
2. **A jail-escape test** if the tool accepts path arguments — verify that inputs like `"../"` or `/etc/hosts` return a failed outcome with an "access denied" message.

Run tests locally with:

```bash
cargo test --workspace
```

### 8. Template to copy

The canonical reference implementation is [glob.rs](crates/ff-tools/src/glob.rs). It covers every convention above: parameter schema, safety classification, path jailing, output capping, deterministic sorting, jail-escape rejection, and a full test suite. Use it as your starting scaffold.

### Quick checklist before submitting

- [ ] `Tool` trait implemented with all five methods
- [ ] `safety()` returns the most permissive level you can prove correct
- [ ] All path inputs go through `resolve_in_root` / `resolve_for_create`
- [ ] Output is capped and deterministically ordered
- [ ] Unit tests + jail-escape test present
- [ ] Module exposed in `lib.rs`, registered in `registry.rs` (or `tools.rs` if Tauri-backed)
- [ ] `cargo fmt --check`, `clippy --all-targets -- -D warnings`, and `cargo test --workspace` all pass

---

## CLI (`apps/cli`)

FlowForge ships a headless CLI binary (`flowforge`) that reuses the same agent
loop, tools, and provider stack as the desktop app — no GUI, no Tauri.

### Building & testing

```bash
cargo build -p ff-cli
cargo test  -p ff-cli
cargo run   -p ff-cli -- --help
```

### Exit-code contract

The `run` subcommand follows a scripting-friendly contract:

- **0** — turn completed successfully (no agent error, no denied approval).
- **non-zero** — an agent error occurred, or a required tool approval was
  denied (`--deny`, piped-no-policy, or `N` at a prompt).

The interactive REPL exits **0** on clean shutdown; per-turn failures are
printed inline.

### Code map

- `main.rs` — clap CLI, subcommand dispatch, `run` / `chat` / `sessions list` / `fork` entry points
- `approver.rs` — `CliApprover`: approval policy (`--yes` / `--deny` / prompt),
  denial tracking (`was_denied()`), piped-no-policy loud-deny rule
- `host.rs` — provider loading, workspace setup, tool registry construction,
  session store path resolution (`build_session_store`)
- `sessions.rs` — pure helpers for session label resolution, `(Fork N)` naming,
  and `sessions list` rendering (mirrors `apps/desktop/src/lib/sessions.ts`)
- `json_events.rs` — `--json` event serialization for machine-readable output

When adding a new flag or subcommand, update the doc comments (they drive
`--help` via clap) and the exit-code section in `README.md` if the contract
changes.
