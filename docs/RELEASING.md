# Releasing FlowForge

A release publishes **both** the headless `flowforge` CLI and the FlowForge
desktop (Tauri) app into a single GitHub Release, tagged `vX.Y.Z`. The desktop
build also ships a minisign-signed updater bundle + an auto-generated
`latest.json` feed so already-installed apps self-update (RFC 0014).

The whole thing is driven by [`.github/workflows/release.yml`](../.github/workflows/release.yml).

---

## 0. One-time prerequisites

These are done once and never again for normal releases.

- **Updater signing key.** A minisign keypair whose **public** key is committed
  in
  [`tauri.conf.json`](../apps/desktop/src-tauri/tauri.conf.json) under
  `plugins.updater.pubkey`:

  ```
  untrusted comment: minisign public key: 46352F8D142E7FEA
  RWTqfy4UjS81RkuS2Y2s7JWVUZkfsjQZq5xUQHL4l2CdZNPM9lcHyteH
  ```

- **Repo secrets.** The matching **private** key + its password live as GitHub
  repository secrets (Settings → Secrets and variables → Actions):

  - `TAURI_SIGNING_PRIVATE_KEY`
  - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

  The release workflow reads these to sign `FlowForge.app.tar.gz`. Without them
  the `desktop` job fails to sign and the updater feed is unusable — **verify
  both secrets exist before cutting your first release.**

- **Updater endpoint.** `tauri.conf.json` points the updater at
  `https://github.com/flowforge-lab/FlowForge/releases/latest/download/latest.json`.
  A published (non-draft, non-prerelease) Release makes that URL resolve.

> **Key custody.** The private key is the root of update trust. Keep it in a
> password manager out-of-band; losing it means no further updates to existing
> installs. (See RFC 0014 §4.)

---

## 1. Bump versions (in lockstep)

The desktop version lives in **two** files and **must** be bumped together:

| File | Field |
|------|-------|
| `apps/desktop/src-tauri/tauri.conf.json` | `version` |
| `apps/desktop/package.json` | `version` |

```bash
NEW=0.2.0
# from repo root
python3 - "$NEW" <<'PY'
import json, sys
v = sys.argv[1]
for p in ("apps/desktop/src-tauri/tauri.conf.json", "apps/desktop/package.json"):
    d = json.load(open(p))
    d["version"] = v
    json.dump(d, open(p, "w"), indent=2)
    open(p, "a").write("\n")
print(f"bumped -> {v}")
PY
```

Keep them identical. The CLI crates (`apps/cli`, `crates/*`) version
independently; only bump those if you are also shipping a CLI change in the same
tag.

Commit the bump:

```bash
git add apps/desktop/src-tauri/tauri.conf.json apps/desktop/package.json
git commit -m "release: vX.Y.Z"
```

---

## 2. Tag and push

```bash
git tag -a vX.Y.Z -m "FlowForge vX.Y.Z"
git push origin main        # make sure the tagged commit is on the remote
git push origin vX.Y.Z      # ⬅ this triggers release.yml
```

Pushing the `v*` tag runs the parallel build jobs plus `release-cli`, which
**assemble a single draft GitHub Release** — a green build does NOT reach
users:

