# FlowForge Settings — Sectioned Information Architecture (Epic `SET`)

## Context

FlowForge's Settings today is a 320px right slide-over
([`apps/desktop/src/components/settings-panel.tsx`](../../apps/desktop/src/components/settings-panel.tsx))
with three inline blocks only: theme/font (`theme-settings.tsx`, landed in #62/#78), web-search
backend picker (`search-settings.tsx`, landed in #84/#93), and an empty "LLM provider (#8)"
placeholder. There is no navigation, no grouping, and no room for the dozen settings surfaces the
app now needs (model, skills, MCP, permissions, memory, scheduled tasks, etc.).

The target structure is a centered modal with a left nav split into two
groups — **PROFILE** (Model, Skills, Control) and **GLOBAL** (Appearance, Profiles, Memory,
Scheduled, Keyboard, Experimental, About) — most sections carrying sub-tabs. We want that
**information architecture**, not a visual restyle.

### Hard design constraint (read before every issue)

> **Do not restyle FlowForge.** Keep the existing color tokens, accent (`--primary`), typography,
> theme system, and component styling exactly as they are today. We are reproducing the reference
> **layout / navigation / section structure** only. Concretely: reuse the existing selected-state
> pattern (`border-primary bg-primary/5 ring-2 ring-primary/30`), neutral surfaces
> (`bg-card`, `bg-muted/50`, `border-border`), and text scale (`text-[13px]`/`text-[12px]`/
> `text-[11px]`, `text-foreground`/`text-muted-foreground`/`text-destructive`) already used in
> `theme-settings.tsx` and `search-settings.tsx`. No new accent colors, no new spacing scale — match
> FlowForge's existing tokens. The reference IA informs *what fields exist and how they're grouped*, never the palette.

### Scope & delivery model

- **FE-first.** Each issue ships the complete React UI. Where a backend already exists, wire to the
  real IPC (theme → `prefs.ts`; web-search → `search-config.ts`; skills → `skills.ts`; provider →
  `ProviderConfig` from #49). Where it doesn't, wire to **mock data** by extending
  `apps/desktop/src/lib/mock.ts` + `apps/desktop/src/lib/ipc.ts`, and file the backend as a Tony
  follow-up — never a blocker.
- **Freeze the IPC contract per issue.** Each issue defines its command shapes in `ipc.ts` +
  bindings before the backend exists, mirroring how web-search went #84 → #93. This is how we avoid
  UI churn when Rust lands.
- **One issue → one branch → one PR, sequential.** `SET.1a` (shell) unblocks everything; ship it
  first. `SET.1b` (shared primitives) can run in parallel with `SET.2`–`SET.4` (those issues consume
  the primitives but can stub locally until `SET.1b` merges, then swap imports). Branch naming
  follows convention, e.g. `feat/set-1a-settings-shell`. PR titles: `feat(desktop): <summary> (SET.x)`.
- **Labels:** `frontend`, `design`, `enhancement`. Add `backend` only on issues that also touch Rust.

### Reset-to-defaults contract (applies to every section)

The shell (`SET.1a`) renders a persistent footer with a **"Reset to defaults"** button and an
optional per-section reset handler registered via context/registry. In `SET.1a` the button renders
**disabled** (no section wires it yet). **Every downstream section issue is responsible for wiring
its own `reset()`** — when the active section provides a reset handler, the footer button enables and
calls it; when it doesn't, the button stays disabled. A section is not "done" until its reset is
wired and tested.

### Shared building blocks

Two issues create these; everything else consumes them.

**`SET.1a` creates** (shell + structure, no new primitives):
- `settings/settings-shell.tsx` — centered modal (overlay, focus trap, Esc-close, footer reset slot).
- `settings/section-nav.tsx` — left nav with `PROFILE` / `GLOBAL` group headers + nav items.
- `settings/coming-soon.tsx` — placeholder rendered by every not-yet-built section.
- `settings/registry.ts` — the section id union + nav metadata (see below).
- `settings/section.tsx` — layout helpers `<SettingsSection>` / `<SettingsRow>` (label +
  description + control slot), matching the heading/spacing in the two existing settings files.

**`SET.1b` creates** (reusable form primitives, built on `radix-ui`, already a dependency):
- `settings/switch.tsx` — on/off toggle (**new primitive**, `radix-ui` `Switch`).
- `settings/slider.tsx` — labeled range with value readout (**new primitive**, `radix-ui` `Slider`).
- `settings/segmented-control.tsx` — generic pill row (extract the inline pattern in
  `theme-settings.tsx`).
- `settings/sub-tabs.tsx` — horizontal segmented sub-tab bar (e.g. `Theme | Notifications | Advanced`).

> When an issue says "reuse the shared X", it means the component above — do not re-implement. Until
> `SET.1b` merges, a consuming issue may inline a minimal local version and swap to the shared import
> in a follow-up commit.

### Section registry (single source of truth)

`SET.1a` defines the section id union **only** in `settings/registry.ts` — never re-declared ad hoc
elsewhere. Consumers import `SettingsSectionId` from the registry.

```ts
type SettingsSectionId =
  | "model" | "skills" | "control"            // PROFILE group
  | "appearance" | "profiles" | "memory"      // GLOBAL group
  | "scheduled" | "keyboard" | "experimental" | "about";
```

> Note the GLOBAL item is `"keyboard"` (label "Keyboard"), **not** "Shortcuts" — the Skills section
> has a "Shortcuts" sub-tab for `/name` message shortcuts, a completely different concept. Keeping the
> nav label distinct avoids two unrelated "Shortcuts" one click apart.

`store/settings.ts` gains `activeSection: SettingsSectionId` (default `"appearance"`) plus
`setSection(id)`. Sections not yet built render `<ComingSoon>` so the nav is fully populated day one.

---

## Issues

> Format per issue: **ID / branch / labels / depends-on**, then **Why**, **Build**, **Files**,
> **State & types**, **IPC**, **Acceptance criteria**, **Tests**, **Out of scope**. Each issue is
> self-contained — implementable without reading the others, beyond the shared building blocks.

---

### SET.1a — Settings shell: centered modal + two-group sectioned nav (critical path)

- **Branch:** `feat/set-1a-settings-shell` · **Labels:** `frontend`, `design` · **Depends on:** none
- **Why:** Today's slide-over has no navigation and can't host 10 sections. Every other `SET` issue
  needs the shell, nav, and registry. Kept deliberately small (no new primitives) so it merges fast
  and unblocks the epic.
- **Build:**
  - Replace the right `aside` slide-over with a **centered modal**: full-viewport scrim
    (`bg-background/60 backdrop-blur-[1px]`, click-to-close — reuse the existing overlay button), and
    a centered dialog (~`max-w-3xl`, `max-h-[85vh]`, `rounded-xl border bg-background shadow-xl`).
    Port the focus-restore logic verbatim from `settings-panel.tsx` (lines 19–24); keep
    `role="dialog"`, `aria-label="Settings"`, Esc-to-close.
  - **Left nav** (`section-nav.tsx`): heading "Settings", group header `PROFILE` → Model / Skills /
    Control, group header `GLOBAL` → Appearance / Profiles / Memory / Scheduled / **Keyboard** /
    Experimental / About. Each item: lucide icon + label; active item uses the existing selected
    pattern. Group headers `text-[11px] uppercase tracking-wide text-muted-foreground`.
  - **Content pane:** header row (section title + close `X`), scrollable body (reuse
    `@/components/ui/scroll-area`), and the persistent **footer reset slot** (button rendered
    **disabled** per the reset-to-defaults contract above).
  - **Migrate existing settings:** move the current Theme/Font UI (`theme-settings.tsx`) and the
    web-search picker (`search-settings.tsx`) into the **Appearance** section content so nothing
    regresses. All other sections render `<ComingSoon>`.
  - Preserve open/close wiring: `Ctrl/Cmd+,` and any `palette.ts` "Open settings" command keep
    working (they call `useSettingsStore`).
- **Files:**
  - Create: `settings/settings-shell.tsx`, `settings/section-nav.tsx`, `settings/coming-soon.tsx`,
    `settings/registry.ts`, `settings/section.tsx`, `settings/appearance-section.tsx` (wrapper hosting
    the migrated Theme/Search UI).
  - Replace: `components/settings-panel.tsx` → thin wrapper rendering `<SettingsShell>` when open.
  - Modify: `store/settings.ts` (add `activeSection` + `setSection` + an optional registered
    `resetHandler` slot for the footer).
- **State & types:** `SettingsSectionId` + nav metadata + the reset-handler registration API live in
  `settings/registry.ts` / `store/settings.ts`.
- **IPC:** none new.
- **Acceptance criteria:**
  - Opening settings shows a centered modal (not a right drawer) with the full two-group nav; GLOBAL
    item reads "Keyboard".
  - Nav click swaps + highlights the pane; Appearance shows existing theme/font + web-search working
    as before.
  - Esc and scrim-click close; focus returns to the prior element; `Ctrl/Cmd+,` toggles open.
  - Footer reset button present but disabled; no visual restyle of the rest of the app.
- **Tests:** `settings/registry.test.ts` (union/order); render test that the shell mounts, nav lists
  all 10 items across the two groups, and `setSection` switches panes. Existing `theme`/`search`
  tests stay green.
- **Out of scope:** new form primitives (`SET.1b`); any section content beyond Appearance.

---

### SET.1b — Shared settings form primitives

- **Branch:** `feat/set-1b-settings-primitives` · **Labels:** `frontend`, `design` ·
  **Depends on:** `SET.1a` (may develop in parallel; merge after)
- **Why:** Switch / Slider / SegmentedControl / SubTabs are used across SET.2–SET.11. Build once,
  consistently tokenized, instead of re-implementing per section.
- **Build:** the four primitives in "Shared building blocks → SET.1b creates", each styled strictly
  with existing FlowForge tokens (no new palette). Provide accessible labels, keyboard operation, and
  `aria-pressed`/`role` where relevant. Include a tiny demo/usage story in a test.
- **Files:** create `settings/switch.tsx`, `settings/slider.tsx`, `settings/segmented-control.tsx`,
  `settings/sub-tabs.tsx`.
- **State & types:** none (controlled components; props in/out).
- **IPC:** none.
- **Acceptance criteria:** each primitive renders, is keyboard-operable, respects disabled state, and
  uses only existing tokens; light/dark both correct.
- **Tests:** `settings/primitives.test.tsx` — interaction + a11y attribute assertions per primitive.
- **Out of scope:** wiring into any section (that happens in the consuming issues).

---

### SET.2 — Appearance section (Theme / Notifications / Advanced sub-tabs)

- **Branch:** `feat/set-2-appearance` · **Labels:** `frontend`, `design` · **Depends on:** `SET.1a`
  (uses `SET.1b` `sub-tabs`/`slider`/`switch`)
- **Why:** Appearance currently only has theme/font/web-search. The reference IA groups appearance into three tabs
  and adds display/notification controls. Extends #62/#78.
- **Build:** `sub-tabs` bar **Theme / Notifications / Advanced**.
  - **Theme:** Mode segmented control Light/Dark/System (reuse `prefs.ts` `setTheme`); Font picker
    (existing `FONTS`); **Font size** slider (`fontScale`, 80–140%, applies a CSS scale var on
    `<html>` the way `applyTheme` is wired); **Display name** text input (`displayName`, overrides
    the name on sent messages; blank = system alias). Keep the web-search picker here (or as an
    Advanced sub-block — keep it reachable).
  - **Notifications:** master **Notifications** switch + children **Message complete**, **Approval
    requests**, **Sound**. FE-only flags.
  - **Advanced:** **Open threads** slider (max threads kept loaded, LRU; 3–20, default 10) — FE flag
    until backend consumes it.
- **Files:** create `settings/appearance/theme-tab.tsx`, `notifications-tab.tsx`, `advanced-tab.tsx`;
  update `settings/appearance-section.tsx` to host the tabs. Modify `store/prefs.ts`.
- **State & types:** extend `PrefsState` (persisted `ff-prefs`) with `fontScale: number`,
  `displayName: string`, `notifications: { enabled; messageComplete; approvalRequests; sound }`,
  `openThreads: number`, setters, and `resetAppearance()`. Apply `fontScale` via a subscribe
  side-effect (mirror `applyTheme`/`applyFont`).
- **IPC:** none (localStorage).
- **Acceptance criteria:** all three tabs render; theme/font live-apply; font-size visibly scales and
  persists across reload; notification + open-thread toggles persist; footer reset resets **only**
  Appearance.
- **Tests:** `store/prefs.test.ts` additions (new fields + hydration of old blobs lacking new keys);
  a `fontScale` apply unit test.
- **Out of scope:** firing real OS notifications / enforcing open-thread LRU (backend follow-up).

---

### SET.3 — Model section (PROFILE)

- **Branch:** `feat/set-3-model` · **Labels:** `frontend`, `design` · **Depends on:** `SET.1a`
  (uses `SET.1b` `switch`/`slider`/`segmented-control`)
- **Why:** No UI to pick the chat model or reasoning controls. #8 (open) + #49 (landed Phase-1
  `ProviderConfig` contract) give the shape to bind to.
- **Build:** single-pane section (no sub-tabs): **Chat model** dropdown (from `ProviderKind`
  `candleVllm | ollama` + `ProviderConfig.model`); **Thinking** switch; **Effort** segmented control
  Low/Medium/High; **Summarization threshold** slider (≈50k–300k, readout "150k"); footer reset.
- **Files:** create `settings/model-section.tsx`, `store/model-config.ts`; modify `lib/ipc.ts` +
  `lib/mock.ts` (provider get/set); map `model` → section in `registry.ts`.
- **State & types:** `store/model-config.ts`: `{ provider: ProviderConfig, thinking: boolean,
  effort: "low"|"medium"|"high", summarizationThreshold: number }` + `resetModel()`. Provider
  round-trips via IPC; reasoning controls persist locally (`persist`, key `ff-model`) until a backend
  field exists.
- **IPC:** add `ipc.getProviderConfig()` / `ipc.setProviderConfig(cfg)`. Call the #49 Rust commands
  if present; else **mock** in `mock.ts` returning `{ kind: "candleVllm", model: "<default>",
  hasKey: false }` and echoing writes. Mirror `search-config.ts`.
- **Acceptance criteria:** model dropdown lists providers/models and persists via IPC (mock echoes on
  reopen); Thinking/Effort/Threshold persist; reset works.
- **Tests:** `store/model-config.test.ts`; `mock.model.test.ts` (round-trip, mirroring
  `mock.search.test.ts`).
- **Out of scope:** API-key entry / hosted providers (#8); real summarization wiring.

---

### SET.4 — Control section (Permissions / Prompts sub-tabs)

- **Branch:** `feat/set-4-control` · **Labels:** `frontend`, `design` · **Depends on:** `SET.1a`
  (uses `SET.1b` `sub-tabs`/`switch`)
- **Why:** Default approval mode + per-profile prompt config have no surface. **Scope reduced from the
  original four-tab proposal** — Team and per-profile UI customization are deferred to `SET.12`.
- **Build:** `sub-tabs` **Permissions / Prompts**.
  - **Permissions:** a **Default Mode** presentation — columns Plan (Read Only) / Auto (Balanced) /
    Act (Full Access); rows Read & browse / Local writes / External changes / Dangerous commands;
    cells ✓ / ✗ / "Ask". Selecting a column sets the default mode.
    > **Backend-mapping caveat (must be in the issue body):** the existing `ApprovalSafety` binding is
    > only `"write" | "dangerous"` — it does **not** map to this 4-row × 3-column matrix. So the matrix
    > is **FE-only presentation** for now: it stores a `defaultMode` choice and a per-row policy map,
    > but does not yet drive runtime approval. Freeze the contract in `ipc.ts` and leave a clear TODO
    > linking to whatever issue extends `ApprovalSafety`. Do not invent backend semantics here.
    Below the matrix: **Custom Overrides** — collapsible Denied / Require Approval / Allowed lists
    with add/remove and counts (FE state, mock-persisted).
  - **Prompts:** **Inject memory** switch; **User instructions** editor (textarea, file-backed
    `user_instructions.md`); **Additional prompt files** list with add/remove (e.g.
    `{workspace}/AGENTS.md`).
- **Files:** create `settings/control/permissions-tab.tsx`, `prompts-tab.tsx`, and
  `settings/control-section.tsx`; `store/control-config.ts`; modify `ipc.ts`/`mock.ts`.
- **State & types:** `store/control-config.ts`: `{ defaultMode: "plan"|"auto"|"act",
  permissionPolicy: Record<PermissionRow, "allow"|"deny"|"ask">,
  overrides: { denied: string[]; requireApproval: string[]; allowed: string[] },
  injectMemory: boolean, userInstructions: string, promptFiles: string[] }` + `resetControl()`.
- **IPC:** mock get/set for control config; define command names in `ipc.ts` so backend has a stable
  target.
- **Acceptance criteria:** both tabs render; selecting a default mode updates the matrix highlight;
  overrides add/remove; prompts persist through mock IPC across reopen; reset works. Issue body
  documents the `ApprovalSafety` caveat.
- **Tests:** `store/control-config.test.ts`; `mock.control.test.ts` round-trip.
- **Out of scope:** runtime permission enforcement (backend); Team + UI customization (`SET.12`).

---

### SET.5 — Skills section (Installed / Marketplace / MCP Servers / Shortcuts sub-tabs)

- **Branch:** `feat/set-5-skills` · **Labels:** `frontend`, `design` · **Depends on:** `SET.1a`,
  **and a resolved boundary with #91** (see below) · uses `SET.1b` `sub-tabs`
- **Why:** Skills are managed only via the ⌘K palette (#64/#83). A settings home for install/browse,
  MCP servers, and `/name` message shortcuts is missing.
- **#91 boundary (resolve before coding):** #91 (M4.4) is Abid's own "FE server-status panel."
  **Decision to confirm with Abid at kickoff, default position:** the Skills → **MCP Servers** sub-tab
  owns *configuration* (add server: command/URL + `stdio` transport + Advanced + Import JSON, and the
  list of configured servers), and **embeds the #91 status panel component** for *live status*
  (health, restart state) rather than re-implementing it. #91 ships the `<McpServerStatus>` panel as a
  reusable component; SET.5 imports it. If #91 hasn't landed, SET.5 renders a status placeholder behind
  the same props so the swap is a one-line import. Record the agreed split in both issue bodies before
  either starts to avoid a merge collision.
- **Build:** `sub-tabs` **Installed / Marketplace / MCP Servers / Shortcuts**.
  - **Installed:** "Install local skill…"; a **Bundled** read-only group with a count badge,
    expandable. Source from `store/skills.ts` (`listSkills`) + `SkillInfo`.
  - **Marketplace:** search input + result cards (skeleton while loading). Mock catalog in `mock.ts`.
  - **MCP Servers:** configuration form + configured-server list (per the #91 boundary above), using
    `McpServerConfig`/`McpServerState`/`McpServerStatus` bindings.
  - **Shortcuts:** "Create a Shortcut" — `Name` + `Message` → Create; lists existing `/name` message
    shortcuts (send a message on `/name`; **not** system-prompt injection; **not** the GLOBAL
    "Keyboard" section).
- **Files:** create `settings/skills/installed-tab.tsx`, `marketplace-tab.tsx`, `mcp-tab.tsx`,
  `shortcuts-tab.tsx` + `settings/skills-section.tsx`; `store/command-shortcuts.ts`; modify
  `ipc.ts`/`mock.ts`. Reuse `store/skills.ts` as-is; import #91's status component.
- **State & types:** `store/command-shortcuts.ts`: `{ shortcuts: { id; name; message }[] }` with
  add/remove, persisted (`ff-command-shortcuts`). MCP list via existing bindings/IPC (mock if absent).
- **IPC:** reuse skill IPC; add mock `listMcpServers`/`addMcpServer` if absent; add mock
  `searchSkillMarketplace(query)`.
- **Acceptance criteria:** Installed shows bundled skills + count; Marketplace search returns mock
  results; MCP add-form validates + lists a server; `/name` shortcut create persists; live MCP status
  comes from #91's component (or its placeholder). Reset wired.
- **Tests:** `store/command-shortcuts.test.ts`; `mock.skills.test.ts` extension for marketplace/MCP.
- **Out of scope:** real MCP supervisor lifecycle (M4.2 #89); real marketplace backend; re-building
  #91's status UI.

---

### SET.6 — Keyboard section (keyboard-shortcut reference)

- **Branch:** `feat/set-6-keyboard` · **Labels:** `frontend` · **Depends on:** `SET.1a`
- **Why:** The help overlay (#20/#65, `shortcuts-overlay.tsx`) already has the data; surface it inside
  Settings too. Low-risk re-presentation. (Nav label "Keyboard" — see registry note.)
- **Build:** read-only reference grouped **Preferences / General / Navigation**, sourced from the
  existing `lib/shortcuts.ts` registry. Add a **Send message** preference toggle Enter / Ctrl+Enter
  (persist to prefs). Render `kbd`-style chips matching the existing overlay styling.
- **Files:** create `settings/keyboard-section.tsx`; reuse `lib/shortcuts.ts`; add
  `sendMessageKey: "enter" | "ctrlEnter"` to `store/prefs.ts`.
- **State & types:** `sendMessageKey` in `PrefsState`; composer send handler reads it.
- **IPC:** none.
- **Acceptance criteria:** all groups render from the single registry (no duplicated list); toggling
  Send-message preference changes composer behavior and persists.
- **Tests:** `store/prefs.test.ts` addition; assert the section derives from `lib/shortcuts.ts`.
- **Out of scope:** user-rebindable shortcuts.

---

### SET.7 — Profiles section (Installed / Marketplace sub-tabs)

- **Branch:** `feat/set-7-profiles` · **Labels:** `frontend`, `design` · **Depends on:** `SET.1a`
  (uses `SET.1b` `sub-tabs`)
- **Why:** Phenotypes/Profiles exist in the model layer (#30/#69) but have no management surface.
- **Build:** `sub-tabs` **Installed / Marketplace**. Installed: profile cards (name, description,
  skill count, lock icon, accent border, ACTIVE badge, ★ default) + "Install local profile…".
  Marketplace: browse/search (mock). Selecting a card sets the active phenotype via `store/skills.ts`
  (`getPhenotype`/`setPhenotype`) if it maps; else mock.
- **Files:** create `settings/profiles/installed-tab.tsx`, `marketplace-tab.tsx` +
  `settings/profiles-section.tsx`; `store/profiles.ts`; `ipc.ts`/`mock.ts`.
- **State & types:** `store/profiles.ts`: `{ profiles: Profile[]; activeId: string }`,
  `Profile = { id; name; description; skillCount; locked; accent }` — prefer reusing `Phenotype`
  bindings where the shape matches. `resetProfiles()` for the footer.
- **IPC:** reuse phenotype IPC; mock `listProfiles` if needed.
- **Acceptance criteria:** cards render with ACTIVE badge on the active profile; setting active
  persists; marketplace search returns mock results; reset works.
- **Tests:** `store/profiles.test.ts`; `mock.profiles.test.ts`.
- **Out of scope:** profile install/marketplace backend.

---

### SET.8 — Memory section

- **Branch:** `feat/set-8-memory` · **Labels:** `frontend`, `design` · **Depends on:** `SET.1a`
- **Why:** A memory browser to pair with ambient/persistent memory (RFC 0002).
- **⚠️ Speculative-shape note (must be in the issue body):** there is **no `ff-memory` crate in the
  codebase yet** — only RFC 0002. The WHO / HOW / WHAT / JOURNAL / FILES layout is a **reference-IA
  hypothesis**, not a contract the backend has agreed to. Build it against mock IPC, but
  explicitly mark the types (`categories`, journal `Entry`, `FileRef`) as provisional so the eventual
  backend work isn't constrained by them. Expect the mock to be revised when the real memory model
  lands. Consider whether this section should ship **behind the Experimental "Smart skill
  surfacing"/memory flag** or as `<ComingSoon>` until the backend exists — flag for Abid's call at
  kickoff.
- **Build:** search bar; category cards **WHO** (Role & preferences) / **HOW** (Patterns &
  conventions) / **WHAT** (Current priorities); a **JOURNAL** list (empty state "No journal entries
  yet"); a **FILES** list with a `N files · NN KB` footer.
- **Files:** create `settings/memory-section.tsx`; `store/memory.ts`; `ipc.ts`/`mock.ts`.
- **State & types:** `store/memory.ts` (all marked provisional): `{ categories: {who; how; what};
  journal: Entry[]; files: FileRef[] }`, `FileRef = { name; sizeBytes }`. Client-side search.
- **IPC:** mock `getMemory()` / `searchMemory(query)`; wire real `ff-memory` IPC only once it exists.
- **Acceptance criteria:** category cards + journal/files render from mock; search filters; files
  footer shows count + summed size; reset clears search. Issue body carries the speculative note.
- **Tests:** `store/memory.test.ts`; `mock.memory.test.ts`.
- **Out of scope:** memory editing/persistence backend; committing to these shapes long-term.

---

### SET.9 — Scheduled section

- **Branch:** `feat/set-9-scheduled` · **Labels:** `frontend`, `design` · **Depends on:** `SET.1a`
- **Why:** No surface for cron-scheduled agent tasks. Net-new; aligns with future automation.
- **Build:** intro line + **New task** button; scheduled-task cards (status dot, name, Builtin badge,
  cadence "Daily at 5:00 PM", "Next … · Last …", open + pause/resume). New task can be a stub form.
- **Files:** create `settings/scheduled-section.tsx`; `store/scheduled.ts`; `ipc.ts`/`mock.ts`.
- **State & types:** `store/scheduled.ts`: `{ tasks: ScheduledTask[] }`,
  `ScheduledTask = { id; name; builtin; cron; cadenceLabel; nextRun; lastRun; paused }` +
  `resetScheduled()`.
- **IPC:** mock `listScheduledTasks()` / `toggleScheduledTask(id)` / `createScheduledTask(input)`.
- **Acceptance criteria:** list renders a mock "Memory Organizer" builtin; pause/resume toggles;
  New task adds a session-persistent mock entry; reset works.
- **Tests:** `store/scheduled.test.ts`; `mock.scheduled.test.ts`.
- **Out of scope:** real cron runner.

---

### SET.10 — Experimental section

- **Branch:** `feat/set-10-experimental` · **Labels:** `frontend` · **Depends on:** `SET.1a`
  (uses `SET.1b` `switch`)
- **Why:** A home for opt-in flags.
- **Build:** vertical list of labeled switches with descriptions: **Use your own API key**,
  **Spotlight**, **Prevent sleep**, **Remote execution**, **Background observers**, **Smart skill
  surfacing**. FE flags; note "restart required" where relevant.
- **Files:** create `settings/experimental-section.tsx`; add `experimental: Record<FlagId, boolean>`
  to `store/prefs.ts` (or `store/experimental.ts`).
- **State & types:** `FlagId` union for the six flags; persisted; default all `false`;
  `resetExperimental()`.
- **IPC:** none; each flag documents the future backend it will gate.
- **Acceptance criteria:** all six toggles render with descriptions, persist across reload, default
  off; reset works.
- **Tests:** store test for defaults + persistence.
- **Out of scope:** the behaviors the flags gate.

---

### SET.11 — About section

- **Branch:** `feat/set-11-about` · **Labels:** `frontend` · **Depends on:** `SET.1a`
- **Why:** Version, updates, backup/restore, help links — closes out the nav.
- **Build:** version line ("Version 0.x — <tagline>"); rows **Check for updates**, **What's New**,
  **Quick Setup**; **Data** group **Export backup** / **Restore from backup**; "View all keyboard
  shortcuts →" calling `setSection("keyboard")`; **Get Help** rows **Report a Bug** / **Join our
  Slack** opening URLs via `@tauri-apps/plugin-opener` (already a dependency).
- **Files:** create `settings/about-section.tsx`; read version from Tauri metadata
  (`@tauri-apps/api`); mock export/restore + update-check as no-op actions with toasts.
- **State & types:** none persisted; version fetched on mount.
- **IPC:** mock `exportBackup()` / `restoreBackup()` / `checkForUpdates()`.
- **Acceptance criteria:** version renders; "View all keyboard shortcuts" navigates to the Keyboard
  section (only needs the `"keyboard"` registry id from `SET.1a` — **no hard dep on `SET.6`**);
  external links open via opener; backup actions show a confirmation toast.
- **Tests:** unit test that the link calls `setSection("keyboard")`.
- **Out of scope:** real updater + backup file format.

---

### SET.12 — Control: Team & per-profile UI customization (deferred / optional)

- **Branch:** `feat/set-12-control-team-ui` · **Labels:** `frontend`, `design` · **Depends on:**
  `SET.4`
- **Why:** Split out of the original SET.4 to keep that issue shippable. Lower priority — sequence
  after the core sections, or scope-cut entirely for the first epic pass.
- **Build:** two more sub-tabs added to the Control section: **Team** (teammate profiles list —
  avatar, name, slug, description — + "Add teammate", mock) and **UI** (per-profile accent color
  swatch, custom logo / favicon file pickers, contextual-greeting switch — all rendered with
  FlowForge's own styling; file pickers may stub to a returned path string).
- **Files:** create `settings/control/team-tab.tsx`, `ui-tab.tsx`; extend `settings/control-section.tsx`
  and `store/control-config.ts` with `teammates: Teammate[]` and `ui: { accentColor; logoPath;
  faviconPath; contextualGreeting }`; `Teammate = { id; name; slug; description }`.
- **IPC:** extend the mock control config from `SET.4`.
- **Acceptance criteria:** both tabs render and persist through mock IPC; reset extends `resetControl()`.
- **Tests:** extend `store/control-config.test.ts` / `mock.control.test.ts`.
- **Out of scope:** real teammate spawning; real file dialogs / favicon application.

---

## Verification (every issue)

1. `pnpm --filter desktop test` — new `mock.*.test.ts` + `store/*.test.ts` pass; existing suites stay
   green.
2. `pnpm --filter desktop dev` (mock IPC is the dev default). Open Settings with `Ctrl/Cmd+,`; confirm
   the section renders inside the centered modal, the nav highlights, sub-tabs switch, controls persist
   across reopen (localStorage) or round-trip through mock IPC, and **the footer reset works for the
   section**.
3. Use the preview MCP tools to snapshot the section and confirm zero console errors before pushing.
4. `pnpm --filter desktop lint` + typecheck clean.
5. Visual check against the **hard design constraint**: no FlowForge colors/tokens changed — only
   structure added.

## Out of scope for the whole epic (backend follow-ups for Tony)

Real Rust IPC + behavior for: model/provider beyond #49's contract, permission enforcement (incl.
extending `ApprovalSafety` to match the Control matrix), profiles, memory persistence (and confirming
the SET.8 shapes), scheduled cron runner, MCP supervisor (M4.2 #89), experimental-flag behaviors, and
the real updater/backup format. Each FE issue freezes its IPC contract in `ipc.ts` + bindings so
backend can fill in without UI churn.
