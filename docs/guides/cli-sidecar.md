# CLI sidecar & PATH caveat (CLI.7)

FlowForge ships the `flowforge` CLI inside the desktop app as a **Tauri
sidecar** (`bundle.externalBin`). This means installing the app gives you the
CLI at zero extra download — the binary lives inside the app bundle.

However, the bundled sidecar is **not on your `PATH`**. This page explains why
and how to get a `flowforge` command in your terminal.

> RFC reference: [RFC 0004 §5 — Distribution](../rfcs/0004-cli.md#5-distribution--two-channels-from-one-crate)

## Why the sidecar is not on PATH

Tauri's `externalBin` mechanism copies the binary into the app bundle's
resource directory — e.g. `FlowForge.app/Contents/Resources/` on macOS. That
directory is not a standard `PATH` location, so typing `flowforge` in a
terminal will not find it unless you take an extra step.

This is by design: the app bundle is self-contained, and modifying the user's
`PATH` from an installer is invasive and platform-specific.

## Option 1 — Install a standalone release artifact (recommended)

The primary distribution channel (RFC 0004 §5.1) is a standalone binary
downloaded from GitHub Releases. This is the recommended way to get
`flowforge` on your `PATH`.

1. Download the binary for your platform from the latest
   [GitHub Release](https://github.com/flowforge-lab/FlowForge/releases):
   - macOS arm64: `flowforge-aarch64-apple-darwin`
   - Linux x86_64: `flowforge-x86_64-unknown-linux-gnu`
2. Rename it to `flowforge` and make it executable:
   ```sh
   mv flowforge-<target-triple> flowforge
   chmod +x flowforge
   ```
3. Move it somewhere on your `PATH`:
   ```sh
   sudo mv flowforge /usr/local/bin/
   # …or, without sudo:
   mkdir -p ~/.local/bin && mv flowforge ~/.local/bin/
   ```
4. Verify:
   ```sh
   flowforge --version
   ```

The standalone binary is self-contained (static Rust stdlib; only platform base
libs are dynamically linked) and is the same binary the app bundles internally.

## Option 2 — Symlink the bundled sidecar

If you already have the desktop app installed and don't want a separate
download, you can symlink the bundled sidecar onto your `PATH`.

### macOS

```sh
# Find the bundled binary (adjust the app name/version as needed)
SIDECAR="/Applications/FlowForge.app/Contents/Resources/flowforge"

# Symlink to /usr/local/bin (or ~/.local/bin)
ln -sf "$SIDECAR" /usr/local/bin/flowforge

# Verify
flowforge --version
```

> **Note:** On macOS the sidecar binary is suffixed with the target triple
> internally (e.g. `flowforge-aarch64-apple-darwin`). Tauri resolves the suffix
> automatically when the app spawns the sidecar, but the file on disk in the
> bundle may keep the suffixed name. Symlink the suffixed file:
> ```sh
> ln -sf "/Applications/FlowForge.app/Contents/Resources/flowforge-aarch64-apple-darwin" /usr/local/bin/flowforge
> ```

### Linux

```sh
# Adjust the path to match your install location
SIDECAR="/opt/flowforge/resources/flowforge-$(rustc -vV | sed -n 's/^host: //p')"

ln -sf "$SIDECAR" ~/.local/bin/flowforge

flowforge --version
```

### Windows

Windows is not a Tier-1 platform (RFC 0004 §6). If you are using the desktop
app via WSL, symlink from inside WSL pointing at the Windows-side binary:

```sh
# Adjust the Windows path to your install location
SIDECAR="/mnt/c/Users/<you>/AppData/Local/FlowForge/resources/flowforge.exe"

ln -sf "$SIDECAR" ~/.local/bin/flowforge
```

## Developer notes

### Staging the sidecar for `tauri build`

The `externalBin` entry lives in a **bundle-only config overlay**
(`src-tauri/tauri.bundle.conf.json`), not in the base `tauri.conf.json`.
This keeps bare `cargo build` / `cargo test` / `cargo clippy` — and `tauri dev`
— unblocked: the build script never validates the sidecar path unless the
overlay is applied via `--config`.

When you *do* bundle (`tauri build`), Tauri requires the sidecar binary to
exist under `apps/desktop/src-tauri/binaries/` with the target-triple suffix:

```
apps/desktop/src-tauri/binaries/flowforge-<target-triple>
```

Build the CLI and copy it into place:

```sh
# 1. Build the CLI
cargo build -p ff-cli --release

# 2. Determine your target triple
TRIPLE=$(rustc -vV | sed -n 's/^host: //p')

# 3. Create the binaries directory and copy
mkdir -p apps/desktop/src-tauri/binaries
cp target/release/flowforge "apps/desktop/src-tauri/binaries/flowforge-$TRIPLE"
```

Then bundle with the overlay:

```sh
pnpm tauri build --config src-tauri/tauri.bundle.conf.json
```

The dev scripts (`scripts/dev-install.sh`, `scripts/dev-release.sh`) already
stage the sidecar and pass `--config src-tauri/tauri.bundle.conf.json` for you.

> The `binaries/` directory is git-ignored — it holds compiled binaries and
> should never be committed.

### Parity smoke-test

The desktop registers a `run_sidecar_turn` Tauri command that spawns the
sidecar with `flowforge run "<prompt>" --json` and re-emits every parsed
`AgentEvent` as the same Tauri events the in-process `run_turn` path emits
(`turn:token`, `tool:call`, `turn:done`, …). This lets the frontend verify that
the sidecar produces an event stream equivalent to the in-process path — the
"parity smoke-test" required by CLI.7.
