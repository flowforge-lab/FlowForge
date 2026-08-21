// Typed IPC contract — the single seam between the React frontend and the Rust backend.
//
// Every command and event the backend exposes is mirrored here with the generated
// `bindings` types. Set `VITE_FF_MOCK=1` to run the frontend against an in-browser
// mock that fulfils this exact contract, so UI work never blocks on the Rust side.

import type {
  UpdateStatus,
  UpdateChannel,
  BackupResult,
  SidecarTurnResult,
} from "@/lib/about";
import type { ControlConfig } from "@/lib/control";
import type { MarketplaceSkill } from "@/lib/marketplace";
import type { MarketplaceProfile } from "@/lib/profile-marketplace";
import type {
  ScheduledTask,
  CreateScheduledTaskInput,
  RunRecord,
} from "@/bindings";
import type { PhenotypeMcpUnavailableEvent } from "@/bindings";
import type { PhenotypePreheatDroppedEvent } from "@/bindings";
import type { UpdateProgressEvent } from "@/bindings";
import type {
  Attachment,
  Message,
  ProviderConfig,
  ProviderConnection,
  ProviderRegistry,
  ProviderKind,
  ModelSelection,
  ResolvedModel,
  SearchConfig,
  SearchBackend,
  Session,
  SessionWorkspace,
  TokenEvent,
  ReasoningEvent,
  TurnDoneEvent,
  TurnErrorEvent,
  TurnStatsEvent,
  IntentionSignal,
  SessionTitleUpdatedEvent,
  ToolApprovalRequestEvent,
  ToolAskRequestEvent,
  ToolCallEvent,
  ToolOutputChunkEvent,
  ProcessOutputEvent,
  ProcessExitedEvent,
  TerminalExitedEvent,
  ObserverInfo,
  ObserverChangedEvent,
  ToolResultEvent,
  SkillInfo,
  SkillAggregate,
  SkillsChangedEvent,
  SkillEvolveApprovalRequestEvent,
  Phenotype,
  McpServerStatus,
  McpServerConfig,
  McpStatusChangedEvent,
  MemoryChunkStat,
  MemoryFileInfo,
  MemoryOverview,
  MemoryFlushedEvent,
  Stratum,
  Mode,
  Safety,
  PermissionCell,
  PermissionMatrixView,
} from "../bindings";
import type { Goal } from "../bindings/Goal";
import type { DirEntry } from "../bindings/DirEntry";
import type { FileContent } from "../bindings/FileContent";
import type { Format } from "../bindings/Format";
import type { SecretKind } from "../bindings/SecretKind";
import type { BedrockAuth } from "../bindings/BedrockAuth";
import type { SearchHit } from "../bindings/SearchHit";
import type { NotebookKernelState } from "../bindings/NotebookKernelState";

export type Unlisten = () => void;

export interface FfIpc {
  // Commands (frontend -> backend)
  // Paint-first boot (#599): the backend defers `AppState::new()` off the
  // synchronous pre-window path onto a background hydrate task, so the window
  // paints a loading state before the state is managed. The FE gates its
  // backend-dependent work on `app:ready` + this flag (subscribe-then-check)
  // — invoke handlers read `State<'_, Arc<AppState>>`, which only resolves once
  // the task publishes the state, so no command below may run before it's true.
  /** True once the background hydrate task has finished `AppState::new()` and
   *  published the managed state. Safe to call pre-ready (reads a static flag). */
  isAppReady(): Promise<boolean>;
  createSession(goal?: string): Promise<Session>;
  // CONTRACT CHANGE (#149, fork into split pane): NEW command needing a real Rust
  // implementation (mocked here for now) — please review @backend-owner. Clones a
  // session's transcript/context into a fresh session server-side and returns it.
  /** Fork a session: create a new session seeded with a copy of `sessionId`'s
   *  messages/context. Rejects an unknown id. */
  forkSession(sessionId: string): Promise<Session>;
  listSessions(): Promise<Session[]>;
  getMessages(sessionId: string, limit?: number): Promise<Message[]>;
  /** Serialize a session for export (#278): lossless `json` ({session, messages})
   *  or human-readable `markdown`. Rejects an unknown id. The FE writes the
   *  returned string to a user-chosen path; no file IO crosses this seam. */
  exportSession(sessionId: string, format: Format): Promise<string>;
  /** Sets a session's persisted display title (server-truth). */
  renameSession(sessionId: string, title: string): Promise<void>;
  /** Permanently remove a session and its transcript. Destructive; pairs with the
   *  sidebar Delete action. Distinct from the FE-only reversible dismiss (#170). */
  deleteSession(sessionId: string): Promise<void>;
  /** The working directory a session's tools run in (slice 3b, #200) with its
   *  git branch when the cwd is a repo (#211). Returns the session's chosen
   *  workspace, or the global default when unset. */
  getSessionWorkspace(sessionId: string): Promise<SessionWorkspace>;
  /** Set a session's working directory. Backend validates the path is an existing
   *  directory and returns the canonical path to display; rejects otherwise. */
  setSessionWorkspace(sessionId: string, path: string): Promise<string>;
  /** Local branches in the session's cwd repo (refname-sorted), to populate a
   *  branch-switch picker (#628). Empty when the cwd is not a git work tree. */
  listBranches(sessionId: string): Promise<string[]>;
  /** Check out `branch` in the session's cwd and return the updated workspace
   *  (#628). Also emits `workspace:branch-changed`, so the chip updates and
   *  flashes (#627) through the same reactive path an external checkout uses --
   *  callers do not need to patch the workspace store themselves. Rejects on an
   *  unknown branch or a checkout git refuses (e.g. a dirty working tree). */
  checkoutBranch(sessionId: string, branch: string): Promise<SessionWorkspace>;
  /** List one directory level under the session workspace, for the Files panel
   *  (#872). `path` is relative to the workspace root (`""` is the root). Jailed
   *  to the root and `.gitignore`-aware (no `node_modules`/`target`); entries are
   *  sorted directories-first, then case-insensitively by name. Rejects a path
   *  that escapes the root or is not a directory. */
  listDirectory(sessionId: string, path: string): Promise<DirEntry[]>;
  /** Read one file's body under the session workspace, for the Files panel viewer
   *  (#872). Reads at most `maxBytes` (default 512KB); `truncated` marks a larger
   *  file. Non-UTF-8 content returns `isBinary: true` with `text: null`. Rejects a
   *  path that escapes the root or is not a file. */
  readFile(
    sessionId: string,
    path: string,
    maxBytes?: number,
  ): Promise<FileContent>;
  /** Open an interactive shell for the terminal drawer (#1284), rooted at this
   *  session's working directory. Resolves to the terminal id the other three
   *  `*Terminal` calls address.
   *
   *  `onData` receives raw PTY bytes as they arrive — feed them straight to
   *  xterm, which owns UTF-8 decoding (a multi-byte character can straddle two
   *  chunks, so this layer must never decode them itself). Under Tauri this is a
   *  `Channel`, not an event: terminal output is the high-throughput case Tauri's
   *  docs steer away from the event system. */
  openTerminal(
    sessionId: string,
    cols: number,
    rows: number,
    onData: (bytes: Uint8Array) => void,
  ): Promise<string>;
  /** Send keystrokes to a terminal's shell — whatever xterm's `onData` produced. */
  writeTerminal(terminalId: string, data: string): Promise<void>;
  /** Tell a shell its window resized, so it re-wraps and full-screen programs
   *  redraw. Driven by the drawer's `ResizeObserver` → `fit()`. */
  resizeTerminal(terminalId: string, cols: number, rows: number): Promise<void>;
  /** Kill a terminal's shell. Idempotent backend-side: closing one that already
   *  exited on its own resolves rather than rejecting. */
  closeTerminal(terminalId: string): Promise<void>;
  /** Persists the user message and starts the assistant turn. Returns the user message id. */
  sendMessage(
    sessionId: string,
    content: string,
    attachments?: Attachment[],
  ): Promise<string>;
  /** Edit a prior user message in place, truncate the transcript after it, and
   *  re-run the turn from the edited prompt (#463; backend #464). Cancels any
   *  in-flight turn + pending approvals for the session first, then re-runs over
   *  the existing `turn:*` / `tool:*` events. Rejects an unknown/wrong-session id
   *  or a non-user message. Returns the edited message id. */
  editMessage(
    sessionId: string,
    messageId: string,
    content: string,
    attachments?: Attachment[],
  ): Promise<string>;
  cancelTurn(sessionId: string): Promise<void>;
  /** Frontend's reply to a [`ToolApprovalRequestEvent`]. Keyed by `(sessionId,
   *  callId)` so it never resolves a colliding call in another session. */
  respondApproval(
    sessionId: string,
    callId: string,
    approved: boolean,
  ): Promise<void>;
  /** Frontend's answer to a [`ToolAskRequestEvent`] (#44, `ask_user`). Resumes the
   *  paused turn with the user's reply. Keyed by `(sessionId, callId)`. */
  respondAsk(sessionId: string, callId: string, answer: string): Promise<void>;

