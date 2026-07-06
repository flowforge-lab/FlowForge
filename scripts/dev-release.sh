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
# Workspace builds share a single target/ at the repo root (not src-tauri/target).
BUNDLE_DIR="$REPO_ROOT/target/release/bundle/macos"
LOCAL_CONF="src-tauri/tauri.local.conf.json"
BUNDLE_CONF="src-tauri/tauri.bundle.conf.json"

if [[ "$(uname)" != "Darwin" ]]; then
  echo "dev-release.sh currently supports macOS only (the P1 dogfood platform)." >&2
  exit 1
fi
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  echo "TAURI_SIGNING_PRIVATE_KEY is not set; aborting (the updater artifact must be signed)." >&2
  echo "Export the minisign key (and _PASSWORD) before running, e.g. from ~/.tauri." >&2
  exit 1
fi

case "$(uname -m)" in
  arm64) PLATFORM="darwin-aarch64" ;;
  x86_64) PLATFORM="darwin-x86_64" ;;
  *) echo "unsupported arch $(uname -m)" >&2; exit 1 ;;
esac

DEV_VERSION="0.0.0-dev.$(date +%s)"
echo "==> Building CLI sidecar binary (CLI.7)"
TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
mkdir -p "$REPO_ROOT/apps/desktop/src-tauri/binaries"
cargo build -p ff-cli --release
cp "$REPO_ROOT/target/release/flowforge" \
   "$REPO_ROOT/apps/desktop/src-tauri/binaries/flowforge-$TRIPLE"

echo "==> Building signed updater bundle, version $DEV_VERSION"
cd "$REPO_ROOT/apps/desktop"
# Remove stale artifacts so we never accidentally serve an old tarball with a new version.
rm -f "$BUNDLE_DIR/FlowForge.app.tar.gz" "$BUNDLE_DIR/FlowForge.app.tar.gz.sig"
pnpm tauri build --bundles updater --config "$LOCAL_CONF" --config "$BUNDLE_CONF" --config "{\"version\":\"$DEV_VERSION\"}"

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

DEV_UPDATE_DIR="$HOME/.config/flowforge/dev-update"
PIDFILE="$HOME/.config/flowforge/dev-update-server.pid"
mkdir -p "$(dirname "$PIDFILE")"

# Kill any previous dev-update server so the port is free.
if [[ -f "$PIDFILE" ]]; then
  OLD_PID="$(cat "$PIDFILE")"
  if kill -0 "$OLD_PID" 2>/dev/null; then
    echo "==> Stopping previous dev-update server (PID $OLD_PID)"
    kill "$OLD_PID" 2>/dev/null || true
    sleep 0.3
  fi
  rm -f "$PIDFILE"
fi

# Copy the bundle + feed to the well-known dev-update directory that the
# file-system watcher (#705 Phase 2) observes for instant detection.
mkdir -p "$DEV_UPDATE_DIR"
cp "$TARBALL" "$DEV_UPDATE_DIR/"
# Write latest.json LAST so the watcher fires after the tarball is in place.
cp "$BUNDLE_DIR/latest.json" "$DEV_UPDATE_DIR/latest.json"

echo "==> Starting background HTTP server at http://localhost:$PORT"
cd "$BUNDLE_DIR"
python3 -m http.server "$PORT" &>/dev/null &
SERVER_PID=$!
echo "$SERVER_PID" > "$PIDFILE"

echo "==> Done. Server PID $SERVER_PID (pidfile: $PIDFILE)"
echo "    The running FlowForge app will detect the update within ~15s"
echo "    (requires the localUpdateChannel experimental flag to be on)."
echo ""
echo "    To stop the server later:  kill $(cat "$PIDFILE")"
echo "    To rebuild:                just re-run this script (auto-kills the old server)."
