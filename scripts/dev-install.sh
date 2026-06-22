#!/usr/bin/env bash
#
# dev-install.sh -- D2 dogfood loop (RFC 0014): build FlowForge locally and run it
# as the installed app, with no updater, no server, and no GitHub round-trip.
#
# This is the day-to-day loop: build, replace /Applications/FlowForge.app, relaunch.
# Local state (~/.flowforge, ~/.config/flowforge) lives in $HOME and survives the swap.
#
# Usage:  ./scripts/dev-install.sh
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="FlowForge.app"
BUNDLE_DIR="$REPO_ROOT/apps/desktop/src-tauri/target/release/bundle/macos"
INSTALL_DIR="/Applications"

if [[ "$(uname)" != "Darwin" ]]; then
  echo "dev-install.sh currently supports macOS only (the P1 dogfood platform)." >&2
  exit 1
fi

echo "==> Building release bundle (pnpm tauri build)"
cd "$REPO_ROOT"
pnpm install --frozen-lockfile
pnpm tauri build

BUILT_APP="$BUNDLE_DIR/$APP_NAME"
if [[ ! -d "$BUILT_APP" ]]; then
  echo "Build did not produce $BUILT_APP" >&2
  exit 1
fi

echo "==> Replacing $INSTALL_DIR/$APP_NAME"
rm -rf "${INSTALL_DIR:?}/$APP_NAME"
cp -R "$BUILT_APP" "$INSTALL_DIR/"

# Unsigned local build: clear the Gatekeeper quarantine flag so it opens without
# the "damaged / unknown developer" prompt (RFC 0014 section 9). Apple
# notarization (phase 2) removes the need for this.
echo "==> Clearing quarantine attribute"
xattr -dr com.apple.quarantine "$INSTALL_DIR/$APP_NAME" || true

echo "==> Done. Relaunch FlowForge from $INSTALL_DIR manually."