  // Four-option tool approval (#229). "Allow once"/"Deny" stay on `respondApproval`;
  // these add the two persistent tiers. The backend owns both approval sets and
  // short-circuits the gate before emitting an event (no UI flicker), so these
  // only ever *write* the set — pair each with `respondApproval(callId, true)` to
  // also release the in-flight call. `bindings/` is untouched: plain string
  // commands, no shared DTO.
  /** Approve `tool` for the rest of `sessionId` (in-memory; cleared on session
   *  delete). Future calls of the same tool skip the prompt. */
  setSessionApprove(sessionId: string, tool: string): Promise<void>;
  /** Approve `tool` for all future sessions (persisted to `tool_permissions.json`).
   *  Revocable via `removeAlwaysApprove`. */
  setAlwaysApprove(tool: string): Promise<void>;
  /** Drop `tool` from the always-approved set (Settings → revoke). Idempotent. */
  removeAlwaysApprove(tool: string): Promise<void>;
  /** The always-approved tool names, sorted. Backs the Settings revocation list. */
  listAlwaysApproved(): Promise<string[]>;

  // Provider settings (Issue #8). Phase 1: local candle-vllm + Ollama, no secrets.
  /** Current persisted LLM provider settings. */
  getProviderConfig(): Promise<ProviderConfig>;
  /** Persist provider settings; resolves with the stored config (e.g. `hasKey`). */
  setProviderConfig(
    kind: ProviderKind,
    baseUrl: string | undefined,
    model: string,
    thinking: boolean,
  ): Promise<ProviderConfig>;
  // Provider connection registry (Issue #138, RFC 0005 Phase A). Lets the user
  // keep several backends configured and switch the active one non-destructively.
  /** All configured connections plus the active pointer. */
  getProviderRegistry(): Promise<ProviderRegistry>;
  /** Select the active connection by id; rejects if the id is unknown. */
  setActiveConnection(id: string): Promise<void>;
  /** Add or update a connection (keyed by `id`); resolves with the stored value
   *  (e.g. a server-derived `id` for a freshly added connection). */
  upsertConnection(conn: ProviderConnection): Promise<ProviderConnection>;
  /** Remove a connection by id; rejects when removing the last one. */
  removeConnection(id: string): Promise<void>;
  /** Best-effort model ids for a connection (defaults to the active one); `[]`
   *  when the endpoint is unreachable. */
  listModels(id?: string): Promise<string[]>;
  // Provider secrets (Issue #202 PR-3). Write-only: secret material (API key,
  // AWS secret access key, session token) goes to the OS keychain and is NEVER
  // read back over IPC — the only observable signal is `hasKey` on the refreshed
  // `ProviderConnection`. `bindings/` is untouched; `SecretKind` is the generated
  // discriminator already shipped with the registry types (#126).
  /** Store one secret for a connection in the OS keychain; flips its `hasKey`.
   *  The value is never returned or logged. */
  setProviderSecret(
    connectionId: string,
    kind: SecretKind,
    value: string,
  ): Promise<void>;
  /** Remove one stored secret for a connection; recomputes `hasKey` from what
   *  remains. Idempotent. */
  clearProviderSecret(connectionId: string, kind: SecretKind): Promise<void>;
  /** Which `SecretKind`s are stored for a connection (#320), in `SecretKind::ALL`
   *  order, so each Bedrock `SecretField` can render Stored/Clear off its own kind
   *  instead of the aggregate `hasKey`. Presence only — no secret value is returned;
   *  rejects on an unknown connection id. */
  providerSecretPresence(connectionId: string): Promise<SecretKind[]>;
  /** The Bedrock auth a connection resolves to right now (#320): the explicit pin,
   *  or the `Auto` precedence winner (API key > profile > IAM keys). `null` for
   *  non-Bedrock or unknown connections — lets the UI badge the active credential. */
  resolvedBedrockAuth(connectionId: string): Promise<BedrockAuth | null>;
  /** Probe a connection end-to-end (defaults to the active one) for the settings
   *  "Test Connection" button. Resolves on a successful round-trip; rejects with a
   *  message to show on failure. Unlike `listModels`, the error is surfaced. */
  testConnection(id?: string): Promise<void>;

  // Web search (Issue #43). SearXNG is wired keyless; hosted backends are gated
  // until key storage (#8). Secrets are never part of this contract.
  /** Current persisted web-search settings. */
  getSearchConfig(): Promise<SearchConfig>;
  /** Persist web-search settings; resolves with the stored config (e.g. `hasKey`). */
  setSearchConfig(
    backend: SearchBackend,
    baseUrl: string | undefined,
    email?: string | undefined,
  ): Promise<SearchConfig>;
  /** Best-effort nudge to wake the model server before the first turn. Never throws meaningfully. */
  warmup(): Promise<void>;

  // Full-text message search (FTS5, #679/#707). Both back real Rust commands
  // registered in `invoke_handler!`; the `SearchHit` binding is ts-rs generated.
  // Empty/blank queries resolve to `[]`.
  /** In-thread find (#679): matches within one session, seq-ordered so next/prev
   *  steps through them in message order. Indexes tool-call args + tool-result
   *  bodies too (v11 migration), not just visible message text. */
  searchInSession(sessionId: string, query: string): Promise<SearchHit[]>;
  /** Cross-session search (#710): BM25-ranked hits across every session. */
  searchMessages(query: string, limit?: number): Promise<SearchHit[]>;

  // Memory (RFC 0006, M5.1e — the Settings memory pane's surface, Issue #131).
  // Reads have real Rust impls + ts-rs bindings. The enable/disable toggle and
  // embeddings are deliberately out of scope.
  //
  // CONTRACT CHANGE (#868, backend seam #969/#1028) — this block is no longer
  // read-only: `writeCuratedMemory` below is the one write, and it is deliberately
  // narrow (whole-stratum replace of the three curated `MEMORY.md` sections).
  // Everything else — daily journal files, non-curated files — stays read-only.
  // Please review @tonytan4ever.
  //
  // CONTRACT NOTE: there is intentionally NO `searchMemory` here. Host-side memory
  // search is deferred to the HybridIndex work (#166) so we don't freeze a
  // result/score DTO right before that PR changes ranking. The Settings pane reads
  // whole files; recall ranking stays an agent-tool concern until #166 lands.
  /** Curated + daily memory files, curated first then daily newest-first. */
  listMemoryFiles(): Promise<MemoryFileInfo[]>;
  /** Read one memory file's body by its root-relative path (from `listMemoryFiles`).
   *  Rejects a path that escapes the memory root. */
  readMemoryFile(relPath: string): Promise<string>;
  /** Store summary (file/byte counts, root, enabled flag) for the pane header. */
  memoryOverview(): Promise<MemoryOverview>;
  /** Replace one curated stratum's body in `MEMORY.md` — whole-section replace,
   *  not the append the agent's `memory_write` tool does. Sibling sections are
   *  preserved, a missing heading is created, and empty `text` clears the body
   *  while keeping the heading. Routed through the backend's atomic single-writer
   *  path, so the FE never writes the file itself. */
  writeCuratedMemory(stratum: Stratum, text: string): Promise<void>;
  // Salience surface (RFC 0007 M6.2, #293). Per-chunk weight/dormant + reset/pin.
  // `weight`/`dormant` are computed authoritatively by the backend; the FE never
  // re-derives the dormancy threshold. Decay/dormancy/pin never edit Markdown —
  // they only change ambient injection.
  /** Per-chunk salience stats: effective weight, dormant flag, access count, pin. */
  listMemoryChunks(): Promise<MemoryChunkStat[]>;
  /** Reset (wake) a chunk: weight back to 1.0, stamp last-accessed now. */
  resetMemoryChunk(chunkKey: string): Promise<void>;
  /** Sleep a chunk: weight to 0, so it goes dormant now instead of decaying
   *  there over days (#1239). Inverse of reset; the chunk stays searchable.
   *  A pinned chunk still reads 1.0, so the UI disables this for pinned rows. */
  sleepMemoryChunk(chunkKey: string): Promise<void>;
  /** Pin/unpin a chunk: pinned holds weight at 1.0 and is never dormant. */
  setMemoryChunkPinned(chunkKey: string, pinned: boolean): Promise<void>;

