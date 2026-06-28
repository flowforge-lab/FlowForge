# 0014 — Release & Self-Update

- **Status:** Proposed
- **Milestone:** _M4 (distribution)_
- **Author:** tonytan4ever
- **Depends on:** nothing hard. Tauri v2 `updater` + `process` plugins; GitHub Releases; complements RFC 0012 (durable persistence — the config this design deliberately does *not* back up). Fills in the About IPC stubs from #134/#158.
- **Tracking issue:** #159

## 1. Summary & Goals

Ship FlowForge as an **installable, self-updating desktop app** instead of a thing you
run with `pnpm tauri dev`. After a one-time install, Settings -> About gains a working
**Check for updates / Update now** button that pulls every new release. This is the real
backend behind the SET.11 About stubs (#134, PR #158), which today are mock no-ops.

The design is **local-first but public-ready**: the first milestone gets a handful of
developers (the author included) self-updating at zero cost; the same pipeline extends to
the general public by switching on OS code-signing later, with no rearchitecture.

Goals:

- Replace the `check_for_updates` mock stub with a real Tauri updater, returning the
  already-defined structured `UpdateStatus` (up-to-date vs. available + version + notes)
  so the UI can branch and offer "Update now".
- Stand up a tagged-release pipeline (GitHub Actions -> GitHub Releases) that produces a
  signed, installable macOS build and the updater manifest the app reads.
- Get the author off the dev server: install once, update in-app thereafter.
- Design the signing and packaging so the public path (Apple notarization, Windows
  signing, multi-platform matrix) is a later switch, not a redesign.

Non-goals up front (see §10): backup/restore (descoped — §8), Apple notarization,
Windows/Linux builds, auto-download-in-background, and delta updates.

## 2. Background — what exists, what doesn't

- **FE contract is already structured.** `lib/about.ts` defines `UpdateStatus = { kind:
  "upToDate"; version } | { kind: "available"; version; notes }` and `formatUpdateStatus`
  (FE owns the toast copy). `ipc.ts` exposes `checkForUpdates(): Promise<UpdateStatus>`
  with a `CONTRACT NOTE (SET.11)` pointing here. So the contract-redesign half of #159 is
  largely done; the `Promise<string>` in the issue body is stale.
- **No Rust backend.** `check_for_updates` / `export_backup` / `restore_backup` have no
  Rust implementation and are absent from `generate_handler!` — only `MockIpc` fulfils
  them.
- **Tauri v2**, plugins `opener` / `dialog` / `fs`. `tauri.conf.json` has no
  `plugins.updater`, `bundle.targets: "all"`, version `0.1.0`. No updater artifacts.
- **Repo is public**, `0` releases, one CI workflow (`ci.yml`: rust + web gates), no
  release workflow.
- **The only way to run the app today is `pnpm tauri dev`** against a local build.

## 3. Architecture — Tauri updater + GitHub Releases + minisign

The standard Tauri v2 self-update topology, which the public repo makes trivial:

```
  release tag (vX.Y.Z)
        |
        v
  GitHub Actions (release.yml, macOS runner)
        |  tauri-action: build + sign + publish
        v
  GitHub Release  ->  FlowForge.app.tar.gz + .sig + .dmg + latest.json
        ^                                                      |
        |  HTTPS GET (no auth - public repo)                   |
  installed app  <------ updater plugin reads latest.json -----+
        |  verifies .sig against baked-in minisign pubkey
        v
  download -> install -> relaunch
```

- **Update feed:** the Tauri updater polls a static **`latest.json`** manifest published
  as a release asset. Because the repo is public, the endpoint is a plain URL with no
  token: `https://github.com/flowforge-lab/FlowForge/releases/latest/download/latest.json`.
- **Flow (Tauri v2):** `app.updater()?.check()` -> `Option<Update>`. `None` => up-to-date;
  `Some(u)` => `u.version` + `u.body` (release notes). Install =
  `update.download_and_install(...).await` then `app.restart()` (via `tauri-plugin-process`).
- **Integrity:** every artifact is signed with a **minisign** key (see §4); the installed
  app carries the matching public key and refuses any update whose signature does not
  verify. This is independent of OS code-signing.

## 4. Signing — two independent layers

These are separate concerns and are often confused:

| Layer | What it protects against | Cost / account | Phase |
|-------|--------------------------|----------------|-------|
| **Updater signature (minisign)** | A malicious or corrupt *update* being installed. The app verifies the `.sig` against a baked-in public key before applying. | Free, no account. | **Required now** — the updater will not install an unsigned update. |
| **OS code-signing (Apple notarization / Windows Authenticode)** | The *first install* triggering Gatekeeper / SmartScreen "unknown developer / damaged app" warnings. | Apple Developer Program ($99/yr); a Windows cert. | **Phase 2** (public). |

**Minisign mechanics:**

1. Generate a keypair once: `pnpm tauri signer generate` -> private key (secret) + public
   key (not secret).
2. Public key is committed into `tauri.conf.json` (`plugins.updater.pubkey`); it ships in
   every install.
3. CI signs each artifact with the private key at release time, producing `.sig` files.
4. The private key + its password are stored as **GitHub Actions repository secrets**
   (`TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`) and consumed by the
   release workflow. They are never committed and never pushed through an API; they are
   pasted into the repo's Settings -> Secrets and variables -> Actions by a maintainer.

Losing the private key means the existing installed base can no longer be updated (a new
key requires a manual reinstall), so it is backed up out-of-band.

## 5. IPC contract

The existing `UpdateStatus` is unchanged. One command is **added** to make "Update now"
possible, since the SET.11 stub only modelled the *check*, not the *install*:

```ts
checkForUpdates(): Promise<UpdateStatus>;   // existing - now real
installUpdate():   Promise<void>;           // new - downloads, installs, relaunches
```

- `check_for_updates` (Rust): `app.updater()?.check()` -> map `None`/`Some` to
  `UpdateStatus`. Errors (offline, malformed manifest) surface as a normal `CmdResult` err
  the FE toasts.
- `install_update` (Rust): re-runs `check()` then `download_and_install()` then
  `app.restart()`. It re-checks rather than caching the `Update` handle across IPC calls —
  the handle is awkward to hold `'static` between invocations, and a second cheap check is
  robust against the user having sat on the dialog. Progress events are out of scope for
  v1 (the button shows a spinner; a `download-progress` event is a later nicety).
- `MockIpc.installUpdate` resolves immediately (no-op) so mock dev is unaffected.
- **About UI:** the "Update now" affordance renders only when `checkForUpdates` returned
  `kind: "available"`; otherwise the section shows the current version + "You're on the
  latest version."

The `backup` stubs (`export_backup` / `restore_backup`) are left as mock no-ops for now —
see §8.

## 6. Release workflow

A new `.github/workflows/release.yml`:

- **Triggers:** `push` of a `v*` tag (the real release path) **and** `workflow_dispatch`
  (so a maintainer can cut an on-demand build from `main` without tagging, for fast
  dogfooding).
- **Runner:** `macos-14` (Apple silicon / arm64) for the first milestone — the author's
  platform. The matrix expands to x86 macOS / Linux / Windows in phase 2.
- **Steps:** checkout -> setup pnpm + node + rust -> `pnpm install --frozen-lockfile` ->
  `tauri-apps/tauri-action@v0` with `projectPath: apps/desktop`, the two signing-key env
  vars, and `GITHUB_TOKEN`. tauri-action builds, creates/updates the GitHub Release for
  the tag, uploads the `.dmg` + `.app.tar.gz` + `.sig`, and **auto-generates `latest.json`**
  when `createUpdaterArtifacts` is on and the signing key is present.
- **Config changes (PR-1):** `bundle.createUpdaterArtifacts: true`;
  `plugins.updater.{endpoints, pubkey}`; `capabilities/default.json` gains
  `updater:default` and `process:allow-restart`.

The existing `ci.yml` (PR gates) is untouched.

### 6.1 Local / dev update channel

For dogfooding without cutting a real GitHub Release, the repo ships two
developer scripts (documented in `docs/SOP-rust-setup.md` section 8):

- **`scripts/dev-install.sh`** (D2) — the daily loop. Runs `pnpm tauri build`,
  replaces `/Applications/FlowForge.app`, and clears the macOS quarantine
  attribute. No update feed involved; you just relaunch the freshly built app.
- **`scripts/dev-release.sh`** (D1) — exercises the *update path itself*. Does a
  signed build, generates a `latest.json`, and serves it over
  `http://localhost:8787` so the in-app "Check for updates" / "Update now" flow
  can be tested end-to-end against a local feed.

D1 relies on a dev-only Tauri config overlay,
`apps/desktop/src-tauri/tauri.local.conf.json`, applied at build time via
`--config` (it is **never** part of the shipped `tauri.conf.json`):

- `plugins.updater.endpoints` -> `http://localhost:8787/latest.json`
- `plugins.updater.dangerousInsecureTransportProtocol: true` (lets the updater
  talk to a plain-HTTP localhost feed; the shipped config stays strict-HTTPS)
- `version: "0.0.0-dev"` so any real release version compares as "newer"

The backend honors an `FF_UPDATER_ENDPOINT` env override and uses a lenient
`version_comparator` (`update.version != current`) so a local feed can push the
same or an older version for testing. Production builds use neither override and
keep the strict-HTTPS, monotonic-version behavior.

## 7. Versioning

- **Single source of truth:** `tauri.conf.json` `version`. `apps/desktop/package.json`
  `version` is kept in lockstep (both `0.1.0` today). `getAppVersion()` already reads the
  Tauri metadata at runtime.
- **Convention:** bump the version in both files, commit, tag `vX.Y.Z`, push the tag ->
  the workflow releases. The updater compares the running app's version against
  `latest.json`'s `version`; a newer manifest version => `available`.
- SemVer: pre-1.0, breaking changes bump the minor. A `RELEASING.md` (PR-2) documents the
  bump-tag-push steps and the first-install instructions.

## 8. Why backup is descoped

`~/.config/flowforge/` (sessions.db, provider-registry.json, search.json,
tool_permissions.json, phenotype.json, mode.json) and `~/.flowforge/` (skills, phenos,
memory, mcp.json) live in the **user's home directory, not the app bundle**. Installing or
self-updating the app replaces only the bundle; it never touches these. So all local state
**survives updates and reinstalls for free** — which is the only durability the dogfooding
goal needs.

Backup/restore (`export_backup` / `restore_backup`) only adds value for *new-machine
migration* or *corruption recovery*, neither of which is in scope for self-update. It also
carries real design weight: a WAL-safe `sessions.db` copy (`VACUUM INTO` over the live
connection), a versioned archive format, and the explicit decision that **keychain secrets
are excluded** (they are not on disk, and excluding them is the safer default — a restored
backup on a new machine re-prompts for API keys). Those tasks stay on #159 as a deferred
checkbox; the mock stubs remain until then. This keeps the milestone to "self-update,
done well".

## 9. macOS first-install friction (and how phase 2 removes it)

Until Apple notarization (phase 2), the macOS build is unsigned by Apple, so Gatekeeper
flags the first install ("FlowForge can't be opened / is damaged"). For local developers
this is a documented one-time bypass: right-click -> Open, or
`xattr -dr com.apple.quarantine /Applications/FlowForge.app`. The updater itself is
unaffected — minisign verification (independent of Apple) guarantees update integrity, and
updater-delivered bundles inherit the same trust the user already granted.

**The first install is always a manual download** of the `.dmg` from the GitHub Release —
the updater only updates an *already-installed* app. After that, the in-app button is the
only step. `RELEASING.md` documents both.

Phase 2 (Developer ID signing + notarization) removes the Gatekeeper prompt entirely and
is purely additive: more secrets + a few `tauri.conf.json`/workflow flags, no contract or
architecture change.

## 10. Phasing

| Phase | Label | Scope | Ships alone? |
|-------|-------|-------|--------------|
| **P1** | backend + plumbing | `tauri-plugin-updater` + `tauri-plugin-process`; `check_for_updates` + `install_update` commands (+ `generate_handler!`); updater config (`endpoints`, `pubkey`, `createUpdaterArtifacts`); capability perms; FE `installUpdate` + "Update now" button; minisign keypair generated, pubkey committed, private key into repo secrets. | No (needs P2 to have a feed) |
| **P2** | release pipeline | `release.yml` (tag + `workflow_dispatch`, macOS arm64, tauri-action) + `RELEASING.md`; cut `v0.1.0`. **After P2 the author installs once and self-updates.** | Yes (the deliverable) |
| **P3** *(deferred)* | backup | Real `export_backup` / `restore_backup` (§8). Stays on #159. | Yes |
| **P4** *(deferred)* | public hardening | Apple notarization, Windows signing, multi-platform matrix; real Slack invite URL (swap `ABOUT_SLACK_URL`). | Yes |
| **P5a** | global update bar | Promote the "Update now" affordance to a global, full-width app bar (§12.1). FE-only; reuses the existing global `useUpdateStore`. | Yes |
| **P5b** | download progress | Backend emits a `download-progress` event; FE renders a determinate/indeterminate progress bar in the global bar and Settings → About (§12.2). | Yes |
| **P5c** | local dev channel | Experimental opt-in that lets a dev build poll a localhost feed so the running app picks up a fresh `dev-release.sh` build via the same bar (§12.3). FE-only. | Yes |

Dependency: **P1 -> P2**. P3 and P4 are independent and unscheduled. P5a/P5b/P5c
build on the shipped P1/P2 spine and are mutually independent (P5b enriches the bar
P5a introduces, but P5a does not block on it).

## 11. Non-goals & open questions

**Non-goals:**

- **Not silent background download/install.** A background *check* on startup
  and on an interval (gated to production builds) is in scope (#363) — it only
  *surfaces* the "Update now" button. The user always clicks "Update now" to
  download and install; nothing is fetched or applied without that click.
- **Not delta/differential updates.** Each update is a full bundle. Fine at this size.
- **Not a private/auth'd update feed.** The repo is public; the endpoint is a plain URL.
  If the repo ever goes private, the updater needs a token or a public mirror — flagged,
  not solved.
- **Not backup/restore** (§8) and **not OS code-signing** (§4) in this milestone.

**Open questions:**

- **Download progress UX.** _Resolved — see §12.2._ v1 shipped a spinner; the
  `download-progress` event is now wired into a real progress bar.
- **Surfacing location.** _Resolved — see §12.1._ The "Update now" affordance lived only
  in Settings → About; it is promoted to a global, full-width app bar so an available
  update is visible without opening Settings.
- **On-demand `main` builds / dogfood loop.** _Partially resolved — see §12.3._ A
  developer building FlowForge with FlowForge wants the running app to pick up a fresh
  local build without a manual reinstall. The local dev update channel (§12.3) makes the
  global bar fire against a localhost feed (`dev-release.sh`), gated behind an Experimental
  opt-in so a plain `pnpm tauri dev` process never polls a release feed. The `-dev`
  version-suffix convention from the original question is satisfied by
  `dev-release.sh` (`0.0.0-dev.<epoch>`) plus the lenient `version_comparator`.
- **Key custody.** Where does the minisign private key live out-of-band (the loss case in
  §4)? Proposal: the maintainer's password manager; revisit if the project gains more
  maintainers.

## 12. Amendment — global update bar, progress, and local-dev channel

P1/P2 shipped the self-update spine: a prod-gated background check populates a
global `useUpdateStore`, and Settings → About renders "Update now" when an update is
available (#362/#363/#364, PR #409). This amendment promotes that affordance to an
app-level surface, gives it a real progress bar, and adds a dogfood loop for
developers who build FlowForge with FlowForge. All three reuse the existing
`useUpdateStore`; the button is source-agnostic (a GitHub release and a local build
both resolve to `status.kind === "available"`).

### 12.1 Global update bar (P5a, FE-only)

Today the only path to "Update now" is opening Settings → About. P5a renders a slim,
full-width bar at the **top of the whole window** (above `<main>`, so it spans the
session sidebar, the chat pane tree, and the split panel) whenever
`useUpdateStore.status?.kind === "available"`. This matches the conventional
"restart to update" bar pattern (browsers, editors, chat apps) and reads as truly
app-level rather than chat-level.

- **Placement:** `apps/desktop/src/components/app-shell.tsx`, above the `<main>`
  element. The existing `bootstrapError` banner (chat-column scoped) is the visual
  precedent; the update bar is its window-wide sibling.
- **Content:** "FlowForge `<version>` is available" + an **Update** button →
  `useUpdateStore.install()`, plus a dismiss control. Dismiss is session-local (a
  `dismissed` flag in the store) so the bar does not nag; it reappears on the next
  poll or launch while the update is still available.
- **Settings → About is retained.** It stays the manual / debug path and remains the
  only surface that toasts feed errors (the background poll swallows them). Both read
  the one store — no duplicated install logic.
- **One-click install + auto-relaunch.** Clicking Update calls the existing
  `install_update`, which downloads, installs, and calls `app.restart()`. This is not
  an in-process hot reload (infeasible for a native Rust binary holding live DB
  connections and MCP child processes — see §12.4); it is a one-click
  install-and-relaunch that removes the manual reinstall step. Local state in
  `~/.config/flowforge` / `~/.flowforge` survives the relaunch (§8), so the app
  reopens to roughly where it was. (Optional: restore the active session/pane after
  relaunch to make the restart near-invisible — a small additive nicety, not
  required for P5a.)

### 12.2 Download progress (P5b, backend + FE)

The backend currently discards updater progress:
`download_and_install(|_chunk, _total| {}, || {})` (`lib.rs:1618`). The callback
already receives bytes-this-chunk and an `Option<u64>` content length; P5b emits them
instead of dropping them.

- **Backend:** in `install_update`, replace the no-op callback with one that emits a
  Tauri event (e.g. `update://progress` carrying `{ downloaded, total }`) per chunk,
  and a terminal event when the download completes. Contained change; no contract
  change to `check_for_updates` / `install_update`.
- **FE:** `lib/events.ts` (the existing one-time event-wiring point) listens and
  writes `progress: { downloaded, total } | null` into `useUpdateStore`. The global
  bar (and Settings → About) render a **determinate** progress bar when `total` is
  known and an **indeterminate** one when content length is absent (it can be).
- Out of scope: delta updates, pause/resume.

### 12.3 Local dev update channel (P5c, FE-only)

Goal: a developer iterating on FlowForge runs `dev-release.sh` (D1, §6.1) and the
**already-running app picks up the new build via the same global bar** — no manual
`dev-install.sh` + relaunch. The backend already supports this feed
(`FF_UPDATER_ENDPOINT` + lenient `version_comparator`, `lib.rs:1585`); the missing
piece is purely the FE poll gate.

- Today the background poll is `import.meta.env.PROD`-only (`App.tsx:28`) so a dev
  build never polls — deliberately, to avoid a `pnpm tauri dev` process surfacing a
  real GitHub release over itself.
- P5c adds one Experimental flag, `localUpdateChannel`, to `store/experimental.ts` +
  a row in `experimental-section.tsx` (default off, clearly dev-only, mirroring the
  existing flags). The poll condition becomes:
  `import.meta.env.PROD || (import.meta.env.DEV && flags.localUpdateChannel)`.
- **Safety guard:** the dev branch is meaningful only when a **local** feed is set
  (`FF_UPDATER_ENDPOINT`). With the flag on but no local feed configured,
  `check_for_updates` simply returns up-to-date, so a dev process never reaches the
  public GitHub feed. The flag + a running `dev-release.sh` feed are the two
  conditions for the dev bar to fire.
- Note: this pairs with `dev-release.sh` (D1, a local updater feed), **not**
  `dev-install.sh` (D2, a direct `/Applications` file swap with the updater disabled
  and no feed — the in-app bar cannot apply to it; D2 stays a manual relaunch loop).

### 12.4 Why not in-process hot reload

True hot reload of the running app is explicitly out of scope:

- **Frontend** changes are already hot-reloaded by Vite HMR under `pnpm tauri dev`;
  no feature is needed for the FE inner loop.
- **Backend (Rust)** changes cannot be swapped into a running native binary. The only
  Rust hot-reload approaches (`hot-lib-reloader`, `dexterous-developer`) require
  carving logic into a `dylib` behind an FFI-safe boundary that holds no state across
  reloads — incompatible with FlowForge's live SQLite connections, tokio runtime, and
  spawned MCP child processes, and they drop exactly the state we want preserved. The
  standard Rust loop (`pnpm tauri dev` auto-rebuild + auto-restart) is the closest the
  ecosystem offers. The one-click install + auto-relaunch (§12.1) is the right tool
  for picking up a built bundle without a manual reinstall.
