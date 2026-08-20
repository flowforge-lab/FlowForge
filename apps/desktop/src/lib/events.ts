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
import { usePreheatNoticeStore } from "@/store/preheat-notice";
import { useScheduledStore } from "@/store/scheduled";
import { useUpdateStore } from "@/store/update";
import { useSessionWorkspaceStore } from "@/store/session-workspace";
import { useGoalStore } from "@/store/goal";
import { useProcessesStore } from "@/store/processes";
import { useObserversStore } from "@/store/observers";
import { useTerminalStore } from "@/store/terminal";

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
  void ipc.onToolOutput(store.applyToolOutputChunk);
  void ipc.onToolResult(store.applyToolResult);
  // Background-process output (#873/#987): a session-scoped, cross-turn sink,
  // independent of the turn/message lifecycle above. Bound methods dispatch
  // straight into the processes store — no token-batcher ordering to respect.
  const procs = useProcessesStore.getState();
  void ipc.onProcessOutput(procs.applyProcessOutput);
  void ipc.onProcessExited(procs.applyProcessExited);
  // An embedded terminal's shell ended (#1284). Marks the tab dead and leaves
  // its output readable; the drawer's byte stream does NOT come through here
  // (it is a per-terminal channel), only this one lifecycle signal.
  void ipc.onTerminalExited((e) =>
    useTerminalStore.getState().applyExited(e.terminalId),
  );
  // Observer panel (#1038, epic #954 M2): coarse `observer:changed` (start /
  // stop / fire) → re-list that session's observers. The panel also loads once
  // on mount; this keeps it live thereafter.
  void ipc.onObserverChanged((e) => {
    void useObserversStore.getState().refresh(e.sessionId);
  });
  void ipc.onTurnStats(store.applyTurnStats);
  void ipc.onApprovalRequest(store.applyApprovalRequest);
  void ipc.onAskRequest(store.applyAskRequest);
  void ipc.onSkillsChanged(() => {
    void useSkillsStore.getState().refresh();
  });
  // Seed the skills cache once at startup. It used to be filled lazily on the
  // first ⌘K open, which was fine when the palette was the only consumer — the
  // composer's slash autocomplete (#1036) reads it too, and must not offer an
  // empty skill list just because the palette has never been opened.
  void useSkillsStore.getState().refresh();
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
  // A just-activated phenotype declares preheat tools that could not all be admitted
  // (#1179) — unknown names, or the byte budget ran out. Surface it as a non-blocking
  // toast: the turn proceeds and the dropped tools remain reachable via `tool_search`,
  // so this is informational, but silence would leave a misconfigured phenotype
  // looking like it worked.
  void ipc.onPhenotypePreheatDropped((e) => {
    usePreheatNoticeStore.getState().show(e);
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
  // #561 — the active workspace's git HEAD changed on disk (terminal checkout,
  // rebase, or the assistant's own bash switching branches). Patch the cached
  // `gitBranch` for every session sharing `path` in place — no reload, no
  // remount. The backend's GitHeadWatcher debounces; this just applies the
  // resolved `SessionWorkspace` payload.
  void ipc.onWorkspaceBranchChanged((ws) => {
    useSessionWorkspaceStore.getState().applyBranchChanged(ws);
  });
  // No UI for intention signals yet (NeuroForge, M8) — observe only.
  void ipc.onIntention((e) => {
    console.debug("[signal:intention]", e.sessionId, e.goal);
  });
  // The backend regenerated a session's title as an LLM summary after its first
  // turn (#671 item 2b). It is already persisted server-side, so patch the cached
  // title in place — no refetch, no write-back.
  void ipc.onSessionTitleUpdated((e) => {
    useChatStore.getState().patchSessionTitle(e.sessionId, e.title);
  });
  // Self-update download progress (#566): feed the shared update store so the global
  // update bar (#565) and Settings → About render a real progress bar. `bigint` on the
  // wire -> `number` here (byte counts are within Number's safe range). The terminal
  // event clears progress, flipping the UI to the indeterminate "finalizing" state
  // until the backend relaunches.
  void ipc.onUpdateProgress((e) => {
    useUpdateStore.getState().setProgress({
      downloaded: Number(e.downloaded),
      total: e.total == null ? null : Number(e.total),
    });
  });
  void ipc.onUpdateDownloadFinished(() => {
    useUpdateStore.getState().setProgress(null);
  });
  // Goal mode (#717, RFC 0020 §7): each iteration boundary / set / pause / resume /
  // complete emits the bare goal, so the status panel re-renders without polling.
  void ipc.onGoalUpdated((goal) => {
    useGoalStore.getState().applyGoalUpdated(goal);
  });
  // `goal_clear` emits `goal:cleared` (bare sessionId), not a terminal update —
  // drop the session's goal so the panel unmounts.
  void ipc.onGoalCleared((sessionId) => {
    useGoalStore.getState().clear(sessionId);
  });
}
