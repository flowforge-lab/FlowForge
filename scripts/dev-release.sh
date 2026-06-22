#!/usr/bin/env bash
#
# dev-release.sh -- D1 dogfood loop (RFC 0014, optional): build an updater-signed
# bundle, generate a local `latest.json`, and serve it over http://localhost so a
# dev-flavored install can pull the build through the in-app "Update now" button.
#
# Pairs with apps/desktop/src-tauri/tauri.local.conf.json (localhost endpoint +
# dangerousInsecureTransportProtocol) and the FF_UPDATER_ENDPOINT / lenient
# version_comparator hooks in the backend. Prod/CI config stays strict-HTTPS.
#
# Usage:  ./scripts/dev-release.sh [PORT]   (default 8787)
#
# Then launch the dev install pointed at the feed:
#   FF_UPDATER_ENDPOINT="http://localhost:8787/latest.json" \
#     /Applications/FlowForge.app/Contents/MacOS/FlowForge
# and click Settings -> About -> Check for updates -> Update now.
#
# Requires TAURI_SIGNING_PRIVATE_KEY[_PASSWORD] in the environment so the build
# signs the updater artifact (the same minisign key whose pubkey is committed).
set -euo pipefail

PORT="${1:-8787}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUNDLE_DIR="$REPO_ROOT/apps/desktop/src-tauri/target/release/bundle/macos"
LOCAL_CONF="src-tauri/tauri.local.conf.json"

if [[ "$(uname)" != "Darwin" ]]; then
  echo "dev-release.sh currently supports macOS only (the P1 dogfood platform)." >&2
  exit 1
fi
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  echo "TAURI_SIGNING_PRIVATE_KEY is not set; the updater artifact will be unsigned." >&2
  echo "Export the minisign key (and _PASSWORD) before running, e.g. from ~/.tauri." >&2
  exit 1
fi

case "$(uname -m)" in
  arm64) PLATFORM="darwin-aarch64" ;;
  x86_64) PLATFORM="darwin-x86_64" ;;
  *) echo "unsupported arch $(uname -m)" >&2; exit 1 ;;
esac

DEV_VERSION="0.0.0-dev.$(date +%s)"
echo "==> Building signed updater bundle, version $DEV_VERSION"
cd "$REPO_ROOT/apps/desktop"
pnpm tauri build --config "$LOCAL_CONF" --config "{\"version\":\"$DEV_VERSION\"}"

TARBALL="$BUNDLE_DIR/FlowForge.app.tar.gz"
SIG="$TARBALL.sig"
if [[ ! -f "$TARBALL" || ! -f "$SIG" ]]; then
  echo "Expected updater artifacts not found: $TARBALL(.sig)" >&2
  exit 1
fi

echo "==> Writing latest.json"
SIGNATURE="$(cat "$SIG")" \
DEV_VERSION="$DEV_VERSION" \
PLATFORM="$PLATFORM" \
PORT="$PORT" \
python3 - "$BUNDLE_DIR/latest.json" <<'PY'
import json, os, sys, datetime
out = sys.argv[1]
manifest = {
    "version": os.environ["DEV_VERSION"],
    "notes": "Local dev build (dev-release.sh).",
    "pub_date": datetime.datetime.now(datetime.timezone.utc)
        .strftime("%Y-%m-%dT%H:%M:%SZ"),
    "platforms": {
        os.environ["PLATFORM"]: {
            "signature": os.environ["SIGNATURE"],
            "url": f"http://localhost:{os.environ['PORT']}/FlowForge.app.tar.gz",
        }
    },
}
with open(out, "w") as f:
    json.dump(manifest, f, indent=2)
PY

echo "==> Serving $BUNDLE_DIR at http://localhost:$PORT (Ctrl-C to stop)"
echo "    Launch the dev install pointed at the feed:"
echo "      FF_UPDATER_ENDPOINT=\"http://localhost:$PORT/latest.json\" \\"
echo "        /Applications/FlowForge.app/Contents/MacOS/FlowForge"
cd "$BUNDLE_DIR"
exec python3 -m http.server "$PORT"