  // Control settings (Issue #127). `ControlConfig` is a FE-owned shape (lib/control.ts):
  // there is no backend/ts-rs type yet, and the permission matrix does NOT map to
  // `ApprovalSafety` ("write"|"dangerous"). For now this round-trips presentation
  // state + mock storage only; it does not drive runtime approval.
  // The global default mode is NOT part of this config — it lives in the backend
  // `mode.json` via `getDefaultMode`/`setDefaultMode` (#798). The real permission
  // matrix that drives approval is `getPermissionMatrix` below (#702).
  /** Current persisted control settings (permissions presentation + prompts). */
  getControlConfig(): Promise<ControlConfig>;
  /** Persist control settings; resolves with the stored config. */
  setControlConfig(config: ControlConfig): Promise<ControlConfig>;

  // Permission matrix (#702, RFC 0019 §3). Unlike `ControlConfig` above, this is
  // the REAL backend matrix that drives runtime approval: `get` returns every
  // Mode × Safety cell; `set` edits one cell, persists it to `permissions.json`,
  // and returns the updated view. Effective on the next tool invocation.
  /** The current permission matrix as a flat Mode × Safety cell list. */
  getPermissionMatrix(): Promise<PermissionMatrixView>;
  /** Edit one matrix cell; resolves with the updated view. */
  setPermissionCell(
    mode: Mode,
    safety: Safety,
    cell: PermissionCell,
  ): Promise<PermissionMatrixView>;
  /** Set a per-tool override (#700); resolves with the updated view. */
  setToolOverride(
    tool: string,
    cell: PermissionCell,
  ): Promise<PermissionMatrixView>;
  /** Remove a per-tool override (#700); resolves with the updated view. */
  removeToolOverride(tool: string): Promise<PermissionMatrixView>;

  // Skills (Issue #27). Discovery + the global active set; backs the command palette.
  /** All installed skills, name-sorted, each flagged active; `score` is always 0. */
  listSkills(): Promise<SkillInfo[]>;
  /** Ranked skill search (shares the agent tool's ranking). Empty query lists all. */
  searchSkills(query: string): Promise<SkillInfo[]>;
  // CONTRACT NOTE (SET.5): FE-owned mock command — no backend/ts-rs binding for a
  // remote catalog exists yet. `MarketplaceSkill` lives in `lib/marketplace.ts`
  // (mirroring SET.4's `ControlConfig`); `bindings/` is untouched. Replace with a
  // generated binding + real command when the marketplace backend lands.
  /** Search the (mock) skill marketplace. Empty query lists the full catalog. */
  searchSkillMarketplace(query: string): Promise<MarketplaceSkill[]>;
  /** Add a skill to the global active set; its body is injected next turn. Rejects an unknown name. */
  activateSkill(name: string): Promise<void>;
  /** Remove a skill from the active set. Idempotent. */
  deactivateSkill(name: string): Promise<void>;
  /** Per-skill telemetry aggregate (RFC 0001 §8), or null if none recorded yet. */
  getSkillTelemetry(skill: string): Promise<SkillAggregate | null>;

  // Skill evolution (Issue #29, M3.5). Manual optimize proposes a streamlined
  // rewrite gated by user approval; versions are archived for rollback.
  /** Propose an LLM rewrite of a skill, gated by a [`SkillEvolveApprovalRequestEvent`].
   *  Resolves with the new version string on approval; rejects if declined or on error. */
  optimizeSkill(sessionId: string, skill: string): Promise<string>;
  /** Restore a previously archived skill version (archives the current one first). */
  rollbackSkill(skill: string, version: string): Promise<void>;
  /** Archived version strings for a skill, newest-first; `[]` when none. */
  listSkillVersions(skill: string): Promise<string[]>;

  // Phenotypes (Issue #28). Named, switchable working sets (RFC 0001 §7).
  /** All selectable phenotypes (built-in `default` + `~/.flowforge/phenos/`), name-sorted. */
  listPhenotypes(): Promise<Phenotype[]>;
  /** The active phenotype. */
  getPhenotype(): Promise<Phenotype>;
  /** Switch the active phenotype: replaces the active-skill set and persists the
   *  choice across restarts. Rejects an unknown name. Resolves with the phenotype now active. */
  switchPhenotype(name: string): Promise<Phenotype>;
  /** Bind a single session to a phenotype, or clear the binding (`name: null`) so it
   *  inherits the global active one (#246). Only the named session changes — other
   *  panes are untouched. Rejects an unknown phenotype name. Used by the pane Pheno
   *  selector (#245) to make a pane's phenotype truly per-session. */
  setSessionPhenotype(sessionId: string, name: string | null): Promise<void>;
  /** Persist an edited phenotype (RFC 0005 Phase D / #525). Accepts the whole
   *  `Phenotype` — a lossless read-modify-write upsert keyed by `name`. Validates
   *  `provider` against the live registry (rejects an unknown connection) and rejects
   *  the built-in `default` (immutable). When the edited phenotype is the active one,
   *  its skills are re-applied and `skills:changed` is emitted. Resolves with the
   *  saved phenotype. */
  updatePhenotype(phenotype: Phenotype): Promise<Phenotype>;

  // Per-session model selection (RFC 0005 §11.2, Phase D; #499). A per-pane
  // override over the phenotype/global tiers, mirroring the set_session_phenotype
  // + autonomy-mode (#265) precedent (global default + per-session override +
  // inherit-when-None). `ModelSelection` already exists from Phase C; capabilities
  // are derived from (kind, model), never stored on the selection (§11.3).
  /** Bind a single session (pane) to a `ModelSelection`, or clear it (`null`) so it
   *  inherits the phenotype/global tiers. Rejects an unknown connection id. */
  setSessionModelSelection(
    sessionId: string,
    selection: ModelSelection | null,
  ): Promise<void>;
  /** The raw per-session override, or `null` when the session inherits — drives the
   *  chip's "overridden / clear" affordance. */
  getSessionModelSelection(sessionId: string): Promise<ModelSelection | null>;
  /** The authoritative resolved pair for a session: session ?? phenotype ?? global
   *  (§11.2), plus the attachment capabilities derived from the resolved
   *  `(kind, model)` (§11.3) — never stored on a connection. Wraps the backend
   *  resolver so the FE never duplicates it; drives the per-pane model chip's label
   *  and the composer attach gate. */
  resolveModelSelection(sessionId: string): Promise<ResolvedModel>;

  // Per-session agent mode (#266, RFC 0011; #789). The composer pill is
  // authoritative and mirrors every change here so `spawn_assistant_turn` — which
  // reads the persisted `sessions.mode` fresh each turn — actually honours Plan/Act.
  /** Persist a session's explicit mode override, or clear it (`null`) so the
   *  backend inherits `default_mode`. */
  setSessionMode(sessionId: string, mode: Mode | null): Promise<void>;

  // Global default mode (#266, #798). Bridges the already-registered backend
  // commands `get_default_mode` / `set_default_mode`, whose source of truth is
  // `mode.json` (`AppState.default_mode`; unbound sessions resolve to it). The FE
  // hydrates from `getDefaultMode` at boot and writes every default-mode edit
  // through `setDefaultMode`, so the choice survives a relaunch instead of living
  // in a transient store. `Mode` is a generated binding; `bindings/` is untouched.
  /** The persisted global default mode new sessions inherit. */
  getDefaultMode(): Promise<Mode>;
  /** Persist the global default mode (to `mode.json`). */
  setDefaultMode(mode: Mode): Promise<void>;

  // CONTRACT NOTE (SET.7): FE-owned mock command — no backend/ts-rs binding for a
  // remote profile catalog exists yet. `MarketplaceProfile` lives in
  // `lib/profile-marketplace.ts` (mirroring SET.5's `MarketplaceSkill`);
  // `bindings/` is untouched. Replace with a generated binding + real command
  // when the profile marketplace backend lands.
  /** Search the (mock) profile marketplace. Empty query lists the full catalog. */
  searchProfileMarketplace(query: string): Promise<MarketplaceProfile[]>;

