#!/usr/bin/env bash
#
# stage-sidecar.sh — Build the `flowforge` CLI and stage it where the desktop
# app's Tauri shell sidecar resolution (`tauri_plugin_shell::Command::new_sidecar`)
# and the `tauri build` bundler both expect it.
#
# Two locations are staged:
#
#   1. apps/desktop/src-tauri/binaries/flowforge-<target-triple>
#      Required by `tauri build --config tauri.bundle.conf.json` (the
#      `externalBin` entry). Git-ignored; never committed.
#
#   2. target/<profile>/flowforge
#      The location `tauri_plugin_shell` resolves at runtime relative to
#      `current_exe()` (see `relative_command_path` in the shell plugin). This
#      is the path the sidecar parity integration test relies on, and the path
#      `tauri dev` resolves when the bundle overlay is NOT applied.
#
# Usage:  ./scripts/stage-sidecar.sh [--release]
#
set -euo pipefail

PROFILE="debug"
if [[ "${1:-}" == "--release" ]]; then
  PROFILE="release"
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TRIPLE=$(rustc -vV | sed -n 's/^host: //p')

echo "==> Building ff-cli (profile: $PROFILE)"
if [[ "$PROFILE" == "release" ]]; then
  cargo build -p ff-cli --release
else
  cargo build -p ff-cli
fi

SIDECAR_BIN="$REPO_ROOT/target/$PROFILE/flowforge"
if [[ ! -f "$SIDECAR_BIN" ]]; then
  echo "Build did not produce $SIDECAR_BIN" >&2
  exit 1
fi

# 1. Stage for `tauri build` (bundle overlay).
BUNDLE_DIR="$REPO_ROOT/apps/desktop/src-tauri/binaries"
mkdir -p "$BUNDLE_DIR"
cp "$SIDECAR_BIN" "$BUNDLE_DIR/flowforge-$TRIPLE"
echo "==> Staged bundle sidecar: $BUNDLE_DIR/flowforge-$TRIPLE"

# 2. target/<profile>/flowforge is already in place from the build — no copy
#    needed. The integration test resolves it relative to the test binary.
echo "==> Runtime sidecar in place: $SIDECAR_BIN"
echo "==> Done. Run the sidecar parity test with:"
if [[ "$PROFILE" == "release" ]]; then
  echo "    cargo test -p flowforge-desktop sidecar_turn --release -- --nocapture --ignored"
else
  echo "    cargo test -p flowforge-desktop sidecar_turn -- --nocapture --ignored"
fi
