#!/usr/bin/env bash
#
# dev-release.sh -- D1 dogfood loop (RFC 0014, optional): build an updater-signed
# bundle, generate a local `latest.json`, and serve it over http://localhost so a
# dev-flavored install can pull the build through the in-app "Update now" button.
#
# Pairs with apps/desktop/src-tauri/tauri.local.conf.json (localhost endpoint +
# dangerousInsecureTransportProtocol) and the FF_UPDATER_ENDPOINT / permissive
# version_comparator hooks in the backend, which surfaces builds in BOTH directions
# and classifies newer vs older itself (#1034). Prod/CI config stays strict-HTTPS.
#
# Usage:  ./scripts/dev-release.sh [PORT]   (default 8787)
#
# Then launch the dev install pointed at the feed:
#   FF_UPDATER_ENDPOINT="http://localhost:8787/latest.json" \
#     /Applications/FlowForge.app/Contents/MacOS/FlowForge
# and click Settings -> About -> Check for updates -> Update now.
#
# Requires TAURI_SIGNING_PRIVATE_KEY[_PASSWORD] in the environment so the build signs
# the updater artifact. That key does NOT have to be the production release key (whose
# pubkey is committed in tauri.conf.json) — see "Dev signing" below (#1047).
#
# Dev signing (#1047): the updater only trusts the pubkey COMPILED INTO the app, so a
# bundle signed with your own dev key installs only if the app was built with your dev
# PUBkey. Drop yours into the git-ignored
#
#   apps/desktop/src-tauri/tauri.dev-local.conf.json
#
#   { "plugins": { "updater": { "pubkey": "<your dev pubkey>" } } }
#
# and this script layers it over the committed configs automatically. Without it the
# build falls back to the production pubkey, which only works if you hold the production
# private key. Generate a personal keypair with:
#
#   pnpm -C apps/desktop tauri signer generate -w ~/.tauri/flowforge-dev.key
#
# The private key stays local and is never committed or shared; only your PUBkey ever
# lands in the (git-ignored) overlay. Full runbook: docs/SOP-rust-setup.md §8.3.
set -euo pipefail

PORT="${1:-8787}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Workspace builds share a single target/ at the repo root (not src-tauri/target).
BUNDLE_DIR="$REPO_ROOT/target/release/bundle/macos"
LOCAL_CONF="src-tauri/tauri.local.conf.json"
BUNDLE_CONF="src-tauri/tauri.bundle.conf.json"
# Per-developer pubkey overlay — git-ignored, so no one person's key is baked into a
# committed config. Layered last so it wins over tauri.conf.json's production pubkey.
DEV_LOCAL_CONF="src-tauri/tauri.dev-local.conf.json"

if [[ "$(uname)" != "Darwin" ]]; then
  echo "dev-release.sh currently supports macOS only (the P1 dogfood platform)." >&2
  exit 1
fi
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  echo "TAURI_SIGNING_PRIVATE_KEY is not set; aborting (the updater artifact must be signed)." >&2
  echo "Use your own dev key -- you do not need the production release key:" >&2
  echo "  pnpm -C apps/desktop tauri signer generate -w ~/.tauri/flowforge-dev.key" >&2
  echo "  export TAURI_SIGNING_PRIVATE_KEY=\"\$(cat ~/.tauri/flowforge-dev.key)\"" >&2
  echo "  export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=\"…\"   # empty string if you set no password" >&2
  echo "Then put the matching PUBkey in $DEV_LOCAL_CONF (see docs/SOP-rust-setup.md §8.3)." >&2
  exit 1
fi

# Layer the dev pubkey overlay when the developer has one. Absent, the build keeps the
# committed production pubkey — fine if you hold the production key, but a dev-key-signed
# artifact would then fail signature verification at install time, so say so up front
# rather than let it surface as an opaque updater error after a full release build.
CONFIG_ARGS=(--config "$LOCAL_CONF" --config "$BUNDLE_CONF")
if [[ -f "$REPO_ROOT/apps/desktop/$DEV_LOCAL_CONF" ]]; then
  CONFIG_ARGS+=(--config "$DEV_LOCAL_CONF")
  echo "==> Using dev pubkey overlay $DEV_LOCAL_CONF"