  // Scheduled tasks (RFC 0017, #540). Real backend commands over the `ff-scheduled`
  // store; `ScheduledTask` / `CreateScheduledTaskInput` are generated bindings. The
  // cadence label and next-run are derived server-side (one source of truth) — the
  // FE never computes or sends them. Firing is a separate concern (#542).
  /** All scheduled tasks (built-in + user-created), newest first. */
  listScheduledTasks(): Promise<ScheduledTask[]>;
  /** Pause/resume a task; resolves with the updated task. Rejects an unknown id. */
  toggleScheduledTask(id: string): Promise<ScheduledTask>;
  /** Create a user task; resolves with the stored task (server-assigned id +
   *  derived cadence label / next run). Rejects an invalid cron expression. */
  createScheduledTask(input: CreateScheduledTaskInput): Promise<ScheduledTask>;
  /** Delete a task. Rejects for built-in tasks (they ship with the app). */
  deleteScheduledTask(id: string): Promise<void>;
  /** The human cadence label a cron expression would produce (e.g. "Daily at
   *  5:00 PM"), for the New-task form's Custom-cron preview. Rejects bad cron. */
  previewCadence(cron: string): Promise<string>;
  // CONTRACT CHANGE (#543, depends on backend Issue C — runner + events): NEW
  // command + the `scheduled:fired` / `scheduled:changed` events below need a real
  // Rust emitter — please review @backend-owner. Both events reuse existing
  // generated bindings (`RunRecord`, `ScheduledTask`), so `bindings/` is untouched;
  // only the Rust command (`run_scheduled_task_now`) + the two emits are owed.
  // Mocked under `VITE_FF_MOCK=1` for now.
  /** Fire a task immediately (out of band of its cron). Resolves with the run it
   *  created — `RunRecord.sessionId` is the session the fire spawned, backing the
   *  ↗ open-session jump. Also drives a `scheduled:fired` + `scheduled:changed`.
   *  Rejects when the global pause-all kill-switch is engaged. */
  runScheduledTaskNow(id: string): Promise<RunRecord>;
  /** A task's fire history, newest first (capped at 50). Ordering is guaranteed by
   *  the backend query (`ORDER BY fired_ms DESC, id DESC`), so consumers render as-is
   *  — no client re-sort. Backs the run-history list and the ↗ open-session
   *  affordance (RFC 0017 §6.2, #544). */
  listScheduledRuns(id: string): Promise<RunRecord[]>;
  /** Engage/release the global pause-all kill-switch (RFC 0017 §8.3, #544). When
   *  engaged the sweep fires nothing, regardless of per-task pause — including
   *  tasks created while engaged. Resolves with the new state; emits
   *  `scheduled:changed`. */
  setScheduledPausedAll(paused: boolean): Promise<boolean>;

  // MCP servers (M4.4, RFC 0003). Enable/disable/add/remove write `mcp.json`; the
  // config watcher reconciles the supervisor, which then emits `mcp:status-changed`.
  /** Current status snapshot of every configured MCP server. */
  listMcpServers(): Promise<McpServerStatus[]>;
  /** Drive an immediate restart of one server (bypasses backoff). No-op if unknown. */
  restartMcpServer(id: string): Promise<void>;
  /** Enable or disable one server: flips `disabled` in `mcp.json` and reconciles. */
  setMcpServerEnabled(id: string, enabled: boolean): Promise<void>;
  /** Add (or replace) a server definition in `mcp.json` and reconcile. */
  addMcpServer(def: McpServerConfig): Promise<void>;
  /** Remove a server definition from `mcp.json` and reconcile. No-op if absent. */
  removeMcpServer(id: string): Promise<void>;

  // CONTRACT NOTE (SET.11): FE-owned result types — no backend/ts-rs bindings
  // exist yet (mock-only). `UpdateStatus` / `BackupResult` live in lib/about.ts
  // (mirroring the SET.5/7 marketplace contracts). The FE owns the user-facing
  // copy; these report only the structured outcome. Replace with generated
  // bindings + a real updater/backup backend when they land (#159).
  /** Check the given update feed. `channel` is explicit (#1033) so the endpoint is
   *  never inferred from a global flag; a `local` check that can't reach the
   *  dev-release server rejects rather than silently falling back. */
  checkForUpdates(channel: UpdateChannel): Promise<UpdateStatus>;
  /** Download and install the available update from `channel`; the backend relaunches
   *  the app on success (so this never resolves in the real app — see #363 / #362).
   *  `channel` must match the check that surfaced the update. `expectedVersion` is the
   *  version this UI showed the user — the backend re-checks the feed, so it refuses the
   *  install when the feed moved in between rather than installing a build the user never
   *  saw (#1034). `allowDowngrade` is the user's explicit confirmation that an OLDER build
   *  may be installed; without it the backend refuses one, so no code path can silently
   *  roll the app backwards. */
  installUpdate(
    channel: UpdateChannel,
    expectedVersion: string,
    allowDowngrade?: boolean,
  ): Promise<void>;
  /** Start the dev-update file-system watcher (#705). Idempotent; the watcher
   *  observes `~/.config/flowforge/dev-update/latest.json` and emits
   *  `update:local-feed-changed` instantly on write. */
  startDevUpdateWatcher(): Promise<void>;
  /** Fired by the dev-update watcher when a new build lands. */
  onLocalFeedChanged(cb: () => void): Promise<Unlisten>;
  /** Export a local backup. */
  exportBackup(): Promise<BackupResult>;
  /** Restore from a backup. */
  restoreBackup(): Promise<BackupResult>;
  /** CLI.7 sidecar parity smoke-test (RFC 0004 §5): spawn the bundled
   *  `flowforge` CLI as a Tauri sidecar, run one agent turn with `--json`,
   *  and re-emit every parsed `AgentEvent` through the same `emit_agent_event`
   *  helper the in-process turn path uses. Resolves with the synthetic
   *  session id and the total event count so a manual QA button can toast the
   *  result. Dev-only — never shipped onto a user-visible surface: the only
   *  caller is the About-section "Run sidecar smoke-test" button, which is
   *  gated behind the `devTools` experimental flag (default off). */
  runSidecarTurn(prompt: string): Promise<SidecarTurnResult>;

  // Goal mode (RFC 0020, #683). A persistent autonomous objective bound to one
  // session: the loop self-continues each turn toward `objective` until it
  // completes, exhausts its budget, or the user intervenes. These mirror the
  // SHIPPED backend commands (#716/#753) exactly, so signatures are ground truth:
  //   goal_set(sessionId, objective, maxIterations?, maxTokens?, maxWallMs?, allowProposePr?) -> Goal
  //   goal_status/goal_pause/goal_resume(sessionId) -> Option<Goal>  (== Goal | null)
  //   goal_clear(sessionId) -> ()  and emits `goal:cleared` (bare sessionId)
  // Reuses the generated `Goal` binding (Track B); `bindings/` is untouched. Mocked
  // under `VITE_FF_MOCK=1` so the panel runs standalone. `goal_complete` exists on
  // the backend too but is out of scope here (a "mark complete" affordance is a
  // follow-up). "Steer" is NOT a command: per §6 a user message sent while a goal is
  // `active` becomes a steer (folded into `pendingSteer`), so it rides `sendMessage`.
  /** Begin (or replace) the session's goal and start the loop. Budget dimensions
   *  are flat optional args (matching `goal_set`); each `undefined` uses the
   *  backend default (RFC 0020: 40 iterations, tokens/wall unbounded).
   *  `allowProposePr` authorises the goal to open a draft PR via `propose_pr` on
   *  completion (#1256); defaults to false (report-only). Resolves the new goal. */
  goalSet(
    sessionId: string,
    objective: string,
    maxIterations?: number,
    maxTokens?: number,
    maxWallMs?: number,
    allowProposePr?: boolean,
  ): Promise<Goal>;
  /** Current goal snapshot for the panel to hydrate on mount, or `null` when the
   *  session has no goal. Closes the race where a goal exists before the
   *  `goal:updated` listener attached. */
  goalStatus(sessionId: string): Promise<Goal | null>;
  /** Pause an active goal at its next iteration boundary. Resolves the paused goal,
   *  or `null` when the session has no goal (backend `Option<Goal>`). */
  goalPause(sessionId: string): Promise<Goal | null>;
  /** Resume a paused goal. Resolves the reactivated goal, or `null` when the session
   *  has no goal (backend `Option<Goal>`). */
  goalResume(sessionId: string): Promise<Goal | null>;
  /** Abort the goal and delete its checkpoint. Idempotent. Emits `goal:cleared`
   *  (not `goal:updated`); the panel unmounts when the store drops that session. */
  goalClear(sessionId: string): Promise<void>;

