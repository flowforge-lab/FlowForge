// Single source of truth for chat state. All backend access goes through `ipc.*`
// (never `invoke` directly) so the store works identically against the mock and
// the real Rust backend.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import { autoTitle } from "@/lib/auto-title";
import type {
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

// ── Title helpers ────────────────────────────────────────────────────────────

const TITLE_STORAGE_KEY = "ff-session-titles";

function loadTitles(): Record<string, string> {
  try {
    return JSON.parse(
      localStorage.getItem(TITLE_STORAGE_KEY) ?? "{}",
    ) as Record<string, string>;
  } catch {
    return {};
  }
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
  /** Frontend-only custom titles (Session has no title field in the contract). */
  sessionTitles: Record<string, string>;
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

  bootstrap: () => Promise<void>;
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
  send: (content: string, sessionId?: string) => Promise<void>;
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
  sessionTitles: loadTitles(),
  sessionApprovedBySession: {},
  alwaysApproved: new Set<string>(),
  bootstrapError: null,

  bootstrap: async () => {
    try {
      let sessions = await ipc.listSessions();
      if (sessions.length === 0) {
        await ipc.createSession();
        sessions = await ipc.listSessions();
      }
      set({ sessions, bootstrapError: null });

      // One-time migration: lift legacy localStorage titles to the backend for
      // any session the server hasn't titled, so labels become server-truth.
      const legacy = get().sessionTitles;
      const toMigrate = sessions.filter((s) => !s.title && legacy[s.id]);
      if (toMigrate.length > 0) {
        await Promise.all(
          toMigrate.map((s) =>
            ipc.renameSession(s.id, legacy[s.id]).catch(() => {}),
          ),
        );
        set((st) => ({
          sessions: st.sessions.map((s) =>
            !s.title && legacy[s.id] ? { ...s, title: legacy[s.id] } : s,
          ),
        }));
      }

      // Hydrate the always-approved mirror so badges render correctly from the
      // first turn (the gate itself stays backend-owned).
      void get().loadAlwaysApproved();

      const first = sessions[0];
      if (first) await get().selectSession(first.id);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error("[FlowForge] bootstrap failed:", msg);
      set({ bootstrapError: msg });
    }
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

  send: async (content, sessionId = get().activeSessionId ?? undefined) => {
    if (!sessionId || get().streamingBySession[sessionId]) return;

    // Auto-title: the backend seeds the title from the first user message; mirror
    // it optimistically here so the sidebar updates without waiting for a refetch.
    const priorMessages = get().messagesBySession[sessionId] ?? [];
    const hasUserMessage = priorMessages.some((m) => m.role === "user");
    const existing = get().sessions.find((x) => x.id === sessionId);
    const hasTitle =
      Boolean(existing?.title) || Boolean(get().sessionTitles[sessionId]);
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
      createdAt: Date.now(),
    };
    set((s) => ({
      messagesBySession: {
        ...s.messagesBySession,
        [sessionId]: [...(s.messagesBySession[sessionId] ?? []), optimistic],
      },
      turnStartBySession: { ...s.turnStartBySession, [sessionId]: Date.now() },
    }));

    try {
      const userMessageId = await ipc.sendMessage(sessionId, content);
      set((s) => ({
        messagesBySession: {
          ...s.messagesBySession,
          [sessionId]: (s.messagesBySession[sessionId] ?? []).map((m) =>
            m.id === tempId ? { ...m, id: userMessageId } : m,
          ),
        },
        sessions: s.sessions.map((sess) =>
          sess.id === sessionId ? { ...sess, updatedAt: Date.now() } : sess,
        ),
      }));
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
    set((s) => clearSessionTurnTiming(s, sessionId));
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
    }));
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
              ? { ...step, status: "awaiting-answer", question: e.question }
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
