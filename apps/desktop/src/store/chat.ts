// Single source of truth for chat state. All backend access goes through `ipc.*`
// (never `invoke` directly) so the store works identically against the mock and
// the real Rust backend.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import { autoTitle } from "@/lib/auto-title";
import { isResumableStopNotice } from "@/store/capped-turn";
import type {
  Attachment,
  ApprovalSafety,
  Message,
  Session,
  TokenEvent,
  ReasoningEvent,
  TurnDoneEvent,
  TurnErrorEvent,
  ToolApprovalRequestEvent,
  ToolAskRequestEvent,
  ToolCallEvent,
  ToolResultEvent,
} from "@/bindings";

// A single tool invocation within an assistant turn, tracked by the UI.
// `args` is the raw decoded JSON value from the backend; `result` is the tools
// textual output once it finishes.
export interface ToolStep {
  callId: string;
  tool: string;
  args: unknown;
  status:
    | "running"
    | "awaiting-approval"
    | "awaiting-answer"
    | "done"
    | "error";
  /** Set when status is "awaiting-approval" — the call's trust level. */
  safety?: ApprovalSafety;
  /** Set when status is "awaiting-answer" — the `ask_user` question (#44). */
  question?: string;
  /** Set when status is "awaiting-answer" — the model requested a secret (#562):
   *  render a masked single-line field; the answer is redacted at rest by the
   *  backend. Never carries the entered value — that is passed straight to the
   *  turn via `respondAsk` and never stashed on the step. */
  secret?: boolean;
  /** Approved via "Allow this session" (#229) — renders the amber "session" badge.
   *  Set on the resolving step and pre-set on later auto-approved calls of the
   *  same tool (the backend short-circuits those with no approval event). */
  sessionApproved?: boolean;
  /** Approved via "Always allow" (#229) — renders the emerald "always" badge. */
  alwaysApproved?: boolean;
  result?: string;
  /** Wall-clock epoch ms when the tool:call arrived. Frontend-set — the backend
   *  contract carries no timing (Issue #17); used only to derive a turn's total
   *  duration for the StepGroup header. */
  startedAt?: number;
  /** Wall-clock epoch ms when the tool:result arrived. */
  finishedAt?: number;
}

// ── Store types ──────────────────────────────────────────────────────────────

interface ChatState {
  sessions: Session[];
  activeSessionId: string | null;
  /** Transcript per session. Only sessions we've opened are populated. */
  messagesBySession: Record<string, Message[]>;
  /** sessionId -> assistant messageId currently streaming in that session. */
  streamingBySession: Record<string, string>;
  /** sessionId -> wall-clock ms when the user sent the in-flight turn (#180). */
  turnStartBySession: Record<string, number>;
  /** assistant messageId -> wall-clock turn start (from send or first stream). */
  turnStartByMessage: Record<string, number>;
  /** assistant messageId -> tool steps emitted during that turn (in order). */
  toolStepsByMessage: Record<string, ToolStep[]>;
  /** assistant messageId -> accumulated reasoning text (not persisted on Message). */
  reasoningByMessage: Record<string, string>;
  /** sessionId -> estimated context token count from the last completed turn
   *  (#244 R6). Populated from TurnDoneEvent.tokenCount; drives a context-usage
   *  indicator. Undefined until the first turn completes with an estimate. */
  contextTokensBySession: Record<string, number>;
  /** sessionId -> effective compaction budget (the denominator) from the last
   *  completed turn (#598). The model-derived `context_window(model) * 0.8` the
   *  loop actually compacts against, forwarded on TurnDoneEvent.budgetTokens.
   *  Pairs with `contextTokensBySession` to render a usage ratio; undefined until a
   *  turn reports one (so the gauge falls back to count-only). */
  contextBudgetBySession: Record<string, number>;
  /** sessionId -> the last turn ended without a usable answer the user can resume
   *  with one click — the agent loop hit the tool-call cap / stalled (`[stopped: …]`),
   *  or the user pressed Stop (a bare `[stopped]`, #636), so a one-click "Continue"
   *  affordance is offered (#513/#636). Set in `finishTurn` when the done turn
   *  streamed no content and the refetched final message is a resumable stop notice;
   *  cleared the moment the next turn starts (send / edit / cancel). */
  resumableBySession: Record<string, boolean>;
  /** FE mirror of the backend's "Allow this session" sets, keyed by sessionId
   *  (#229). Drives the "session" badge on auto-approved follow-up calls; the
   *  backend stays the source of truth for the gate itself. */
  sessionApprovedBySession: Record<string, Set<string>>;
  /** FE mirror of the backend's persistent "Always allow" set (#229). Loaded at
   *  bootstrap; drives the "always" badge and the Settings revocation list. */
  alwaysApproved: Set<string>;
  /** Set when bootstrap() fails so the UI can show a clear error instead of a
   *  silently broken input bar. */
  bootstrapError: string | null;
  /** Ids of sessions created but not yet committed to one round of conversation
   *  (#671 item 2). A fresh `＋` session lives here as an in-memory draft: it stays
   *  in `sessions` (so its pane/transcript resolve) but is hidden from the sidebar
   *  list until the first user message persists it. Promoted (removed here) on the
   *  first successful `send`. The backend mirrors this by deferring its DB INSERT to
   *  the first message, so an untouched draft leaves no row. */
  draftSessionIds: Set<string>;

