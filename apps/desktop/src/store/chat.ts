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
  TurnDoneEvent,
  TurnErrorEvent,
  ToolApprovalRequestEvent,
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
  status: "running" | "awaiting-approval" | "done" | "error";
  /** Set when status is "awaiting-approval" — the call's trust level. */
  safety?: ApprovalSafety;
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
  /** assistant messageId -> tool steps emitted during that turn (in order). */
  toolStepsByMessage: Record<string, ToolStep[]>;
  /** Frontend-only custom titles (Session has no title field in the contract). */
  sessionTitles: Record<string, string>;
  /** Set when bootstrap() fails so the UI can show a clear error instead of a
   *  silently broken input bar. */
  bootstrapError: string | null;

  bootstrap: () => Promise<void>;
  selectSession: (sessionId: string) => Promise<void>;
  newSession: (goal?: string) => Promise<void>;
  send: (content: string) => Promise<void>;
  cancelActiveTurn: () => Promise<void>;
  setSessionTitle: (sessionId: string, title: string) => void;

  // Driven by backend events (wired once in lib/events.ts).
  applyToken: (e: TokenEvent) => void;
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
  toolStepsByMessage: {},
  sessionTitles: loadTitles(),
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

  send: async (content) => {
    const sessionId = get().activeSessionId;
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
      set((s) => ({
        messagesBySession: {
          ...s.messagesBySession,
          [sessionId]: [
            ...(s.messagesBySession[sessionId] ?? []).filter(
              (m) => m.id !== tempId,
            ),
            systemMessage(sessionId, `Failed to send: ${String(err)}`),
          ],
        },
      }));
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

  cancelActiveTurn: async () => {
    const sessionId = get().activeSessionId;
    if (!sessionId || !get().streamingBySession[sessionId]) return;
    await ipc.cancelTurn(sessionId);
    set((s) => {
      const { [sessionId]: _, ...rest } = s.streamingBySession;
      return { streamingBySession: rest };
    });
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
        streamingBySession: {
          ...s.streamingBySession,
          [e.sessionId]: e.messageId,
        },
      };
    });
  },

  finishTurn: (e) => {
    set((s) => {
      const { [e.sessionId]: _, ...rest } = s.streamingBySession;
      return {
        streamingBySession: rest,
        sessions: s.sessions.map((sess) =>
          sess.id === e.sessionId ? { ...sess, updatedAt: Date.now() } : sess,
        ),
      };
    });
  },

  failTurn: (e) => {
    set((s) => {
      const { [e.sessionId]: _, ...rest } = s.streamingBySession;
      return {
        streamingBySession: rest,
        messagesBySession: {
          ...s.messagesBySession,
          [e.sessionId]: [
            ...(s.messagesBySession[e.sessionId] ?? []),
            systemMessage(e.sessionId, e.message),
          ],
        },
      };
    });
  },

  applyToolCall: (e) => {
    set((s) => {
      const steps = s.toolStepsByMessage[e.messageId] ?? [];
      // Idempotent: ignore a duplicate call event for the same callId.
      if (steps.some((step) => step.callId === e.callId)) return s;
      const step: ToolStep = {
        callId: e.callId,
        tool: e.tool,
        args: e.args,
        status: "running",
        startedAt: Date.now(),
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
        streamingBySession: {
          ...s.streamingBySession,
          [e.sessionId]: e.messageId,
        },
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
}));