  // #871: the `notebook_status` / `notebook_stop` Tauri commands are backed by
  // `KernelSupervisor` (`crates/ff-tools/src/notebook/mod.rs`) and registered in
  // `invoke_handler!`; `NotebookKernelState` is the generated ts-rs binding.
  // `notebookStatus` still fails closed after the first rejection (see
  // `store/notebook.ts`) so an older build without the commands degrades
  // gracefully instead of retrying every mount. Mocked under `VITE_FF_MOCK=1`.
  // No new events yet: a later `notebook:updated` push event can replace the
  // panel's polling as the source of live signal — tracked in #871.
  /** Per-session snapshot of the `notebook_runner` kernel (#871 FE-1). Returns
   *  the current state without spinning up an agent turn — mirrors
   *  `goalStatus`. When the session has no kernel, `hasKernel=false` and every
   *  other field collapses to its null/zero default. */
  notebookStatus(sessionId: string): Promise<NotebookKernelState>;
  /** Stop the session's kernel (#871 FE-1). Mirrors `goalClear`'s void return;
   *  the caller calls `notebookStatus` again to observe the post-stop
   *  snapshot. Idempotent — no error when the session has no kernel. `kernelId`
   *  stops a single kernel once a session holds more than one (Phase 3);
   *  omitted, the backend stops every kernel in the session. */
  notebookStop(sessionId: string, kernelId?: string): Promise<void>;
  /** Restart the session's kernel (#871 FE-2): kill the current subprocess and
   *  spawn a fresh one, discarding in-kernel state (globals, execution count).
   *  Backed by `KernelSupervisor::restart`. Resolves the post-restart snapshot
   *  so the panel re-renders without a follow-up `notebookStatus`. `kernelId`
   *  targets a specific kernel once a session holds more than one (Phase 3);
   *  omitted, the backend restarts the session's sole/representative kernel. */
  notebookRestart(
    sessionId: string,
    kernelId?: string,
  ): Promise<NotebookKernelState>;

  /** A session's active background observers (#1038, epic #954 M2) — backs the
   *  `👁 Observers` panel. Oldest id first; only this session's observers. */
  listObservers(sessionId: string): Promise<ObserverInfo[]>;
  /** Stop one observer by id (the panel's `[×]`). The backend emits
   *  `observer:changed` so the panel re-lists. */
  stopObserver(id: number, sessionId: string): Promise<void>;

  // Events (backend -> frontend)
  onToken(cb: (e: TokenEvent) => void): Promise<Unlisten>;
  onReasoning(cb: (e: ReasoningEvent) => void): Promise<Unlisten>;
  onTurnDone(cb: (e: TurnDoneEvent) => void): Promise<Unlisten>;
  onTurnError(cb: (e: TurnErrorEvent) => void): Promise<Unlisten>;
  onTurnStats(cb: (e: TurnStatsEvent) => void): Promise<Unlisten>;
  onIntention(cb: (e: IntentionSignal) => void): Promise<Unlisten>;
  /** A session's title was regenerated as an LLM summary after its first turn
   *  (#671 item 2b). Patch the cached session title in place -- no refetch. */
  onSessionTitleUpdated(
    cb: (e: SessionTitleUpdatedEvent) => void,
  ): Promise<Unlisten>;
  onToolCall(cb: (e: ToolCallEvent) => void): Promise<Unlisten>;
  onToolOutput(cb: (e: ToolOutputChunkEvent) => void): Promise<Unlisten>;
  onToolResult(cb: (e: ToolResultEvent) => void): Promise<Unlisten>;
  /** Live stdout/stderr from a background process started via `process_manager`
   *  (#873). Flows across turns for the life of the process (unlike the per-turn
   *  `tool:output`); keyed by `processId`, no `messageId`. */
  onProcessOutput(cb: (e: ProcessOutputEvent) => void): Promise<Unlisten>;
  /** A background process ended (exited / killed / failed) — terminal, emitted
   *  once after its last `process:output`. */
  onProcessExited(cb: (e: ProcessExitedEvent) => void): Promise<Unlisten>;
  /** A session's active observer set changed — one started, stopped, or fired
   *  (#1038). Coarse: the handler re-runs `listObservers(sessionId)`. */
  onObserverChanged(cb: (e: ObserverChangedEvent) => void): Promise<Unlisten>;
  /** An embedded terminal's shell exited (#1284) — `exit`, a crash, or our own
   *  kill. One per terminal for its whole life, which is why this is an event
   *  while the byte stream is a channel. An id the drawer no longer knows is
   *  expected (closing a tab kills the shell) and ignored. */
  onTerminalExited(cb: (e: TerminalExitedEvent) => void): Promise<Unlisten>;
  onApprovalRequest(
    cb: (e: ToolApprovalRequestEvent) => void,
  ): Promise<Unlisten>;
  /** Active skill set changed (activate/deactivate, or an install/uninstall reload). */
  onSkillsChanged(cb: (e: SkillsChangedEvent) => void): Promise<Unlisten>;
  /** An `optimizeSkill` proposal is awaiting approval; render a diff and reply via
   *  `respondApproval` (keyed by `requestId` as both session and call id). */
  onEvolveApprovalRequest(
    cb: (e: SkillEvolveApprovalRequestEvent) => void,
  ): Promise<Unlisten>;
  /** The agent asked the user a clarifying question (#44, `ask_user`); render a
   *  prompt and reply via `respondAsk`. */
  onAskRequest(cb: (e: ToolAskRequestEvent) => void): Promise<Unlisten>;
  /** One or more MCP servers changed status (start/stop/restart/connect failure, or
   *  an enable/disable/add/remove reload). Carries the full snapshot; re-fetch
   *  definitions via `listMcpServers`. */
  onMcpStatusChanged(cb: (e: McpStatusChangedEvent) => void): Promise<Unlisten>;
  /** A silent context-pressure memory flush wrote durable facts to on-disk memory
   *  mid-turn (#283). Fires only when something was written, so the memory browser
   *  can surface provenance. Re-read files via `listMemoryFiles`/`memoryOverview`. */
  onMemoryFlushed(cb: (e: MemoryFlushedEvent) => void): Promise<Unlisten>;
  /** A scheduled task fired (via cron or `runScheduledTaskNow`) and produced a run.
   *  Carries the `RunRecord` so the UI can cache the run's `sessionId` for the ↗
   *  open-session jump. See the CONTRACT CHANGE note on `runScheduledTaskNow`. */
  onScheduledFired(cb: (e: RunRecord) => void): Promise<Unlisten>;
  /** A scheduled task's derived state changed (a fire recomputed next/last, or a
   *  create/delete/toggle the runner applied). Carries the full task-list snapshot,
   *  which replaces the store wholesale (mirrors `onMcpStatusChanged`) so `Next` /
   *  `Last` update live without a reload. */
  onScheduledChanged(cb: (e: ScheduledTask[]) => void): Promise<Unlisten>;
  // CONTRACT CHANGE (#301, surface unavailable skill-required MCP servers): NEW
  // event needing a real Rust emitter — please review @backend-owner. PR #296
  // added the backend compute (`AppState::unavailable_required_servers`) but it is
  // `tracing::warn!`-only; this carries that list to the UI. It must be emitted
  // from the phenotype activation path (`switch_phenotype` + `set_session_phenotype`)
  // alongside the existing warn — non-fatal, never blocks activation. The payload
  // is FE-owned (lib/phenotype-mcp.ts) until the backend adds a ts-rs binding;
  // `bindings/` is untouched. Mocked under `VITE_FF_MOCK=1` for now.
  /** A just-activated phenotype lists a skill whose declared MCP server is absent
   *  from `mcp.json` or present but not running, so its bridged tools are silently
   *  unavailable. Fires only when the unavailable list is non-empty. */
  onPhenotypeMcpUnavailable(
    cb: (e: PhenotypeMcpUnavailableEvent) => void,
  ): Promise<Unlisten>;
  /** A just-activated phenotype declares `preheat` tools that could not all be
   *  admitted to the resident block — unknown names, or the byte budget ran out.
   *  Non-fatal: the turn proceeds, the dropped tools stay behind `tool_search`.
   *  Fires only when something was actually dropped (#1179). */
  onPhenotypePreheatDropped(
    cb: (e: PhenotypePreheatDroppedEvent) => void,
  ): Promise<Unlisten>;
  // FE completion of the already-merged backend emit (#566, #568). `install_update`
  // emits `update:progress` per downloaded chunk (cumulative bytes; `total` is the
  // content length, `null` -> indeterminate bar), then a terminal
  // `update:download-finished` before the auto-relaunch. The event names + payload
  // are fixed by the backend; the binding (`UpdateProgressEvent`) is generated.
  /** Self-update download progress: cumulative bytes downloaded + content length. */
  onUpdateProgress(cb: (e: UpdateProgressEvent) => void): Promise<Unlisten>;
  /** The self-update download finished; the app relaunches shortly after. */
  onUpdateDownloadFinished(cb: () => void): Promise<Unlisten>;
  // CONTRACT CHANGE (#561, live-sync git branch when HEAD changes): NEW event
  // needing a real Rust emitter — please review @backend-owner. The backend's
  // `GitHeadWatcher` (mirroring `McpConfigWatcher`/`SkillWatcher`) watches the
  // active workspace's `.git/HEAD` and, on a debounced change, re-resolves
  // `git_branch(root)` and emits `workspace:branch-changed`. The payload reuses
  // the existing `SessionWorkspace` (`{ path, gitBranch }`) — NO new ts-rs
  // binding; `bindings/` is untouched. Detached HEAD carries `gitBranch: null`,
  // preserving the existing semantics. Mocked under `VITE_FF_MOCK=1` (a synthetic
  // emit on `setSessionWorkspace`) so the reactive path is exercisable without a
  // watcher.
  /** The active workspace's git HEAD changed on disk — a terminal checkout,
   *  rebase, or the assistant's own `bash` switching branches. Patches the cached
   *  `gitBranch` for every session sharing `path`; no remount, no reload. */
  onWorkspaceBranchChanged(
    cb: (e: SessionWorkspace) => void,
  ): Promise<Unlisten>;
  // Paint-first boot (#599): the backend emits this once `AppState::new()` has
  // finished and the state is managed. Pair with `isAppReady` (subscribe-then-
  // check) to close the race where the event fired before the listener attached.
  /** The backend finished its deferred heavy init and is ready for command work. */
  onAppReady(cb: () => void): Promise<Unlisten>;
  // Error-surface counterpart to `onAppReady` (#599 boot regression): emitted on
  // ANY early-exit path in the background hydrate task (an `AppState::new()`
  // panic, or a panic in the post-init wiring — `init_mcp` / git watcher /
  // reaper / scheduler setup) so the FE can surface an actionable error instead
  // of hanging on `<BootSplash>` forever. Payload is the human-readable reason.
  // Pair with a timeout in the FE (App.tsx) for the case where the task dies
  // without emitting.
  /** The background hydrate task failed; `reason` explains why. Never fires
   *  alongside `onAppReady`. */
  onAppInitError(cb: (reason: string) => void): Promise<Unlisten>;
  /** A goal advanced a boundary or changed status (RFC 0020 §7, #717) — emitted at
   *  each iteration boundary and on set/pause/resume/complete so the goal status
   *  panel re-renders without polling (same pattern as `scheduled:changed`). The
   *  payload is the bare `Goal` (backend `emit("goal:updated", &goal)`). */
  onGoalUpdated(cb: (goal: Goal) => void): Promise<Unlisten>;
  /** A goal was cleared/aborted (`goal_clear`) — carries the bare `sessionId`. The
   *  panel drops that session's goal from the store and unmounts. Distinct from a
   *  terminal `goal:updated`, which never fires on clear. */
  onGoalCleared(cb: (sessionId: string) => void): Promise<Unlisten>;
}