  bootstrap: () => Promise<void>;
  /** Re-pull the session list from the backend (e.g. after a scheduled task fires
   *  and creates a session out of band, #543), so it appears in the sidebar and the
   *  ↗ open-session jump can resolve it. Replaces the list with backend truth. */
  refreshSessions: () => Promise<void>;
  selectSession: (sessionId: string) => Promise<void>;
  /** Pull a session's history into messagesBySession WITHOUT changing the active
   *  session. Used by split panes, which each show an independent session while
   *  only one is the global "active" (sidebar highlight, keyboard targets). */
  loadSession: (sessionId: string) => Promise<void>;
  /** Pure setter for the globally-active session (sidebar highlight + shortcuts).
   *  Does not pull history — pair with loadSession when opening a fresh session. */
  setActiveSession: (sessionId: string) => void;
  newSession: (goal?: string) => Promise<void>;
  /** Fork `sourceId` into a new session (clones its transcript via IPC) and pull
   *  the cloned history into the store WITHOUT changing the active session — the
   *  caller (a split pane) decides where it lands. Resolves with the new id, or
   *  null on failure. */
  forkSession: (sourceId: string) => Promise<string | null>;
  /** Permanently delete `sessionId` (backend + store). Optimistic: removes it and
   *  reassigns the active session immediately, rolling back if the backend rejects.
   *  Purges its sidebar prefs and reconciles split panes. If it was the last
   *  session, a fresh blank one is created so the app is never session-less.
   *  Resolves true on success, false on failure / unknown id. */
  deleteSession: (sessionId: string) => Promise<boolean>;
  /** Send into `sessionId`, defaulting to the active session. Panes pass their own
   *  session so a background pane can send/stream independently. */
  send: (
    content: string,
    sessionId?: string,
    attachments?: Attachment[],
  ) => Promise<void>;
  /** Edit a prior user message in place and re-run from it (#463). Optimistically
   *  replaces the message and truncates everything after it, then calls the backend
   *  `editMessage`, which cancels any in-flight turn and re-runs over the existing
   *  `turn:*` events. Restores the transcript on failure. */
  editMessage: (
    sessionId: string,
    messageId: string,
    content: string,
    attachments?: Attachment[],
  ) => Promise<void>;
  /** Cancel the streaming turn in `sessionId` (session-scoped). */
  cancelTurn: (sessionId: string) => Promise<void>;
  cancelActiveTurn: () => Promise<void>;
  setSessionTitle: (sessionId: string, title: string) => void;

  // Driven by backend events (wired once in lib/events.ts).
  applyToken: (e: TokenEvent) => void;
  applyReasoning: (e: ReasoningEvent) => void;
  finishTurn: (e: TurnDoneEvent) => void;
  failTurn: (e: TurnErrorEvent) => void;
  applyToolCall: (e: ToolCallEvent) => void;
  applyToolResult: (e: ToolResultEvent) => void;
  applyApprovalRequest: (e: ToolApprovalRequestEvent) => void;
  respondApproval: (
    sessionId: string,
    messageId: string,
    callId: string,
    approved: boolean,
  ) => Promise<void>;
  applyAskRequest: (e: ToolAskRequestEvent) => void;
  respondAsk: (
    sessionId: string,
    messageId: string,
    callId: string,
    answer: string,
  ) => Promise<void>;

  // Four-option approval (#229). Each pairs the persistent-set write with the
  // existing approve round-trip so the in-flight call is also released.
  /** "Allow this session": approve `tool` for `sessionId` and release this call. */
  approveSession: (
    sessionId: string,
    messageId: string,
    callId: string,
    tool: string,
  ) => Promise<void>;
  /** "Always allow": persist `tool` and release this call. */
  approveAlways: (
    sessionId: string,
    messageId: string,
    callId: string,
    tool: string,
  ) => Promise<void>;
  /** Load the persistent always-approved set into the mirror (bootstrap + the
   *  Settings panel on mount). Resolves `true` on success, `false` if the IPC
   *  fetch failed — the Settings panel uses this to distinguish a load error
   *  from a genuinely empty list. */
  loadAlwaysApproved: () => Promise<boolean>;
  /** Revoke `tool` from the always-approved set (Settings). Optimistic. */
  revokeAlways: (tool: string) => Promise<void>;
}

/** Replace one step (by callId) within a message's step list, merging `patch`.
 *  No-op when the message has no steps yet. */
function patchStep(
  s: Pick<ChatState, "toolStepsByMessage">,
  messageId: string,
  callId: string,
  patch: Partial<ToolStep>,
): Pick<ChatState, "toolStepsByMessage"> | null {
  const steps = s.toolStepsByMessage[messageId];
  if (!steps) return null;
  return {
    toolStepsByMessage: {
      ...s.toolStepsByMessage,
      [messageId]: steps.map((step) =>
        step.callId === callId ? { ...step, ...patch } : step,
      ),
    },
  };
}

