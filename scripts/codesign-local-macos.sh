#!/usr/bin/env bash
#
# Ad-hoc codesign + clear quarantine on the local macOS .app produced by
# `pnpm build:local`. Unsigned bundles on macOS 26 often open a blank webview
# even when the binary and embedded assets are fine. Matches the end of
# scripts/dev-install.sh (without the /Applications install step).
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$REPO_ROOT/target/release/bundle/macos/FlowForge.app"

if [[ "$(uname)" != "Darwin" ]]; then
  exit 0
fi

if [[ ! -d "$APP" ]]; then
  echo "codesign-local-macos: no app at $APP (build may have failed)" >&2
  exit 1
fi

echo "==> Clearing quarantine on $APP"
xattr -dr com.apple.quarantine "$APP" || true

echo "==> Ad-hoc codesign"
codesign --force --deep --sign - "$APP"

echo "==> Local bundle ready: $APP"
echo "    Install with: ./scripts/dev-install.sh"
echo "    Or: rm -rf /Applications/FlowForge.app && cp -R \"$APP\" /Applications/ && codesign --force --deep --sign - /Applications/FlowForge.app"