// Explicit mock flag OR auto-fallback when not inside a Tauri window.
//
// The `!IN_TAURI` auto-fallback is gated behind `import.meta.env.DEV` on purpose:
// it only matters when running `pnpm dev` in a plain browser. A *production*
// build always runs inside Tauri, so `DEV` is statically `false` there and the
// whole `USE_MOCK` expression const-folds to `false` (when VITE_FF_MOCK isn't
// set) — letting Rollup dead-code-eliminate the `await import("./mock")` branch
// below, so `mock.ts` never ships in a desktop binary. (A dynamic import alone
// isn't enough: a runtime-dependent `USE_MOCK` keeps the chunk on disk.)
const IN_TAURI =
  globalThis.window !== undefined && "__TAURI_INTERNALS__" in globalThis.window;
const USE_MOCK =
  import.meta.env.VITE_FF_MOCK === "1" || (import.meta.env.DEV && !IN_TAURI);

if (import.meta.env.DEV && !IN_TAURI && import.meta.env.VITE_FF_MOCK !== "1") {
  console.warn(
    "[FlowForge] Not running inside Tauri — falling back to MockIpc.\n" +
      "Set VITE_FF_MOCK=1 to silence this, or use `pnpm tauri dev` for the real backend.",
  );
}

class TauriIpc implements FfIpc {
  private readonly invoke = async <T>(
    cmd: string,
    args?: Record<string, unknown>,
  ): Promise<T> => {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<T>(cmd, args);
  };

  private listen = async <T>(
    event: string,
    cb: (e: T) => void,
  ): Promise<Unlisten> => {
    const { listen } = await import("@tauri-apps/api/event");
    return listen<T>(event, (ev) => cb(ev.payload));
  };

  createSession = (goal?: string) =>
    this.invoke<Session>("create_session", { goal });
  isAppReady = (): Promise<boolean> => this.invoke<boolean>("is_app_ready");
  forkSession = (sessionId: string) =>
    this.invoke<Session>("fork_session", { sessionId });
  listMemoryFiles = () => this.invoke<MemoryFileInfo[]>("list_memory_files");
  readMemoryFile = (relPath: string) =>
    this.invoke<string>("read_memory_file", { relPath });
  memoryOverview = () => this.invoke<MemoryOverview>("memory_overview");
  writeCuratedMemory = (stratum: Stratum, text: string) =>
    this.invoke<void>("write_curated_memory", { stratum, text });
  listMemoryChunks = () => this.invoke<MemoryChunkStat[]>("list_memory_chunks");
  resetMemoryChunk = (chunkKey: string) =>
    this.invoke<void>("reset_memory_chunk", { chunkKey });
  sleepMemoryChunk = (chunkKey: string) =>
    this.invoke<void>("sleep_memory_chunk", { chunkKey });
  setMemoryChunkPinned = (chunkKey: string, pinned: boolean) =>
    this.invoke<void>("set_memory_chunk_pinned", { chunkKey, pinned });
  listSessions = () => this.invoke<Session[]>("list_sessions");
  getMessages = (sessionId: string, limit?: number) =>
    this.invoke<Message[]>("get_messages", { sessionId, limit });
  exportSession = (sessionId: string, format: Format) =>
    this.invoke<string>("export_session", { sessionId, format });
  renameSession = (sessionId: string, title: string) =>
    this.invoke<void>("rename_session", { sessionId, title });
  deleteSession = (sessionId: string) =>
    this.invoke<void>("delete_session", { sessionId });
  getSessionWorkspace = (sessionId: string) =>
    this.invoke<SessionWorkspace>("get_session_workspace", { sessionId });
  setSessionWorkspace = (sessionId: string, path: string) =>
    this.invoke<string>("set_session_workspace", { sessionId, path });
  listBranches = (sessionId: string) =>
    this.invoke<string[]>("list_branches", { sessionId });
  checkoutBranch = (sessionId: string, branch: string) =>
    this.invoke<SessionWorkspace>("checkout_branch", { sessionId, branch });
  listDirectory = (sessionId: string, path: string) =>
    this.invoke<DirEntry[]>("list_directory", { sessionId, path });
  readFile = (sessionId: string, path: string, maxBytes?: number) =>
    this.invoke<FileContent>("read_file", { sessionId, path, maxBytes });
  // The one `Channel` in the app (#1284). Everything else streams over the event
  // system, but Tauri's own docs say that system is "not designed for low latency
  // or high throughput" and point at channels for streams — and a terminal is the
  // throughput case (a build log, `cat` on a big file). The backend sends raw
  // bytes, which arrive here as an `ArrayBuffer` rather than a JSON number array.
  openTerminal = async (
    sessionId: string,
    cols: number,
    rows: number,
    onData: (bytes: Uint8Array) => void,
  ) => {
    const { Channel, invoke } = await import("@tauri-apps/api/core");
    const channel = new Channel<ArrayBuffer>();
    channel.onmessage = (message) => onData(new Uint8Array(message));
    return invoke<string>("terminal_open", {
      sessionId,
      cols,
      rows,
      onOutput: channel,
    });
  };
  writeTerminal = (terminalId: string, data: string) =>
    this.invoke<void>("terminal_write", { terminalId, data });
  resizeTerminal = (terminalId: string, cols: number, rows: number) =>
    this.invoke<void>("terminal_resize", { terminalId, cols, rows });
  closeTerminal = (terminalId: string) =>
    this.invoke<void>("terminal_close", { terminalId });
  sendMessage = (
    sessionId: string,
    content: string,
    attachments?: Attachment[],
  ) => this.invoke<string>("send_message", { sessionId, content, attachments });
  editMessage = (
    sessionId: string,
    messageId: string,
    content: string,
    attachments?: Attachment[],
  ) =>
    this.invoke<string>("edit_message", {
      sessionId,
      messageId,
      content,
      attachments,
    });
  cancelTurn = (sessionId: string) =>
    this.invoke<void>("cancel_turn", { sessionId });
  respondApproval = (sessionId: string, callId: string, approved: boolean) =>
    this.invoke<void>("respond_approval", { sessionId, callId, approved });
  respondAsk = (sessionId: string, callId: string, answer: string) =>
    this.invoke<void>("respond_ask", { sessionId, callId, answer });
  setSessionApprove = (sessionId: string, tool: string) =>
    this.invoke<void>("set_session_approve", { sessionId, tool });
  setAlwaysApprove = (tool: string) =>
    this.invoke<void>("set_always_approve", { tool });
  removeAlwaysApprove = (tool: string) =>
    this.invoke<void>("remove_always_approve", { tool });
  listAlwaysApproved = () => this.invoke<string[]>("list_always_approved");