/** Wall-clock turn start when streaming begins (#180). */
function streamingPatch(
  s: Pick<
    ChatState,
    "streamingBySession" | "turnStartBySession" | "turnStartByMessage"
  >,
  sessionId: string,
  messageId: string,
) {
  const wall = s.turnStartBySession[sessionId] ?? Date.now();
  return {
    streamingBySession: { ...s.streamingBySession, [sessionId]: messageId },
    turnStartByMessage: {
      ...s.turnStartByMessage,
      [messageId]: s.turnStartByMessage[messageId] ?? wall,
    },
  };
}

function clearSessionTurnTiming(
  s: Pick<ChatState, "streamingBySession" | "turnStartBySession">,
  sessionId: string,
) {
  const { [sessionId]: _stream, ...streamingBySession } = s.streamingBySession;
  const { [sessionId]: _start, ...turnStartBySession } = s.turnStartBySession;
  return { streamingBySession, turnStartBySession };
}

/** Drop a session's "offer Continue" flag (#513/#636) — a new turn is starting, so
 *  any prior capped/stopped state is stale. */
function clearResumable(
  s: Pick<ChatState, "resumableBySession">,
  sessionId: string,
) {
  if (!(sessionId in s.resumableBySession)) return null;
  const { [sessionId]: _resumable, ...resumableBySession } =
    s.resumableBySession;
  return { resumableBySession };
}

const systemMessage = (sessionId: string, content: string): Message => ({
  id: crypto.randomUUID(),
  sessionId,
  role: "system",
  content,
  createdAt: Date.now(),
});

