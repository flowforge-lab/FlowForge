// Single source of truth for chat state. All backend access goes through `ipc.*`
// (never `invoke` directly) so the store works identically against the mock and
// the real Rust backend.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type {
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
  /** Set when status is "awaiting-approval" — `"write"` or `"dangerous"`. */
  safety?: string;
  result?: string;
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

function persistTitles(titles: Record<string, string>): void {
  localStorage.setItem(TITLE_STORAGE_KEY, JSON.stringify(titles));
}

// Words we always skip at the start of a prompt before extracting the title.
// Includes pronouns, articles, modals, common question stems, and proxy verbs
// that precede the actual subject ("understand how X" → skip to X).
const STOP = new Set([
  "a",
  "an",
  "the",
  "is",
  "are",
  "was",
  "were",
  "i",
  "you",
  "we",
  "they",
  "it",
  "he",
  "she",
  "my",
  "your",
  "our",
  "their",
  "in",
  "on",
  "at",
  "to",
  "for",
  "of",
  "and",
  "or",
  "but",
  "how",
  "what",
  "when",
  "where",
  "why",
  "who",
  "do",
  "does",
  "did",
  "can",
  "could",
  "would",
  "should",
  "will",
  "please",
  "help",
  "me",
  "us",
  // proxy verbs that prefix the real topic
  "understand",
  "explain",
  "tell",
  "show",
  "describe",
  "clarify",
  "give",
]);

/**
 * Derive a short, readable title from the user's first prompt.
 * All leading stop-words are skipped to land on the first meaningful word,
 * then word count scales with input length (2 → 5 words).
 */
export function autoTitle(content: string): string {
  const words = content.trim().split(/\s+/).filter(Boolean);
  if (words.length === 0) return "New session";

  // Advance past ALL leading stop-words, but always keep at least 1 word.
  let start = 0;
  while (
    start < words.length - 1 &&
    STOP.has(words[start].toLowerCase().replace(/[^a-z]/g, ""))
  ) {
    start++;
  }
  const meaningful = words.slice(start);

  // Scale word count on input length.
  const len = content.length;
  const count = Math.min(
    meaningful.length,
    len <= 25 ? 2 : len <= 50 ? 3 : len <= 100 ? 4 : 5,
  );

  const title = meaningful.slice(0, count).join(" ");
  return title.charAt(0).toUpperCase() + title.slice(1);
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

    // Auto-title: generate from the first user message if no custom title set.
    const priorMessages = get().messagesBySession[sessionId] ?? [];
    const hasUserMessage = priorMessages.some((m) => m.role === "user");
    const hasCustomTitle = Boolean(get().sessionTitles[sessionId]);
    if (!hasUserMessage && !hasCustomTitle) {
      const title = autoTitle(content);
      const next = { ...get().sessionTitles, [sessionId]: title };
      persistTitles(next);
      set({ sessionTitles: next });
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
    set((s) => {
      const next = { ...s.sessionTitles, [sessionId]: title };
      persistTitles(next);
      return { sessionTitles: next };
    });
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
      // A result should always follow its call, but if it ever arrives first
      // (or after the call was lost) materialize a step so the outcome is never
      // silently dropped — the tool name/args just aren't known here.
      const nextSteps = known
        ? steps.map((step) =>
            step.callId === e.callId
              ? { ...step, status, result: e.result }
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

  respondApproval: async (messageId, callId, approved) => {
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
      await ipc.respondApproval(callId, approved);
    } catch (err) {
      // IPC failed — the backend never received the response. Revert to the
      // approval gate so the user can retry. Only status changed, so the safety
      // field (and thus the buttons) re-render correctly.
      console.error("respondApproval IPC failed:", err);
      setStatus("awaiting-approval");
    }
  },
}));
