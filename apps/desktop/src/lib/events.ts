// One-time wiring of backend events into the chat store. Events are routed by
// sessionId/messageId inside the store, so background sessions keep streaming
// correctly even when they're not on screen.

import { ipc } from "./ipc";
import { TokenBatcher } from "./token-batcher";
import { useChatStore } from "@/store/chat";
import { useSkillsStore } from "@/store/skills";
import { useMcpStore } from "@/store/mcp";

let started = false;

/** Idempotent: safe under React StrictMode double-effects. Subscriptions live
 *  for the app lifetime, so we never unlisten. */
export function startIpcEvents(): void {
  if (started) return;
  started = true;

  const store = useChatStore.getState();

  // Coalesce tokens to animation-frame cadence (#104). Any event that depends on
  // the streamed text being applied first (turn finish/error, tool calls) drains
  // the batcher synchronously before it runs, preserving order.
  const tokens = new TokenBatcher(store.applyToken, (cb) =>
    requestAnimationFrame(cb),
  );

  void ipc.onToken((e) => tokens.push(e));
  void ipc.onTurnDone((e) => {
    tokens.drain();
    store.finishTurn(e);
  });
  void ipc.onTurnError((e) => {
    tokens.drain();
    store.failTurn(e);
  });
  void ipc.onToolCall((e) => {
    tokens.drain();
    store.applyToolCall(e);
  });
  void ipc.onToolResult(store.applyToolResult);
  void ipc.onApprovalRequest(store.applyApprovalRequest);
  void ipc.onAskRequest(store.applyAskRequest);
  void ipc.onSkillsChanged(() => {
    void useSkillsStore.getState().refresh();
  });
  // MCP status snapshots replace the store wholesale (#91); mirrors skills:changed.
  void ipc.onMcpStatusChanged((e) => {
    useMcpStore.getState().setServers(e.servers);
  });
  // No UI for intention signals yet (NeuroForge, M8) — observe only.
  void ipc.onIntention((e) => {
    console.debug("[signal:intention]", e.sessionId, e.goal);
  });
}