export const useChatStore = create<ChatState>((set, get) => ({
  sessions: [],
  activeSessionId: null,
  messagesBySession: {},
  streamingBySession: {},
  turnStartBySession: {},
  turnStartByMessage: {},
  toolStepsByMessage: {},
  reasoningByMessage: {},
  contextTokensBySession: {},
  contextBudgetBySession: {},
  resumableBySession: {},
  sessionApprovedBySession: {},
  alwaysApproved: new Set<string>(),
  bootstrapError: null,
  draftSessionIds: new Set<string>(),

  bootstrap: async () => {
    try {
      const sessions = await ipc.listSessions();
      set({ sessions, bootstrapError: null });

      // Hydrate the always-approved mirror so badges render correctly from the
      // first turn (the gate itself stays backend-owned).
      void get().loadAlwaysApproved();

      const first = sessions[0];
      if (first) {
        await get().selectSession(first.id);
      } else {
        // No persisted sessions yet — open a fresh draft (#671 item 2) so the app is
        // never session-less. It isn't written until the first message, so it stays
        // out of the sidebar but is the active session in the pane, ready to type.
        await get().newSession();
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error("[FlowForge] bootstrap failed:", msg);
      set({ bootstrapError: msg });
    }
  },

  refreshSessions: async () => {
    const fetched = await ipc.listSessions();
    set((s) => {
      // Backend truth won't include not-yet-persisted drafts (#671 item 2), so
      // carry any still-draft session over rather than letting a refresh drop it
      // from `sessions` (which would let a split pane's leaf reconcile away).
      const fetchedIds = new Set(fetched.map((f) => f.id));
      const keptDrafts = s.sessions.filter(
        (x) => s.draftSessionIds.has(x.id) && !fetchedIds.has(x.id),
      );
      return { sessions: [...keptDrafts, ...fetched] };
    });
  },

  selectSession: async (sessionId) => {
    set({ activeSessionId: sessionId });
    await get().loadSession(sessionId);
  },

  setActiveSession: (sessionId) => {
    set({ activeSessionId: sessionId });
  },

  loadSession: async (sessionId) => {
    // Always re-pull history so a switch shows the backend's truth, including
    // turns that streamed while the session was in the background.
    const messages = await ipc.getMessages(sessionId);
    set((s) => {
      const streamingId = s.streamingBySession[sessionId];
      const local = s.messagesBySession[sessionId] ?? [];
      // Don't let a fetch race clobber a partially streamed assistant message:
      // keep the local (longer) copy of the actively streaming bubble.
      const merged = messages.map((m) => {
        if (m.id !== streamingId) return m;
        const partial = local.find((lm) => lm.id === streamingId);
        return partial && partial.content.length > m.content.length
          ? partial
          : m;
      });
      const messagesBySession = {
        ...s.messagesBySession,
        [sessionId]: merged,
      };
      // Garbage-collect tool steps for messages that no longer exist in any
      // loaded session (e.g. ids replaced on history re-pull), keeping the
      // actively streaming turns. Without this the map only ever grows.
      const liveIds = new Set<string>(Object.values(s.streamingBySession));
      for (const msgs of Object.values(messagesBySession)) {
        for (const m of msgs) liveIds.add(m.id);
      }
      const toolStepsByMessage = Object.fromEntries(
        Object.entries(s.toolStepsByMessage).filter(([id]) => liveIds.has(id)),
      );
      return { messagesBySession, toolStepsByMessage };
    });
  },

  newSession: async (goal) => {
    const session = await ipc.createSession(goal);
    set((s) => ({
      sessions: [session, ...s.sessions],
      messagesBySession: { ...s.messagesBySession, [session.id]: [] },
      // Fresh, empty session: hold it as a draft (#671 item 2) so it doesn't clutter
      // the sidebar until the first message persists it. It stays active in its pane.
      draftSessionIds: new Set(s.draftSessionIds).add(session.id),
    }));
    await get().selectSession(session.id);
  },

  forkSession: async (sourceId) => {
    try {
      const session = await ipc.forkSession(sourceId);
      set((s) => ({
        sessions: [session, ...s.sessions],
        messagesBySession: { ...s.messagesBySession, [session.id]: [] },
      }));
      // Pull the cloned transcript without stealing focus from the source pane.
      await get().loadSession(session.id);
      return session.id;
    } catch {
      return null;
    }
  },

  deleteSession: async (sessionId) => {
    const prev = get();
    if (!prev.sessions.some((s) => s.id === sessionId)) return false;

    const remaining = prev.sessions.filter((s) => s.id !== sessionId);
    const wasActive = prev.activeSessionId === sessionId;
    const nextActive = wasActive
      ? (remaining[0]?.id ?? null)
      : prev.activeSessionId;

    // Optimistic removal from the store.
    set((s) => {
      const { [sessionId]: _msgs, ...messagesBySession } = s.messagesBySession;
      const { [sessionId]: _stream, ...streamingBySession } =
        s.streamingBySession;
      return {
        sessions: remaining,
        messagesBySession,
        streamingBySession,
        activeSessionId: nextActive,
      };
    });

    try {
      await ipc.deleteSession(sessionId);
    } catch {
      // Backend rejected -> restore the pre-delete snapshot.
      set({
        sessions: prev.sessions,
        messagesBySession: prev.messagesBySession,
        streamingBySession: prev.streamingBySession,
        activeSessionId: prev.activeSessionId,
      });
      return false;
    }

    // Drop any lingering sidebar prefs (pin / dismiss) for the gone session.
    const { useSessionPrefsStore } = await import("@/store/session-prefs");
    useSessionPrefsStore.getState().purge(sessionId);

    // Never leave the app session-less: recreate + select a fresh blank session.
    if (remaining.length === 0) {
      await get().newSession();
    } else if (wasActive && nextActive) {
      await get().selectSession(nextActive);
    }

    // Reconcile split panes against the final live list (drops any leaf showing
    // the deleted session, preserving the rest of the layout).
    const live = get().sessions;
    const { usePanesStore } = await import("@/store/panes");
    usePanesStore.getState().init(
      live.map((s) => s.id),
      get().activeSessionId,
    );

    return true;
  },

  send: async (
    content,
    sessionId = get().activeSessionId ?? undefined,
    attachments,
  ) => {
    if (!sessionId || get().streamingBySession[sessionId]) return;

    // Auto-title: the backend seeds the title from the first user message; mirror
    // it optimistically here so the sidebar updates without waiting for a refetch.
    const priorMessages = get().messagesBySession[sessionId] ?? [];
    const hasUserMessage = priorMessages.some((m) => m.role === "user");
    const existing = get().sessions.find((x) => x.id === sessionId);
    const hasTitle = Boolean(existing?.title);
    if (!hasUserMessage && !hasTitle) {
      const title = autoTitle(content);
      set((s) => ({
        sessions: s.sessions.map((sess) =>
          sess.id === sessionId ? { ...sess, title } : sess,
        ),
      }));
    }

    // Optimistic user message; reconciled with the real id from the backend.
    const tempId = crypto.randomUUID();
    const optimistic: Message = {
      id: tempId,
      sessionId,
      role: "user",
      content,
      attachments: attachments?.length ? attachments : undefined,
      createdAt: Date.now(),
    };
    set((s) => ({
      ...clearResumable(s, sessionId),
      messagesBySession: {
        ...s.messagesBySession,
        [sessionId]: [...(s.messagesBySession[sessionId] ?? []), optimistic],
      },
      turnStartBySession: { ...s.turnStartBySession, [sessionId]: Date.now() },
    }));

    try {
      const userMessageId = await ipc.sendMessage(
        sessionId,
        content,
        attachments,
      );
      set((s) => {
        // First round landed: the backend has now persisted this session, so drop
        // its draft flag (#671 item 2) — it graduates into the sidebar list.
        let draftSessionIds = s.draftSessionIds;
        if (draftSessionIds.has(sessionId)) {
          draftSessionIds = new Set(draftSessionIds);
          draftSessionIds.delete(sessionId);
        }
        return {
          draftSessionIds,
          messagesBySession: {
            ...s.messagesBySession,
            [sessionId]: (s.messagesBySession[sessionId] ?? []).map((m) =>
              m.id === tempId ? { ...m, id: userMessageId } : m,
            ),
          },
          sessions: s.sessions.map((sess) =>
            sess.id === sessionId ? { ...sess, updatedAt: Date.now() } : sess,
          ),
        };
      });
    } catch (err) {
      set((s) => {
        const { [sessionId]: _, ...turnStartBySession } = s.turnStartBySession;
        return {
          turnStartBySession,
          messagesBySession: {
            ...s.messagesBySession,
            [sessionId]: [
              ...(s.messagesBySession[sessionId] ?? []).filter(
                (m) => m.id !== tempId,
              ),
              systemMessage(sessionId, `Failed to send: ${String(err)}`),
            ],
          },
        };
      });
    }
  },

  editMessage: async (sessionId, messageId, content, attachments) => {
    const prior = get().messagesBySession[sessionId] ?? [];
    const idx = prior.findIndex((m) => m.id === messageId);
    if (idx < 0) return;

    // Optimistic: replace the edited message in place and truncate everything
    // after it (the old response + anything following), then mark the turn as
    // in-flight so the composer shows Stop and the transcript a "thinking" row —
    // the backend cancels any running turn and re-runs over the existing events.
    const edited: Message = {
      ...prior[idx],
      content,
      attachments: attachments?.length ? attachments : undefined,
    };
    set((s) => {
      // Drop any in-flight streaming pointer: editing mid-turn truncates the
      // streaming assistant message away, and the backend cancels that turn before
      // re-running. Fresh turn timing then drives the pending/Stop indicator.
      const cleared = clearSessionTurnTiming(s, sessionId);
      const resumableCleared = clearResumable(s, sessionId);
      const messagesBySession = {
        ...s.messagesBySession,
        [sessionId]: [...prior.slice(0, idx), edited],
      };
      // GC the per-message maps for the truncated messages, mirroring the
      // history-reconcile path (see loadSession) — without this they only grow.
      const liveIds = new Set<string>(
        Object.values(cleared.streamingBySession),
      );
      for (const msgs of Object.values(messagesBySession)) {
        for (const m of msgs) liveIds.add(m.id);
      }
      const toolStepsByMessage = Object.fromEntries(
        Object.entries(s.toolStepsByMessage).filter(([id]) => liveIds.has(id)),
      );
      const turnStartByMessage = Object.fromEntries(
        Object.entries(s.turnStartByMessage).filter(([id]) => liveIds.has(id)),
      );
      return {
        ...resumableCleared,
        messagesBySession,
        toolStepsByMessage,
        turnStartByMessage,
        streamingBySession: cleared.streamingBySession,
        turnStartBySession: {
          ...cleared.turnStartBySession,
          [sessionId]: Date.now(),
        },
      };
    });

    try {
      const editedId = await ipc.editMessage(
        sessionId,
        messageId,
        content,
        attachments,
      );
      set((s) => ({
        messagesBySession: {
          ...s.messagesBySession,
          [sessionId]: (s.messagesBySession[sessionId] ?? []).map((m) =>
            m.id === messageId ? { ...m, id: editedId } : m,
          ),
        },
        sessions: s.sessions.map((sess) =>
          sess.id === sessionId ? { ...sess, updatedAt: Date.now() } : sess,
        ),
      }));
    } catch (err) {
      // The backend never mutates the transcript on a failed edit — restore the
      // snapshot and surface the error rather than leaving a truncated history.
      set((s) => {
        const { [sessionId]: _, ...turnStartBySession } = s.turnStartBySession;
        return {
          turnStartBySession,
          messagesBySession: {
            ...s.messagesBySession,
            [sessionId]: [
              ...prior,
              systemMessage(sessionId, `Failed to edit: ${String(err)}`),
            ],
          },
        };
      });
    }
  },

  setSessionTitle: (sessionId, title) => {
    // Server-truth now: persist via ipc, optimistically reflect on the session.
    void ipc.renameSession(sessionId, title).catch((err) => {
      console.error("[FlowForge] rename_session failed:", err);
    });
    set((s) => ({
      sessions: s.sessions.map((sess) =>
        sess.id === sessionId ? { ...sess, title } : sess,
      ),
    }));
  },

  cancelTurn: async (sessionId) => {
    // Cancellable as soon as the turn is in flight, not only once it streams:
    // the backend registers the cancel token at send_message time, so a turn
    // stuck in the pending window (a cold model ramping up before its first
    // token) can still be stopped. turnStartBySession marks that window.
    if (!sessionId) return;
    const s0 = get();
    if (
      !s0.streamingBySession[sessionId] &&
      !s0.turnStartBySession[sessionId]
    ) {
      return;
    }
    await ipc.cancelTurn(sessionId);
    // Clear the flag now; the backend then writes a bare `[stopped]`, and after
    // turn:done + the finishTurn refetch it is re-set so Continue reappears (#636).
    set((s) => ({
      ...clearSessionTurnTiming(s, sessionId),
      ...clearResumable(s, sessionId),
    }));
  },

  cancelActiveTurn: async () => {
    const sessionId = get().activeSessionId;
    if (sessionId) await get().cancelTurn(sessionId);
  },

  applyToken: (e) => {
    set((s) => {
      const messages = s.messagesBySession[e.sessionId] ?? [];
      const idx = messages.findIndex((m) => m.id === e.messageId);
      const next =
        idx >= 0
          ? messages.map((m, i) =>
              i === idx ? { ...m, content: m.content + e.delta } : m,
            )
          : [
              ...messages,
              {
                id: e.messageId,
                sessionId: e.sessionId,
                role: "assistant" as const,
                content: e.delta,
                createdAt: Date.now(),
              },
            ];
      return {
        messagesBySession: { ...s.messagesBySession, [e.sessionId]: next },
        ...streamingPatch(s, e.sessionId, e.messageId),
      };
    });
  },

  applyReasoning: (e) => {
    set((s) => {
      const prev = s.reasoningByMessage[e.messageId] ?? "";
      const messages = s.messagesBySession[e.sessionId] ?? [];
      const nextMessages = messages.some((m) => m.id === e.messageId)
        ? messages
        : [
            ...messages,
            {
              id: e.messageId,
              sessionId: e.sessionId,
              role: "assistant" as const,
              content: "",
              createdAt: Date.now(),
            },
          ];
      return {
        messagesBySession: {
          ...s.messagesBySession,
          [e.sessionId]: nextMessages,
        },
        streamingBySession: {
          ...s.streamingBySession,
          [e.sessionId]: e.messageId,
        },
        reasoningByMessage: {
          ...s.reasoningByMessage,
          [e.messageId]: prev + e.delta,
        },
      };
    });
  },

  finishTurn: (e) => {
    // A turn that ends with no streamed content is the structural signature of
    // the agent loop's cap/stall finalizer: it writes a `[stopped: …]` notice via
    // set_message_content, which emits no token and isn't carried on turn:done, so
    // the bubble is empty here until we re-pull history (#513). Capture that before
    // the set so a later normal turn can't mask it.
    const doneMsg = (get().messagesBySession[e.sessionId] ?? []).find(
      (m) => m.id === e.messageId,
    );
    const endedEmpty = !doneMsg || doneMsg.content.trim() === "";

    // Effective compaction budget the loop compacts against (#598). Read through a
    // local augmentation until the ts-rs binding regeneration lands the field on
    // TurnDoneEvent (backend/contract change) — drop the cast once it does.
    const budget = (e as TurnDoneEvent & { budgetTokens?: number | null })
      .budgetTokens;

    set((s) => ({
      ...clearSessionTurnTiming(s, e.sessionId),
      sessions: s.sessions.map((sess) =>
        sess.id === e.sessionId ? { ...sess, updatedAt: Date.now() } : sess,
      ),
      // Record the backend's context-usage estimate so the FE can surface it
      // (#244 R6). Only overwrite when an estimate is present — a Done without
      // a count must not clobber the prior value.
      contextTokensBySession:
        e.tokenCount != null
          ? { ...s.contextTokensBySession, [e.sessionId]: e.tokenCount }
          : s.contextTokensBySession,
      // Same guard for the ratio denominator (#598): a Done without a budget keeps
      // the prior value rather than dropping the gauge back to count-only.
      contextBudgetBySession:
        budget != null
          ? { ...s.contextBudgetBySession, [e.sessionId]: budget }
          : s.contextBudgetBySession,
    }));

    if (!endedEmpty) return;
    // Re-pull authoritative history (the stop reason + notice text live on the
    // backend, not on the transient event's empty bubble), then offer Continue if
    // the final assistant message stopped without an answer. The turn:done event
    // now carries a structured `stopReason` (#658); the reloaded message carries the
    // same field, which the classification below prefers over the marker string.
    void get()
      .loadSession(e.sessionId)
      .then(() => {
        // Bail if a newer turn superseded this one in the window between turn:done
        // and this refetch resolving (the user clicked Continue / sent meanwhile):
        // `send` already cleared the flag, and re-setting it here would resurrect a
        // stale flag that the next real-content turn early-returns past without
        // clearing — a spurious lingering button (review nit). Two guards: an
        // in-flight turn, and the done message no longer being the last assistant
        // (a newer turn that already finished).
        if (
          get().turnStartBySession[e.sessionId] ||
          get().streamingBySession[e.sessionId]
        ) {
          return;
        }
        const after = get().messagesBySession[e.sessionId] ?? [];
        const lastAssistant = [...after]
          .reverse()
          .find((m) => m.role === "assistant");
        if (
          lastAssistant &&
          lastAssistant.id === e.messageId &&
          // Prefer the structured stop reason (#658); fall back to the legacy
          // marker-string classifier for rows persisted before the column existed.
          (lastAssistant.stopReason != null ||
            isResumableStopNotice(lastAssistant.content))
        ) {
          set((s) => ({
            resumableBySession: {
              ...s.resumableBySession,
              [e.sessionId]: true,
            },
          }));
        }
      })
      .catch(() => {
        // Best-effort: a refetch failure just means no Continue affordance this
        // time — the user can still type "continue" as before.
      });
  },

  failTurn: (e) => {
    set((s) => ({
      ...clearSessionTurnTiming(s, e.sessionId),
      messagesBySession: {
        ...s.messagesBySession,
        [e.sessionId]: [
          ...(s.messagesBySession[e.sessionId] ?? []),
          systemMessage(e.sessionId, e.message),
        ],
      },
    }));
  },

  applyToolCall: (e) => {
    set((s) => {
      const steps = s.toolStepsByMessage[e.messageId] ?? [];
      // Idempotent: ignore a duplicate call event for the same callId.
      if (steps.some((step) => step.callId === e.callId)) return s;
      // Pre-set the trust badge for a tool the user already approved this session
      // or always: the backend short-circuits the gate for these, so the step
      // never enters "awaiting-approval" and would otherwise carry no badge.
      const sessionApproved =
        s.sessionApprovedBySession[e.sessionId]?.has(e.tool) ?? false;
      const alwaysApproved = s.alwaysApproved.has(e.tool);
      const step: ToolStep = {
        callId: e.callId,
        tool: e.tool,
        args: e.args,
        status: "running",
        startedAt: Date.now(),
        ...(sessionApproved ? { sessionApproved: true } : {}),
        ...(alwaysApproved ? { alwaysApproved: true } : {}),
      };
      // A tool call can precede any streamed token (the backend emits no token
      // for empty deltas), so applyToken may never create the anchoring assistant
      // message. Upsert it here and mark the session streaming, so the step
      // renders and Stop/cancel works even on a tool-first turn.
      const messages = s.messagesBySession[e.sessionId] ?? [];
      const nextMessages = messages.some((m) => m.id === e.messageId)
        ? messages
        : [
            ...messages,
            {
              id: e.messageId,
              sessionId: e.sessionId,
              role: "assistant" as const,
              content: "",
              createdAt: Date.now(),
            },
          ];
      return {
        messagesBySession: {
          ...s.messagesBySession,
          [e.sessionId]: nextMessages,
        },
        ...streamingPatch(s, e.sessionId, e.messageId),
        toolStepsByMessage: {
          ...s.toolStepsByMessage,
          [e.messageId]: [...steps, step],
        },
      };
    });
  },

  applyToolResult: (e) => {
    set((s) => {
      const steps = s.toolStepsByMessage[e.messageId] ?? [];
      const status: ToolStep["status"] = e.success ? "done" : "error";
      const known = steps.some((step) => step.callId === e.callId);
      const now = Date.now();
      // A result should always follow its call, but if it ever arrives first
      // (or after the call was lost) materialize a step so the outcome is never
      // silently dropped — the tool name/args just aren't known here.
      const nextSteps = known
        ? steps.map((step) =>
            step.callId === e.callId
              ? { ...step, status, result: e.result, finishedAt: now }
              : step,
          )
        : [
            ...steps,
            {
              callId: e.callId,
              tool: "tool",
              args: undefined,
              status,
              result: e.result,
              startedAt: now,
              finishedAt: now,
            },
          ];
      return {
        toolStepsByMessage: {
          ...s.toolStepsByMessage,
          [e.messageId]: nextSteps,
        },
      };
    });
  },

  applyApprovalRequest: (e) => {
    set((s) => {
      const steps = s.toolStepsByMessage[e.messageId];
      if (!steps) return s;
      return {
        toolStepsByMessage: {
          ...s.toolStepsByMessage,
          [e.messageId]: steps.map((step) =>
            step.callId === e.callId
              ? { ...step, status: "awaiting-approval", safety: e.safety }
              : step,
          ),
        },
      };
    });
  },

  respondApproval: async (sessionId, messageId, callId, approved) => {
    const setStatus = (status: ToolStep["status"]) =>
      set((s) => {
        const steps = s.toolStepsByMessage[messageId];
        if (!steps) return s;
        return {
          toolStepsByMessage: {
            ...s.toolStepsByMessage,
            [messageId]: steps.map((step) =>
              step.callId === callId ? { ...step, status } : step,
            ),
          },
        };
      });

    // Optimistic only on approve: flip to running so the UI doesn't sit on the
    // buttons while the round-trip + tool execution complete. On deny the tool is
    // not executed; the backend still emits a tool:result (error), which settles
    // the step — so don't flash a spinner for a call that never runs.
    if (approved) setStatus("running");
    try {
      await ipc.respondApproval(sessionId, callId, approved);
    } catch (err) {
      // IPC failed — the backend never received the response. Revert to the
      // approval gate so the user can retry. Only status changed, so the safety
      // field (and thus the buttons) re-render correctly.
      console.error("respondApproval IPC failed:", err);
      setStatus("awaiting-approval");
    }
  },

  applyAskRequest: (e) => {
    set((s) => {
      const steps = s.toolStepsByMessage[e.messageId];
      if (!steps) return s;
      return {
        toolStepsByMessage: {
          ...s.toolStepsByMessage,
          [e.messageId]: steps.map((step) =>
            step.callId === e.callId
              ? {
                  ...step,
                  status: "awaiting-answer",
                  question: e.question,
                  secret: e.secret,
                }
              : step,
          ),
        },
      };
    });
  },

  respondAsk: async (sessionId, messageId, callId, answer) => {
    const setStatus = (status: ToolStep["status"]) =>
      set((s) => {
        const steps = s.toolStepsByMessage[messageId];
        if (!steps) return s;
        return {
          toolStepsByMessage: {
            ...s.toolStepsByMessage,
            [messageId]: steps.map((step) =>
              step.callId === callId ? { ...step, status } : step,
            ),
          },
        };
      });

    // Optimistically flip to running: the answer becomes the tool result and the
    // turn resumes, so the prompt should clear immediately rather than sit on the
    // submit button while the round-trip completes. The backend then emits a
    // tool:result that settles the step.
    setStatus("running");
    try {
      await ipc.respondAsk(sessionId, callId, answer);
    } catch (err) {
      // IPC failed — the backend never received the answer. Revert to the prompt
      // so the user can retry; `question` is untouched, so it re-renders intact.
      console.error("respondAsk IPC failed:", err);
      setStatus("awaiting-answer");
    }
  },

  approveSession: async (sessionId, messageId, callId, tool) => {
    // Contract guard (#232): the allowlist never covers Dangerous calls, so a
    // session grant would be written yet the backend would keep prompting. Refuse
    // it here too (defense in depth — the UI already hides the button).
    const safety = get().toolStepsByMessage[messageId]?.find(
      (s) => s.callId === callId,
    )?.safety;
    if (safety === "dangerous") return;
    const had = get().sessionApprovedBySession[sessionId]?.has(tool) ?? false;
    // Optimistic: flip to running (the round-trip + tool exec are in flight),
    // tag the step's badge, and add the tool to the session mirror.
    set((s) => {
      const next = new Set(s.sessionApprovedBySession[sessionId] ?? []);
      next.add(tool);
      return {
        ...(patchStep(s, messageId, callId, {
          status: "running",
          sessionApproved: true,
        }) ?? {}),
        sessionApprovedBySession: {
          ...s.sessionApprovedBySession,
          [sessionId]: next,
        },
      };
    });
    try {
      await ipc.setSessionApprove(sessionId, tool);
      await ipc.respondApproval(sessionId, callId, true);
    } catch (err) {
      // Revert to the gate so the user can retry. Only drop the mirror entry if
      // this call is the one that added it (a prior success may already own it).
      console.error("approveSession failed:", err);
      set((s) => {
        const next = new Set(s.sessionApprovedBySession[sessionId] ?? []);
        if (!had) next.delete(tool);
        return {
          ...(patchStep(s, messageId, callId, {
            status: "awaiting-approval",
            sessionApproved: false,
          }) ?? {}),
          sessionApprovedBySession: {
            ...s.sessionApprovedBySession,
            [sessionId]: next,
          },
        };
      });
    }
  },

  approveAlways: async (sessionId, messageId, callId, tool) => {
    // Contract guard (#232): Dangerous calls are never allowlist-covered — see
    // `approveSession`.
    const safety = get().toolStepsByMessage[messageId]?.find(
      (s) => s.callId === callId,
    )?.safety;
    if (safety === "dangerous") return;
    const had = get().alwaysApproved.has(tool);
    set((s) => {
      const next = new Set(s.alwaysApproved);
      next.add(tool);
      return {
        ...(patchStep(s, messageId, callId, {
          status: "running",
          alwaysApproved: true,
        }) ?? {}),
        alwaysApproved: next,
      };
    });
    try {
      await ipc.setAlwaysApprove(tool);
      await ipc.respondApproval(sessionId, callId, true);
    } catch (err) {
      console.error("approveAlways failed:", err);
      set((s) => {
        const next = new Set(s.alwaysApproved);
        if (!had) next.delete(tool);
        return {
          ...(patchStep(s, messageId, callId, {
            status: "awaiting-approval",
            alwaysApproved: false,
          }) ?? {}),
          alwaysApproved: next,
        };
      });
    }
  },

  loadAlwaysApproved: async () => {
    try {
      const tools = await ipc.listAlwaysApproved();
      set({ alwaysApproved: new Set(tools) });
      return true;
    } catch (err) {
      console.error("listAlwaysApproved failed:", err);
      return false;
    }
  },

  revokeAlways: async (tool) => {
    const prev = get().alwaysApproved;
    if (!prev.has(tool)) return;
    set(() => {
      const next = new Set(prev);
      next.delete(tool);
      return { alwaysApproved: next };
    });
    try {
      await ipc.removeAlwaysApprove(tool);
    } catch (err) {
      // Restore the entry so the list reflects backend truth.
      console.error("removeAlwaysApprove failed:", err);
      set((s) => {
        const next = new Set(s.alwaysApproved);
        next.add(tool);
        return { alwaysApproved: next };
      });
    }
  },
}));