- `build-cli` → `flowforge-<triple>.tar.gz` (macOS arm64, Linux x86_64)
- `desktop` (OS matrix: macos-14 / ubuntu-latest / windows-latest, tauri-action)
  → creates the Release **as a draft** and uploads the per-OS bundles: macOS
  `FlowForge_<v>_aarch64.dmg` + `FlowForge.app.tar.gz` (+ `.sig`), Linux
  `.deb`/`.rpm`/`.AppImage`, and Windows NSIS `FlowForge_<v>_x64-setup.exe`
  (+ `.sig`) + `FlowForge_<v>_x64_en-US.msi`. Each platform merges its entry
  into one generated `latest.json` (the Windows entry points at the NSIS
  setup, since MSI can't auto-update).
- `release-cli` → attaches the CLI archives + a consolidated `SHA256SUMS` and
  fills in auto-generated release notes (still a draft — nothing is published).

Because the Release stays a draft, `releases/latest/download/latest.json`
does **not** resolve and the in-app updater sees nothing until the draft is
published (next step).

> **Re-running.** To re-trigger for an existing tag without retagging, use
> *Actions → Release → Run workflow* and pass the `tag` input. The jobs refresh
> that tag's draft Release (artifacts are re-uploaded; it is not published).

### Publish the draft (the release gate)

A successful build leaves a **draft** Release. Drafts are invisible to
`releases/latest`, so the updater won't serve anything yet. Publishing is the
explicit, human gate:

1. Open the draft for `vX.Y.Z` in the **Releases** UI.
2. Confirm the asset list (next section) is complete and give the notes body a
   curated pass. The auto-generated notes are the baseline — regroup merged PRs
   since the last tag into **Features / Fixes / Internal** and paste it into the
   body. No in-CI LLM call or extra secret is needed; the draft gate makes this
   a normal edit-before-publish step.
3. Click **Publish**. Only now does `releases/latest/download/latest.json`
   resolve and the in-app updater pick up the release; then run the end-to-end
   in-app smoke (§4). To exercise updater plumbing *before* publishing, use the
   local dev channel (§5), which needs no published Release.

---

## 3. Verify the Release

> **Draft vs. published.** The `releases/latest/download/...` URLs below
> resolve only **after** the draft is published. To verify a draft *before*
> publishing, fetch assets with `gh release download vX.Y.Z` (or the Release
> UI) and run the same checks against those local files.

On the GitHub Release for `vX.Y.Z`, confirm these assets exist:

- `flowforge-aarch64-apple-darwin.tar.gz`
- `flowforge-x86_64-unknown-linux-gnu.tar.gz`
- `SHA256SUMS`
- `FlowForge_<v>_aarch64.dmg` (macOS)
- `FlowForge.app.tar.gz` + `FlowForge.app.tar.gz.sig` (macOS updater bundle)
- `FlowForge_<v>_amd64.deb` / `FlowForge-<v>.x86_64.rpm` / `FlowForge_<v>_amd64.AppImage` (Linux)
- `FlowForge_<v>_x64-setup.exe` + `FlowForge_<v>_x64-setup.exe.sig` (Windows NSIS updater bundle)
- `FlowForge_<v>_x64_en-US.msi` (Windows)
- `latest.json`

### latest.json

```bash
TAG=vX.Y.Z
curl -fsSL https://github.com/flowforge-lab/FlowForge/releases/latest/download/latest.json | python3 -m json.tool
```

`version` must equal the tag (sans leading `v`), and each platform entry the
release ships (`darwin-aarch64`, `linux-x86-64`, `windows-x86_64`) must have a
non-empty `.signature` + `.url`. The Windows `.url` points at the NSIS
`-setup.exe` (not the `.msi`).

### Signature verifies against the committed pubkey

```bash
# download the updater bundle + sig
curl -fsSL -O https://github.com/flowforge-lab/FlowForge/releases/latest/download/FlowForge.app.tar.gz
curl -fsSL -O https://github.com/flowforge-lab/FlowForge/releases/latest/download/FlowForge.app.tar.gz.sig

# reconstruct the minisign pubkey from tauri.conf.json and verify
python3 -c "import json,base64;print(base64.b64decode(json.load(open('apps/desktop/src-tauri/tauri.conf.json'))['plugins']['updater']['pubkey']).decode())" > flowforge.pub
minisign -Vm FlowForge.app.tar.gz -p flowforge.pub    # expect: Signature and comment signature verified
```

This proves the private key that produced the `.sig` matches the pubkey baked
into the app — exactly the check the updater performs at install time.

### CLI archives

```bash
curl -fsSL https://github.com/flowforge-lab/FlowForge/releases/latest/download/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
```

---

## 4. Smoke the in-app updater (end-to-end)

The updater only updates an **already-installed** app — the very first install
is a manual `.dmg` download (RFC 0014 §9).

1. **Install once.** Download `FlowForge_<v>_aarch64.dmg` from the Release,
   drag to `/Applications`. Until Apple notarization (phase 2) lands, clear the
   Gatekeeper quarantine flag the first time:

   ```bash
   xattr -dr com.apple.quarantine /Applications/FlowForge.app
   ```

2. **Check for updates.** Open FlowForge → Settings → About → **Check for
   updates**. It should find the version on `latest.json`.

3. **Update now.** Click **Update now**. The updater downloads
   `FlowForge.app.tar.gz`, verifies the `.sig` against the embedded pubkey,
   installs, and relaunches. A successful relaunch at the new version is the
   green light.

---

## 5. Local dev channel (no GitHub round-trip)

[`scripts/dev-release.sh`](../scripts/dev-release.sh) builds a signed updater
bundle locally and serves `latest.json` over `http://localhost`, so you can
exercise the *same* in-app Check / Update now path against
[`tauri.local.conf.json`](../apps/desktop/src-tauri/tauri.local.conf.json)
without cutting a real Release (RFC 0014 P1 / #370 dogfood loop).

```bash
# requires the minisign key in your env (same key whose pubkey is committed)
export TAURI_SIGNING_PRIVATE_KEY=$(cat ~/.tauri/flowforge.key)
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=...

# build + serve the local feed on :8787
./scripts/dev-release.sh

# in another terminal, point an installed app at the local feed and relaunch
FF_UPDATER_ENDPOINT="http://localhost:8787/latest.json" \
  /Applications/FlowForge.app/Contents/MacOS/FlowForge
```

Then Settings → About → Update now. This validates the updater plumbing
(isolated from Release mechanics) and is the fastest loop while iterating on the
updater itself. See `./scripts/dev-install.sh` for the no-updater day-to-day
build-and-replace loop.

---

## Notes

- **macOS first-install friction.** The build is Apple-unsigned until phase 2
  notarization. The one-time `xattr` bypass (above) is documented; the updater
  itself is unaffected (minisign verification is independent of Apple).
- **One Release, two surfaces.** If `desktop` fails, `release-cli` is skipped,
  so the draft stays desktop-less. Either way nothing auto-publishes (the draft
  gate), so a CLI-only updater feed can never reach users. Fix the `desktop` job
  and re-run.
- **Phase 2 (partially in place).** The desktop matrix now spans
  macOS/Linux/Windows (RFC 0014 P4 matrix). Still deferred: Developer ID signing
  + notarization and Windows Authenticode signing — additive: more secrets + a
  few `tauri.conf.json`/workflow flags, no contract change (RFC 0014 §10). Until
  those land, the macOS build still needs the one-time `xattr` Gatekeeper bypass
  and the Windows build trips SmartScreen on first install.
