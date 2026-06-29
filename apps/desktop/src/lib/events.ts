// One-time wiring of backend events into the chat store. Events are routed by
// sessionId/messageId inside the store, so background sessions keep streaming
// correctly even when they're not on screen.

import { ipc } from "./ipc";
import { TokenBatcher } from "./token-batcher";
import { useChatStore } from "@/store/chat";
import { useSkillsStore } from "@/store/skills";
import { useMcpStore } from "@/store/mcp";
import { useMemoryStore } from "@/store/memory";
import { usePhenoMcpNoticeStore } from "@/store/pheno-mcp-notice";
import { useScheduledStore } from "@/store/scheduled";

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
  const reasoning = new TokenBatcher(store.applyReasoning, (cb) =>
    requestAnimationFrame(cb),
  );

  void ipc.onToken((e) => tokens.push(e));
  void ipc.onReasoning((e) => reasoning.push(e));
  void ipc.onTurnDone((e) => {
    tokens.drain();
    reasoning.drain();
    store.finishTurn(e);
  });
  void ipc.onTurnError((e) => {
    tokens.drain();
    reasoning.drain();
    store.failTurn(e);
  });
  void ipc.onToolCall((e) => {
    tokens.drain();
    reasoning.drain();
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
  // A silent context-pressure flush curated memory mid-turn (#283) — record it so
  // the memory browser surfaces provenance and reloads.
  void ipc.onMemoryFlushed((e) => {
    useMemoryStore.getState().noteFlush(e.writes);
  });
  // A just-activated phenotype lists a skill whose declared MCP server is
  // unavailable (#301) — surface it as a non-blocking toast. Activation itself
  // never blocks; this is informational.
  void ipc.onPhenotypeMcpUnavailable((e) => {
    usePhenoMcpNoticeStore.getState().show(e);
  });
  // A scheduled task fired (#543): cache the run's session for the ↗ jump and
  // optimistically stamp Last; the snapshot below is the source of truth. The fire
  // created a session out of band, so re-pull the session list (it lands in the
  // sidebar, and the ↗ jump can resolve its title/history).
  void ipc.onScheduledFired((e) => {
    useScheduledStore.getState().applyFired(e);
    void useChatStore.getState().refreshSessions();
  });
  // Scheduled-task state changed: a full snapshot replaces the list wholesale, so
  // Next / Last live-update without a reload (mirrors mcp:status-changed).
  void ipc.onScheduledChanged((e) => {
    useScheduledStore.getState().applyChanged(e);
  });
  // No UI for intention signals yet (NeuroForge, M8) — observe only.
  void ipc.onIntention((e) => {
    console.debug("[signal:intention]", e.sessionId, e.goal);
  });
}
