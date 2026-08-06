#!/usr/bin/env bash
#
# dev-install.sh -- D2 dogfood loop (RFC 0014): build FlowForge locally and run it
# as the installed app, with no update feed, no server, and no GitHub round-trip.
# The updater plugin still ships (only artifact creation is off), so the build is
# pointed at the local D3 feed and dev pubkey -- see the tauri build call below.
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
# Per-developer dev pubkey overlay (git-ignored), same path scripts/dev-release.sh uses.
DEV_LOCAL_CONF="src-tauri/tauri.dev-local.conf.json"

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
# updater artifact of our own -- so no signing keys are required here.
#
# `no-updater-sign` only turns off createUpdaterArtifacts; the updater plugin and
# its baked-in pubkey still ship, so an installed D2 build *does* check for
# updates. Without the dev pubkey overlay below it bakes tauri.conf.json's
# PRODUCTION pubkey, and then the D1 loop (scripts/dev-release.sh, which signs with
# your dev key) can never install onto it -- the signature fails to verify. That is
# not fixable at runtime: updater_builder() overrides the endpoint per channel but
# never the pubkey, so it has to be layered at build time.
#
# We deliberately do NOT layer tauri.local.conf.json, which would also replace
# `endpoints`: the Local channel builds its feed URL at runtime, but the GitHub
# channel uses bare app.updater() and reads the *config* endpoint, so baking the
# localhost feed would silently redirect real update checks as well.
#
# The one piece we do need from it is dangerousInsecureTransportProtocol. The
# Local channel passes http://localhost to UpdaterBuilder::endpoints(), which
# validates against that flag as baked into the config, and a release build turns
# a non-https endpoint into a hard Err (tauri-plugin-updater config.rs). So it is
# set inline below, leaving the GitHub endpoint untouched.
CONFIG_ARGS=(
  --config src-tauri/tauri.bundle.conf.json
  --config src-tauri/tauri.no-updater-sign.conf.json
  --config '{"plugins":{"updater":{"dangerousInsecureTransportProtocol":true}}}'
)
# Layered last so it wins over tauri.conf.json's production pubkey. The overlay is
# per-developer and git-ignored, so it is optional -- warn rather than fail, since
# D2 on its own works fine without it.
if [[ -f "$REPO_ROOT/apps/desktop/$DEV_LOCAL_CONF" ]]; then
  CONFIG_ARGS+=(--config "$DEV_LOCAL_CONF")
  echo "==> Using dev pubkey overlay $DEV_LOCAL_CONF"
else
  echo "==> WARNING: $DEV_LOCAL_CONF not found; baking the PRODUCTION pubkey." >&2
  echo "    scripts/dev-release.sh updates will not install onto this build." >&2
  echo "    To fix: docs/SOP-rust-setup.md 8.3, 'One-time setup: your own dev signing key'." >&2
fi
cd "$REPO_ROOT/apps/desktop"
pnpm install --frozen-lockfile
pnpm tauri build --bundles app "${CONFIG_ARGS[@]}" \
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
