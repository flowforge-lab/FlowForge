// Typed IPC contract — the single seam between the React frontend and the Rust backend.
//
// Every command and event the backend exposes is mirrored here with the generated
// `bindings` types. Set `VITE_FF_MOCK=1` to run the frontend against an in-browser
// mock that fulfils this exact contract, so UI work never blocks on the Rust side.

import type { UpdateStatus, BackupResult } from "@/lib/about";
import type { ControlConfig } from "@/lib/control";
import type { MarketplaceSkill } from "@/lib/marketplace";
import type { MarketplaceProfile } from "@/lib/profile-marketplace";
import type {
  ScheduledTask,
  CreateScheduledTaskInput,
  RunRecord,
} from "@/bindings";
import type { PhenotypeMcpUnavailableEvent } from "@/bindings";
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
  IntentionSignal,
  SessionTitleUpdatedEvent,
  ToolApprovalRequestEvent,
  ToolAskRequestEvent,
  ToolCallEvent,
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
} from "../bindings";
import type { Format } from "../bindings/Format";
import type { SecretKind } from "../bindings/SecretKind";
import type { BedrockAuth } from "../bindings/BedrockAuth";

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
  getMessages(sessionId: string): Promise<Message[]>;
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
  ): Promise<SearchConfig>;
  /** Best-effort nudge to wake the model server before the first turn. Never throws meaningfully. */
  warmup(): Promise<void>;

  // Memory (RFC 0006, M5.1e — frozen read-only surface for the Settings memory
  // pane, Issue #131). These three commands have real Rust impls + ts-rs bindings.
  // Writes, the enable/disable toggle, and embeddings are deliberately out of scope.
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
  // Salience surface (RFC 0007 M6.2, #293). Per-chunk weight/dormant + reset/pin.
  // `weight`/`dormant` are computed authoritatively by the backend; the FE never
  // re-derives the dormancy threshold. Decay/dormancy/pin never edit Markdown —
  // they only change ambient injection.
  /** Per-chunk salience stats: effective weight, dormant flag, access count, pin. */
  listMemoryChunks(): Promise<MemoryChunkStat[]>;
  /** Reset (wake) a chunk: weight back to 1.0, stamp last-accessed now. */
  resetMemoryChunk(chunkKey: string): Promise<void>;
  /** Pin/unpin a chunk: pinned holds weight at 1.0 and is never dormant. */
  setMemoryChunkPinned(chunkKey: string, pinned: boolean): Promise<void>;

  // Control settings (Issue #127). `ControlConfig` is a FE-owned shape (lib/control.ts):
  // there is no backend/ts-rs type yet, and the permission matrix does NOT map to
  // `ApprovalSafety` ("write"|"dangerous"). For now this round-trips presentation
  // state + mock storage only; it does not drive runtime approval.
  // TODO(#127 follow-up): once `ApprovalSafety` is extended to cover the
  // 4-row × 3-column matrix, replace `ControlConfig` with a generated `bindings`
  // type and wire `defaultMode`/`permissionPolicy` into runtime approval.
  /** Current persisted control settings (permissions presentation + prompts). */
  getControlConfig(): Promise<ControlConfig>;
  /** Persist control settings; resolves with the stored config. */
  setControlConfig(config: ControlConfig): Promise<ControlConfig>;

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
  /** Check for app updates. */
  checkForUpdates(): Promise<UpdateStatus>;
  /** Download and install the available update; the backend relaunches the app on
   *  success (so this never resolves in the real app — see #363 / #362). */
  installUpdate(): Promise<void>;
  /** Export a local backup. */
  exportBackup(): Promise<BackupResult>;
  /** Restore from a backup. */
  restoreBackup(): Promise<BackupResult>;

  // Events (backend -> frontend)
  onToken(cb: (e: TokenEvent) => void): Promise<Unlisten>;
  onReasoning(cb: (e: ReasoningEvent) => void): Promise<Unlisten>;
  onTurnDone(cb: (e: TurnDoneEvent) => void): Promise<Unlisten>;
  onTurnError(cb: (e: TurnErrorEvent) => void): Promise<Unlisten>;
  onIntention(cb: (e: IntentionSignal) => void): Promise<Unlisten>;
  /** A session's title was regenerated as an LLM summary after its first turn
   *  (#671 item 2b). Patch the cached session title in place -- no refetch. */
  onSessionTitleUpdated(
    cb: (e: SessionTitleUpdatedEvent) => void,
  ): Promise<Unlisten>;
  onToolCall(cb: (e: ToolCallEvent) => void): Promise<Unlisten>;
  onToolResult(cb: (e: ToolResultEvent) => void): Promise<Unlisten>;
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
  listMemoryChunks = () => this.invoke<MemoryChunkStat[]>("list_memory_chunks");
  resetMemoryChunk = (chunkKey: string) =>
    this.invoke<void>("reset_memory_chunk", { chunkKey });
  setMemoryChunkPinned = (chunkKey: string, pinned: boolean) =>
    this.invoke<void>("set_memory_chunk_pinned", { chunkKey, pinned });
  listSessions = () => this.invoke<Session[]>("list_sessions");
  getMessages = (sessionId: string) =>
    this.invoke<Message[]>("get_messages", { sessionId });
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
  setSearchConfig = (backend: SearchBackend, baseUrl: string | undefined) =>
    this.invoke<SearchConfig>("set_search_config", { backend, baseUrl });

  getControlConfig = () => this.invoke<ControlConfig>("get_control_config");
  setControlConfig = (config: ControlConfig) =>
    this.invoke<ControlConfig>("set_control_config", { config });
  warmup = () => this.invoke<void>("warmup");

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

  checkForUpdates = () => this.invoke<UpdateStatus>("check_for_updates");
  installUpdate = () => this.invoke<void>("install_update");
  exportBackup = () => this.invoke<BackupResult>("export_backup");
  restoreBackup = () => this.invoke<BackupResult>("restore_backup");

  onToken = (cb: (e: TokenEvent) => void) =>
    this.listen<TokenEvent>("turn:token", cb);
  onReasoning = (cb: (e: ReasoningEvent) => void) =>
    this.listen<ReasoningEvent>("turn:reasoning", cb);
  onTurnDone = (cb: (e: TurnDoneEvent) => void) =>
    this.listen<TurnDoneEvent>("turn:done", cb);
  onTurnError = (cb: (e: TurnErrorEvent) => void) =>
    this.listen<TurnErrorEvent>("turn:error", cb);
  onIntention = (cb: (e: IntentionSignal) => void) =>
    this.listen<IntentionSignal>("signal:intention", cb);
  onSessionTitleUpdated = (cb: (e: SessionTitleUpdatedEvent) => void) =>
    this.listen<SessionTitleUpdatedEvent>("session:title-updated", cb);
  onToolCall = (cb: (e: ToolCallEvent) => void) =>
    this.listen<ToolCallEvent>("tool:call", cb);
  onToolResult = (cb: (e: ToolResultEvent) => void) =>
    this.listen<ToolResultEvent>("tool:result", cb);
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
  onUpdateProgress = (cb: (e: UpdateProgressEvent) => void) =>
    this.listen<UpdateProgressEvent>("update:progress", cb);
  onUpdateDownloadFinished = (cb: () => void) =>
    this.listen<void>("update:download-finished", () => cb());
  onWorkspaceBranchChanged = (cb: (e: SessionWorkspace) => void) =>
    this.listen<SessionWorkspace>("workspace:branch-changed", cb);
  onAppReady = (cb: () => void) => this.listen<void>("app:ready", () => cb());
  onAppInitError = (cb: (reason: string) => void) =>
    this.listen<string>("app:init-error", (reason) => cb(reason));
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
