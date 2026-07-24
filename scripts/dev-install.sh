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
BUNDLE_DIR="$REPO_ROOT/target/release/bundle/macos"
INSTALL_DIR="/Applications"

if [[ "$(uname)" != "Darwin" ]]; then
  echo "dev-install.sh currently supports macOS only (the P1 dogfood platform)." >&2
  exit 1
fi

echo "==> Building CLI sidecar binary (CLI.7)"
TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
mkdir -p "$REPO_ROOT/apps/desktop/src-tauri/binaries"
cargo build -p ff-cli --release
cp "$REPO_ROOT/target/release/flowforge" \
   "$REPO_ROOT/apps/desktop/src-tauri/binaries/flowforge-$TRIPLE"

# Version by the COMMITTER DATE of the built commit, not a fixed value or the
# build moment (#1034), matching scripts/dev-release.sh. A hardcoded 0.0.0-dev.0
# made every dev-install look identical, so the updater's downgrade guard could
# not order two local builds; committer time is monotonic along history, so the
# semver ordering of the 0.0.0-dev.<epoch> prerelease is exactly commit recency.
DEV_VERSION="0.0.0-dev.$(git -C "$REPO_ROOT" show -s --format=%ct HEAD)"

echo "==> Building release bundle (pnpm tauri build) as $DEV_VERSION"
# pnpm/tauri scripts live in apps/desktop; the cargo workspace target is at the
# repo root. Build the app bundle only -- no .dmg (flaky bundle_dmg.sh) and no
# updater artifact (D2 has no updater, so signing keys are not required).
cd "$REPO_ROOT/apps/desktop"
pnpm install --frozen-lockfile
pnpm tauri build --bundles app \
  --config src-tauri/tauri.bundle.conf.json \
  --config src-tauri/tauri.no-updater-sign.conf.json \
  --config "{\"version\":\"$DEV_VERSION\"}"

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

# Ad-hoc codesign with a stable identity so macOS Keychain recognizes the app
# across rebuilds and stops prompting for keychain access on every launch.
# `--deep` is deprecated by Apple (prefer signing nested items individually);
# fine for ad-hoc local/dev builds — same as scripts/codesign-local-macos.sh.
echo "==> Codesigning (ad-hoc)"
codesign --force --deep --sign - "$INSTALL_DIR/$APP_NAME"

echo "==> Done. Relaunch FlowForge from $INSTALL_DIR manually."
