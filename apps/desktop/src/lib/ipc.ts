// Typed IPC contract — the single seam between the React frontend and the Rust backend.
//
// Every command and event the backend exposes is mirrored here with the generated
// `bindings` types. Set `VITE_FF_MOCK=1` to run the frontend against an in-browser
// mock that fulfils this exact contract, so UI work never blocks on the Rust side.

import type {
  Message,
  Session,
  TokenEvent,
  TurnDoneEvent,
  TurnErrorEvent,
  IntentionSignal,
  ToolApprovalRequestEvent,
  ToolCallEvent,
  ToolResultEvent,
} from "../bindings";

export type Unlisten = () => void;

export interface FfIpc {
  // Commands (frontend -> backend)
  createSession(goal?: string): Promise<Session>;
  listSessions(): Promise<Session[]>;
  getMessages(sessionId: string): Promise<Message[]>;
  /** Persists the user message and starts the assistant turn. Returns the user message id. */
  sendMessage(sessionId: string, content: string): Promise<string>;
  cancelTurn(sessionId: string): Promise<void>;
  /** Frontend's reply to a [`ToolApprovalRequestEvent`]. */
  respondApproval(callId: string, approved: boolean): Promise<void>;

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
  sendMessage = (sessionId: string, content: string) =>
    this.invoke<string>("send_message", { sessionId, content });
  cancelTurn = (sessionId: string) =>
    this.invoke<void>("cancel_turn", { sessionId });
  respondApproval = (callId: string, approved: boolean) =>
    this.invoke<void>("respond_approval", { callId, approved });

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
