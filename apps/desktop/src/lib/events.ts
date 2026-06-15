// One-time wiring of backend events into the chat store. Events are routed by
// sessionId/messageId inside the store, so background sessions keep streaming
// correctly even when they're not on screen.

import { ipc } from "./ipc";
import { useChatStore } from "@/store/chat";

let started = false;

/** Idempotent: safe under React StrictMode double-effects. Subscriptions live
 *  for the app lifetime, so we never unlisten. */
export function startIpcEvents(): void {
  if (started) return;
  started = true;

  const store = useChatStore.getState();

  void ipc.onToken(store.applyToken);
  void ipc.onTurnDone(store.finishTurn);
  void ipc.onTurnError(store.failTurn);
  void ipc.onToolCall(store.applyToolCall);
  void ipc.onToolResult(store.applyToolResult);
  void ipc.onApprovalRequest(store.applyApprovalRequest);
  void ipc.onAskRequest(store.applyAskRequest);
  // No UI for intention signals yet (NeuroForge, M8) — observe only.
  void ipc.onIntention((e) => {
    console.debug("[signal:intention]", e.sessionId, e.goal);
  });
}