  getProviderConfig = () => this.invoke<ProviderConfig>("get_provider_config");
  setProviderConfig = (
    kind: ProviderKind,
    baseUrl: string | undefined,
    model: string,
    thinking: boolean,
  ) =>
    this.invoke<ProviderConfig>("set_provider_config", {
      kind,
      baseUrl,
      model,
      thinking,
    });
  getProviderRegistry = () =>
    this.invoke<ProviderRegistry>("get_provider_registry");
  setActiveConnection = (id: string) =>
    this.invoke<void>("set_active_connection", { id });
  upsertConnection = (conn: ProviderConnection) =>
    this.invoke<ProviderConnection>("upsert_connection", { conn });
  removeConnection = (id: string) =>
    this.invoke<void>("remove_connection", { id });
  listModels = (id?: string) => this.invoke<string[]>("list_models", { id });
  setProviderSecret = (connectionId: string, kind: SecretKind, value: string) =>
    this.invoke<void>("set_provider_secret", { connectionId, kind, value });
  clearProviderSecret = (connectionId: string, kind: SecretKind) =>
    this.invoke<void>("clear_provider_secret", { connectionId, kind });
  providerSecretPresence = (connectionId: string) =>
    this.invoke<SecretKind[]>("provider_secret_presence", { connectionId });
  resolvedBedrockAuth = (connectionId: string) =>
    this.invoke<BedrockAuth | null>("resolved_bedrock_auth", { connectionId });
  testConnection = (id?: string) =>
    this.invoke<void>("test_connection", { id });

  getSearchConfig = () => this.invoke<SearchConfig>("get_search_config");
  setSearchConfig = (
    backend: SearchBackend,
    baseUrl: string | undefined,
    email?: string | undefined,
  ) =>
    this.invoke<SearchConfig>("set_search_config", { backend, baseUrl, email });

  getControlConfig = () => this.invoke<ControlConfig>("get_control_config");
  setControlConfig = (config: ControlConfig) =>
    this.invoke<ControlConfig>("set_control_config", { config });
  getPermissionMatrix = () =>
    this.invoke<PermissionMatrixView>("get_permission_matrix");
  setPermissionCell = (mode: Mode, safety: Safety, cell: PermissionCell) =>
    this.invoke<PermissionMatrixView>("set_permission_cell", {
      mode,
      safety,
      cell,
    });
  setToolOverride = (tool: string, cell: PermissionCell) =>
    this.invoke<PermissionMatrixView>("set_tool_override", { tool, cell });
  removeToolOverride = (tool: string) =>
    this.invoke<PermissionMatrixView>("remove_tool_override", { tool });
  warmup = () => this.invoke<void>("warmup");

  searchInSession = (sessionId: string, query: string) =>
    this.invoke<SearchHit[]>("search_in_session", { sessionId, query });
  searchMessages = (query: string, limit?: number) =>
    this.invoke<SearchHit[]>("search_messages", { query, limit });

  listSkills = () => this.invoke<SkillInfo[]>("list_skills");
  searchSkills = (query: string) =>
    this.invoke<SkillInfo[]>("search_skills", { query });
  searchSkillMarketplace = (query: string) =>
    this.invoke<MarketplaceSkill[]>("search_skill_marketplace", { query });
  activateSkill = (name: string) =>
    this.invoke<void>("activate_skill", { name });
  deactivateSkill = (name: string) =>
    this.invoke<void>("deactivate_skill", { name });
  getSkillTelemetry = (skill: string) =>
    this.invoke<SkillAggregate | null>("get_skill_telemetry", { skill });
  optimizeSkill = (sessionId: string, skill: string) =>
    this.invoke<string>("optimize_skill", { sessionId, skill });
  rollbackSkill = (skill: string, version: string) =>
    this.invoke<void>("rollback_skill", { skill, version });
  listSkillVersions = (skill: string) =>
    this.invoke<string[]>("list_skill_versions", { skill });

  listPhenotypes = () => this.invoke<Phenotype[]>("list_phenotypes");
  getPhenotype = () => this.invoke<Phenotype>("get_phenotype");
  switchPhenotype = (name: string) =>
    this.invoke<Phenotype>("switch_phenotype", { name });
  setSessionPhenotype = (sessionId: string, name: string | null) =>
    this.invoke<void>("set_session_phenotype", { sessionId, name });
  updatePhenotype = (phenotype: Phenotype) =>
    this.invoke<Phenotype>("update_phenotype", { phenotype });
  setSessionModelSelection = (
    sessionId: string,
    selection: ModelSelection | null,
  ) =>
    this.invoke<void>("set_session_model_selection", { sessionId, selection });
  getSessionModelSelection = (sessionId: string) =>
    this.invoke<ModelSelection | null>("get_session_model_selection", {
      sessionId,
    });
  resolveModelSelection = (sessionId: string) =>
    this.invoke<ResolvedModel>("resolve_model_selection", { sessionId });
  setSessionMode = (sessionId: string, mode: Mode | null) =>
    this.invoke<void>("set_session_mode", { sessionId, mode });
  getDefaultMode = () => this.invoke<Mode>("get_default_mode");
  setDefaultMode = (mode: Mode) =>
    this.invoke<void>("set_default_mode", { mode });
  searchProfileMarketplace = (query: string) =>
    this.invoke<MarketplaceProfile[]>("search_profile_marketplace", { query });
  listScheduledTasks = () =>
    this.invoke<ScheduledTask[]>("list_scheduled_tasks");
  toggleScheduledTask = (id: string) =>
    this.invoke<ScheduledTask>("toggle_scheduled_task", { id });
  createScheduledTask = (input: CreateScheduledTaskInput) =>
    this.invoke<ScheduledTask>("create_scheduled_task", { input });
  deleteScheduledTask = (id: string) =>
    this.invoke<void>("delete_scheduled_task", { id });
  previewCadence = (cron: string) =>
    this.invoke<string>("preview_cadence", { cron });
  runScheduledTaskNow = (id: string) =>
    this.invoke<RunRecord>("run_scheduled_task_now", { id });
  listScheduledRuns = (id: string) =>
    this.invoke<RunRecord[]>("list_scheduled_runs", { id });
  setScheduledPausedAll = (paused: boolean) =>
    this.invoke<boolean>("set_scheduled_paused_all", { paused });

