// Typed IPC contract — the single seam between the React frontend and the Rust backend.
//
// Every command and event the backend exposes is mirrored here with the generated
// `bindings` types. Set `VITE_FF_MOCK=1` to run the frontend against an in-browser
// mock that fulfils this exact contract, so UI work never blocks on the Rust side.

import type { ControlConfig } from "@/lib/control";
import type { MarketplaceSkill } from "@/lib/marketplace";
import type { MarketplaceProfile } from "@/lib/profile-marketplace";
import type {
  Message,
  ProviderConfig,
  ProviderConnection,
  ProviderRegistry,
  ProviderKind,
  SearchConfig,
  SearchBackend,
  Session,
  TokenEvent,
  TurnDoneEvent,
  TurnErrorEvent,
  IntentionSignal,
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
} from "../bindings";

export type Unlisten = () => void;

export interface FfIpc {
  // Commands (frontend -> backend)
  createSession(goal?: string): Promise<Session>;
  listSessions(): Promise<Session[]>;
  getMessages(sessionId: string): Promise<Message[]>;
  /** Sets a session's persisted display title (server-truth). */
  renameSession(sessionId: string, title: string): Promise<void>;
  /** Persists the user message and starts the assistant turn. Returns the user message id. */
  sendMessage(sessionId: string, content: string): Promise<string>;
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

  // Provider settings (Issue #8). Phase 1: local candle-vllm + Ollama, no secrets.
  /** Current persisted LLM provider settings. */
  getProviderConfig(): Promise<ProviderConfig>;
  /** Persist provider settings; resolves with the stored config (e.g. `hasKey`). */
  setProviderConfig(
    kind: ProviderKind,
    baseUrl: string | undefined,
    model: string,
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
  /** All selectable phenotypes (built-in `default` + `~/.flowforge/phenotypes/`), name-sorted. */
  listPhenotypes(): Promise<Phenotype[]>;
  /** The active phenotype. */
  getPhenotype(): Promise<Phenotype>;
  /** Switch the active phenotype: replaces the active-skill set and persists the
   *  choice across restarts. Rejects an unknown name. Resolves with the phenotype now active. */
  switchPhenotype(name: string): Promise<Phenotype>;
  // CONTRACT NOTE (SET.7): FE-owned mock command — no backend/ts-rs binding for a
  // remote profile catalog exists yet. `MarketplaceProfile` lives in
  // `lib/profile-marketplace.ts` (mirroring SET.5's `MarketplaceSkill`);
  // `bindings/` is untouched. Replace with a generated binding + real command
  // when the profile marketplace backend lands.
  /** Search the (mock) profile marketplace. Empty query lists the full catalog. */
  searchProfileMarketplace(query: string): Promise<MarketplaceProfile[]>;

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

  // About section (SET.11). Mock no-ops until updater + backup backend lands.
  // Each resolves with a user-facing confirmation string for toasts.
  /** Check for app updates; mock always reports up to date. */
  checkForUpdates(): Promise<string>;
  /** Export a local backup; mock no-op. */
  exportBackup(): Promise<string>;
  /** Restore from a backup; mock no-op. */
  restoreBackup(): Promise<string>;

  // Events (backend -> frontend)
  onToken(cb: (e: TokenEvent) => void): Promise<Unlisten>;
  onTurnDone(cb: (e: TurnDoneEvent) => void): Promise<Unlisten>;
  onTurnError(cb: (e: TurnErrorEvent) => void): Promise<Unlisten>;
  onIntention(cb: (e: IntentionSignal) => void): Promise<Unlisten>;
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
  listSessions = () => this.invoke<Session[]>("list_sessions");
  getMessages = (sessionId: string) =>
    this.invoke<Message[]>("get_messages", { sessionId });
  renameSession = (sessionId: string, title: string) =>
    this.invoke<void>("rename_session", { sessionId, title });
  sendMessage = (sessionId: string, content: string) =>
    this.invoke<string>("send_message", { sessionId, content });
  cancelTurn = (sessionId: string) =>
    this.invoke<void>("cancel_turn", { sessionId });
  respondApproval = (sessionId: string, callId: string, approved: boolean) =>
    this.invoke<void>("respond_approval", { sessionId, callId, approved });
  respondAsk = (sessionId: string, callId: string, answer: string) =>
    this.invoke<void>("respond_ask", { sessionId, callId, answer });

  getProviderConfig = () => this.invoke<ProviderConfig>("get_provider_config");
  setProviderConfig = (
    kind: ProviderKind,
    baseUrl: string | undefined,
    model: string,
  ) =>
    this.invoke<ProviderConfig>("set_provider_config", {
      kind,
      baseUrl,
      model,
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
  searchProfileMarketplace = (query: string) =>
    this.invoke<MarketplaceProfile[]>("search_profile_marketplace", { query });

  listMcpServers = () => this.invoke<McpServerStatus[]>("list_mcp_servers");
  restartMcpServer = (id: string) =>
    this.invoke<void>("restart_mcp_server", { id });
  setMcpServerEnabled = (id: string, enabled: boolean) =>
    this.invoke<void>("set_mcp_server_enabled", { id, enabled });
  addMcpServer = (def: McpServerConfig) =>
    this.invoke<void>("add_mcp_server", { def });
  removeMcpServer = (id: string) =>
    this.invoke<void>("remove_mcp_server", { id });

  checkForUpdates = () => this.invoke<string>("check_for_updates");
  exportBackup = () => this.invoke<string>("export_backup");
  restoreBackup = () => this.invoke<string>("restore_backup");

  onToken = (cb: (e: TokenEvent) => void) =>
    this.listen<TokenEvent>("turn:token", cb);
  onTurnDone = (cb: (e: TurnDoneEvent) => void) =>
    this.listen<TurnDoneEvent>("turn:done", cb);
  onTurnError = (cb: (e: TurnErrorEvent) => void) =>
    this.listen<TurnErrorEvent>("turn:error", cb);
  onIntention = (cb: (e: IntentionSignal) => void) =>
    this.listen<IntentionSignal>("signal:intention", cb);
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