else
  echo "note: no $DEV_LOCAL_CONF — building with the committed PRODUCTION pubkey." >&2
  echo "      The install will reject this bundle unless you signed it with the" >&2
  echo "      production key. See docs/SOP-rust-setup.md §8.3 to set up a dev key." >&2
fi

case "$(uname -m)" in
  arm64) PLATFORM="darwin-aarch64" ;;
  x86_64) PLATFORM="darwin-x86_64" ;;
  *) echo "unsupported arch $(uname -m)" >&2; exit 1 ;;
esac

# Version by the COMMITTER DATE of the built commit, not the build moment (#1034).
# `date +%s` made every rebuild look newer — even a rebuild of an older commit — so
# the app could never tell a genuinely newer build from a downgrade. Committer time
# is monotonic along history, so semver ordering of the `0.0.0-dev.<epoch>`
# prerelease is exactly commit recency, which is what the backend compares.
COMMIT_EPOCH="$(git -C "$REPO_ROOT" show -s --format=%ct HEAD)"
COMMIT_SHA="$(git -C "$REPO_ROOT" rev-parse --short HEAD)"
COMMIT_DATE="$(git -C "$REPO_ROOT" show -s --format=%cI HEAD)"
DEV_VERSION="0.0.0-dev.$COMMIT_EPOCH"
# The sha stays OUT of the version string (macOS bundle-version parsing is fussy and
# build metadata is semver-ignored anyway); it travels in the manifest notes instead,
# which the app surfaces so a dev can see exactly which commit they're installing.
if [[ -n "$(git -C "$REPO_ROOT" status --porcelain)" ]]; then
  echo "warning: working tree is dirty — the version describes commit $COMMIT_SHA," >&2
  echo "         not the tree actually built. Commit first for a truthful version." >&2
fi

echo "==> Building CLI sidecar binary (CLI.7)"
TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
mkdir -p "$REPO_ROOT/apps/desktop/src-tauri/binaries"
cargo build -p ff-cli --release
cp "$REPO_ROOT/target/release/flowforge" \
   "$REPO_ROOT/apps/desktop/src-tauri/binaries/flowforge-$TRIPLE"

echo "==> Building signed updater bundle, version $DEV_VERSION ($COMMIT_SHA, $COMMIT_DATE)"
cd "$REPO_ROOT/apps/desktop"
# Remove stale artifacts so we never accidentally serve an old tarball with a new version.
rm -f "$BUNDLE_DIR/FlowForge.app.tar.gz" "$BUNDLE_DIR/FlowForge.app.tar.gz.sig"
# `--bundles app,updater`, not `--bundles updater`: on macOS the updater artifact is a
# tarball OF the .app bundle, so asking for `updater` alone makes the current tauri CLI
# compile the binary and skip bundling entirely — no .app, no .tar.gz, no .sig, and no
# warning. That silently produced nothing until the artifact check below caught it.
pnpm tauri build --bundles app,updater "${CONFIG_ARGS[@]}" --config "{\"version\":\"$DEV_VERSION\"}"

TARBALL="$BUNDLE_DIR/FlowForge.app.tar.gz"
SIG="$TARBALL.sig"
if [[ ! -f "$TARBALL" || ! -f "$SIG" ]]; then
  echo "Expected updater artifacts not found: $TARBALL(.sig)" >&2
  exit 1
fi

echo "==> Writing latest.json"
SIGNATURE="$(cat "$SIG")" \
DEV_VERSION="$DEV_VERSION" \
COMMIT_SHA="$COMMIT_SHA" \
COMMIT_DATE="$COMMIT_DATE" \
PLATFORM="$PLATFORM" \
PORT="$PORT" \
python3 - "$BUNDLE_DIR/latest.json" <<'PY'
import json, os, sys, datetime
out = sys.argv[1]
manifest = {
    "version": os.environ["DEV_VERSION"],
    # Build identity (#1034): the app shows this so a dev can tell exactly which
    # commit a build is before installing it — especially for a deliberate downgrade.
    "notes": "Local dev build {} ({}).".format(
        os.environ["COMMIT_SHA"], os.environ["COMMIT_DATE"]
    ),
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