  listMcpServers = () => this.invoke<McpServerStatus[]>("list_mcp_servers");
  restartMcpServer = (id: string) =>
    this.invoke<void>("restart_mcp_server", { id });
  setMcpServerEnabled = (id: string, enabled: boolean) =>
    this.invoke<void>("set_mcp_server_enabled", { id, enabled });
  addMcpServer = (def: McpServerConfig) =>
    this.invoke<void>("add_mcp_server", { def });
  removeMcpServer = (id: string) =>
    this.invoke<void>("remove_mcp_server", { id });

  checkForUpdates = (channel: UpdateChannel) =>
    this.invoke<UpdateStatus>("check_for_updates", { channel });
  installUpdate = (
    channel: UpdateChannel,
    expectedVersion: string,
    allowDowngrade = false,
  ) =>
    this.invoke<void>("install_update", {
      channel,
      expectedVersion,
      allowDowngrade,
    });
  startDevUpdateWatcher = () => this.invoke<void>("start_dev_update_watcher");
  onLocalFeedChanged = (cb: () => void) =>
    this.listen<void>("update:local-feed-changed", cb);
  exportBackup = () => this.invoke<BackupResult>("export_backup");
  restoreBackup = () => this.invoke<BackupResult>("restore_backup");
  runSidecarTurn = (prompt: string) =>
    this.invoke<SidecarTurnResult>("run_sidecar_turn", { prompt });

  goalSet = (
    sessionId: string,
    objective: string,
    maxIterations?: number,
    maxTokens?: number,
    maxWallMs?: number,
    allowProposePr?: boolean,
  ) =>
    this.invoke<Goal>("goal_set", {
      sessionId,
      objective,
      maxIterations,
      maxTokens,
      maxWallMs,
      allowProposePr,
    });
  goalStatus = (sessionId: string) =>
    this.invoke<Goal | null>("goal_status", { sessionId });
  goalPause = (sessionId: string) =>
    this.invoke<Goal | null>("goal_pause", { sessionId });
  goalResume = (sessionId: string) =>
    this.invoke<Goal | null>("goal_resume", { sessionId });
  goalClear = (sessionId: string) =>
    this.invoke<void>("goal_clear", { sessionId });

  notebookStatus = (sessionId: string) =>
    this.invoke<NotebookKernelState>("notebook_status", { sessionId });
  notebookStop = (sessionId: string, kernelId?: string) =>
    this.invoke<void>("notebook_stop", { sessionId, kernelId });
  notebookRestart = (sessionId: string, kernelId?: string) =>
    this.invoke<NotebookKernelState>("notebook_restart", {
      sessionId,
      kernelId,
    });

  listObservers = (sessionId: string) =>
    this.invoke<ObserverInfo[]>("list_observers", { sessionId });
  stopObserver = (id: number, sessionId: string) =>
    this.invoke<void>("stop_observer", { id, sessionId });

  onToken = (cb: (e: TokenEvent) => void) =>
    this.listen<TokenEvent>("turn:token", cb);
  onReasoning = (cb: (e: ReasoningEvent) => void) =>
    this.listen<ReasoningEvent>("turn:reasoning", cb);
  onTurnDone = (cb: (e: TurnDoneEvent) => void) =>
    this.listen<TurnDoneEvent>("turn:done", cb);
  onTurnError = (cb: (e: TurnErrorEvent) => void) =>
    this.listen<TurnErrorEvent>("turn:error", cb);
  onTurnStats = (cb: (e: TurnStatsEvent) => void) =>
    this.listen<TurnStatsEvent>("turn:stats", cb);
  onIntention = (cb: (e: IntentionSignal) => void) =>
    this.listen<IntentionSignal>("signal:intention", cb);
  onSessionTitleUpdated = (cb: (e: SessionTitleUpdatedEvent) => void) =>
    this.listen<SessionTitleUpdatedEvent>("session:title-updated", cb);
  onToolCall = (cb: (e: ToolCallEvent) => void) =>
    this.listen<ToolCallEvent>("tool:call", cb);
  onToolOutput = (cb: (e: ToolOutputChunkEvent) => void) =>
    this.listen<ToolOutputChunkEvent>("tool:output", cb);
  onToolResult = (cb: (e: ToolResultEvent) => void) =>
    this.listen<ToolResultEvent>("tool:result", cb);
  onProcessOutput = (cb: (e: ProcessOutputEvent) => void) =>
    this.listen<ProcessOutputEvent>("process:output", cb);
  onProcessExited = (cb: (e: ProcessExitedEvent) => void) =>
    this.listen<ProcessExitedEvent>("process:exited", cb);
  onObserverChanged = (cb: (e: ObserverChangedEvent) => void) =>
    this.listen<ObserverChangedEvent>("observer:changed", cb);
  onTerminalExited = (cb: (e: TerminalExitedEvent) => void) =>
    this.listen<TerminalExitedEvent>("terminal:exited", cb);
  onApprovalRequest = (cb: (e: ToolApprovalRequestEvent) => void) =>
    this.listen<ToolApprovalRequestEvent>("tool:approval-request", cb);
  onAskRequest = (cb: (e: ToolAskRequestEvent) => void) =>
    this.listen<ToolAskRequestEvent>("tool:ask-request", cb);
  onSkillsChanged = (cb: (e: SkillsChangedEvent) => void) =>
    this.listen<SkillsChangedEvent>("skills:changed", cb);
  onEvolveApprovalRequest = (
    cb: (e: SkillEvolveApprovalRequestEvent) => void,
  ) =>
    this.listen<SkillEvolveApprovalRequestEvent>(
      "skill:evolve-approval-request",
      cb,
    );
  onMcpStatusChanged = (cb: (e: McpStatusChangedEvent) => void) =>
    this.listen<McpStatusChangedEvent>("mcp:status-changed", cb);
  onMemoryFlushed = (cb: (e: MemoryFlushedEvent) => void) =>
    this.listen<MemoryFlushedEvent>("memory:flushed", cb);
  onScheduledFired = (cb: (e: RunRecord) => void) =>
    this.listen<RunRecord>("scheduled:fired", cb);
  onScheduledChanged = (cb: (e: ScheduledTask[]) => void) =>
    this.listen<ScheduledTask[]>("scheduled:changed", cb);
  onPhenotypeMcpUnavailable = (cb: (e: PhenotypeMcpUnavailableEvent) => void) =>
    this.listen<PhenotypeMcpUnavailableEvent>("phenotype:mcp-unavailable", cb);
  onPhenotypePreheatDropped = (cb: (e: PhenotypePreheatDroppedEvent) => void) =>
    this.listen<PhenotypePreheatDroppedEvent>("phenotype:preheat-dropped", cb);
  onUpdateProgress = (cb: (e: UpdateProgressEvent) => void) =>
    this.listen<UpdateProgressEvent>("update:progress", cb);
  onUpdateDownloadFinished = (cb: () => void) =>
    this.listen<void>("update:download-finished", () => cb());
  onWorkspaceBranchChanged = (cb: (e: SessionWorkspace) => void) =>
    this.listen<SessionWorkspace>("workspace:branch-changed", cb);
  onAppReady = (cb: () => void) => this.listen<void>("app:ready", () => cb());
  onAppInitError = (cb: (reason: string) => void) =>
    this.listen<string>("app:init-error", (reason) => cb(reason));
  onGoalUpdated = (cb: (goal: Goal) => void) =>
    this.listen<Goal>("goal:updated", cb);
  onGoalCleared = (cb: (sessionId: string) => void) =>
    this.listen<string>("goal:cleared", cb);
}

// `MockIpc` is pulled in with a dynamic import so the bundler gives it its own
// chunk and leaves it out of production builds. A static import could NOT be
// tree-shaken here: `USE_MOCK` depends on the runtime `!IN_TAURI` check, so the
// branch isn't statically constant-foldable and `MockIpc` (plus its transitive
// imports) would ship in every desktop binary as dead weight.
//
// Top-level await keeps `ipc` a resolved `FfIpc` value (not a Promise), so the
// store and event wiring keep reading it synchronously — they only run after
// module init, which the await covers.
async function createIpc(): Promise<FfIpc> {
  if (USE_MOCK) {
    const { MockIpc } = await import("./mock");
    return new MockIpc();
  }
  return new TauriIpc();
}

export const ipc: FfIpc = await createIpc();
