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

  // Events (backend -> frontend)
  onToken(cb: (e: TokenEvent) => void): Promise<Unlisten>;
  onTurnDone(cb: (e: TurnDoneEvent) => void): Promise<Unlisten>;
  onTurnError(cb: (e: TurnErrorEvent) => void): Promise<Unlisten>;
  onIntention(cb: (e: IntentionSignal) => void): Promise<Unlisten>;
  onToolCall(cb: (e: ToolCallEvent) => void): Promise<Unlisten>;
  onToolResult(cb: (e: ToolResultEvent) => void): Promise<Unlisten>;
}

// Explicit mock flag OR auto-fallback when not inside a Tauri window.
// This lets `pnpm dev` work in a plain browser without the flag; Tauri desktop
// (`pnpm tauri dev`) still uses the real IPC.
const IN_TAURI =
  globalThis.window !== undefined && "__TAURI_INTERNALS__" in globalThis.window;
const USE_MOCK = import.meta.env.VITE_FF_MOCK === "1" || !IN_TAURI;

if (!IN_TAURI && import.meta.env.VITE_FF_MOCK !== "1") {
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
}

// Mock is referenced only when VITE_FF_MOCK=1; production builds const-fold the
// flag and bundlers drop the unused branch.
import { MockIpc } from "./mock";

export const ipc: FfIpc = USE_MOCK ? new MockIpc() : new TauriIpc();
