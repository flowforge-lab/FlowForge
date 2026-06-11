// In-browser mock backend. Fulfils the FfIpc contract with canned data and a faked
// token stream, so the frontend runs standalone via `VITE_FF_MOCK=1 pnpm dev`.
//
// Set `VITE_FF_MOCK_SLOW=1` to stream at 300 ms/word instead of 40 ms/word —
// gives you enough time to hit Stop and verify the cancelTurn path.

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
import type { FfIpc, Unlisten } from "./ipc";

type Listener<T> = (e: T) => void;

const uid = () => crypto.randomUUID();
const now = () => Date.now();

// 300 ms/word in slow mode — long enough to see the Stop button and click it.
const TOKEN_INTERVAL_MS = import.meta.env.VITE_FF_MOCK_SLOW === "1" ? 300 : 40;

const MOCK_REPLY =
  "This is a mocked assistant reply streamed token by token so the UI can be " +
  "built without a running backend.";

interface ActiveTurn {
  // All pending interval/timeout handles for this turn, cleared on cancel.
  timers: ReturnType<typeof setInterval>[];
  messageId: string;
  // callIds emitted but not yet resolved. On cancel these are backfilled with a
  // "[cancelled]" result, mirroring the real backend's tool-result backfill so a
  // cancelled step never spins forever in the UI.
  pendingToolCalls: string[];
}

const uidShort = () => crypto.randomUUID().slice(0, 8);

export class MockIpc implements FfIpc {
  private sessions = new Map<string, Session>();
  private messages = new Map<string, Message[]>();
  // One active timer per session so cancelTurn can stop it.
  private activeTimers = new Map<string, ActiveTurn>();

  private tokenListeners = new Set<Listener<TokenEvent>>();
  private doneListeners = new Set<Listener<TurnDoneEvent>>();
  private errorListeners = new Set<Listener<TurnErrorEvent>>();
  private intentionListeners = new Set<Listener<IntentionSignal>>();
  private toolCallListeners = new Set<Listener<ToolCallEvent>>();
  private toolResultListeners = new Set<Listener<ToolResultEvent>>();

  async createSession(goal?: string): Promise<Session> {
    const ts = now();
    const session: Session = {
      id: uid(),
      goal: goal ?? null,
      status: "active",
      createdAt: ts,
      updatedAt: ts,
    };
    this.sessions.set(session.id, session);
    this.messages.set(session.id, []);
    if (goal) {
      this.emit(this.intentionListeners, { sessionId: session.id, goal });
    }
    return session;
  }

  async listSessions(): Promise<Session[]> {
    return [...this.sessions.values()].sort(
      (a, b) => b.updatedAt - a.updatedAt,
    );
  }

  async getMessages(sessionId: string): Promise<Message[]> {
    return [...(this.messages.get(sessionId) ?? [])];
  }

  async sendMessage(sessionId: string, content: string): Promise<string> {
    const user = this.append(sessionId, "user", content);
    this.streamAssistant(sessionId);
    return user.id;
  }

  async cancelTurn(sessionId: string): Promise<void> {
    const active = this.activeTimers.get(sessionId);
    if (!active) return;
    active.timers.forEach((t) => clearInterval(t));
    this.activeTimers.delete(sessionId);
    // Any tool call still in flight needs a matching result, or its step would
    // spin forever — the real backend backfills "[cancelled]" the same way.
    for (const callId of active.pendingToolCalls) {
      this.emit(this.toolResultListeners, {
        sessionId,
        messageId: active.messageId,
        callId,
        success: false,
        result: "[cancelled]",
      });
    }
    // Emit done with whatever partial content was accumulated — mirrors what
    // the real backend does when a CancellationToken fires.
    this.emit(this.doneListeners, { sessionId, messageId: active.messageId });
  }

  onToken(cb: Listener<TokenEvent>): Promise<Unlisten> {
    return this.subscribe(this.tokenListeners, cb);
  }
  onTurnDone(cb: Listener<TurnDoneEvent>): Promise<Unlisten> {
    return this.subscribe(this.doneListeners, cb);
  }
  onTurnError(cb: Listener<TurnErrorEvent>): Promise<Unlisten> {
    return this.subscribe(this.errorListeners, cb);
  }
  onIntention(cb: Listener<IntentionSignal>): Promise<Unlisten> {
    return this.subscribe(this.intentionListeners, cb);
  }
  onToolCall(cb: Listener<ToolCallEvent>): Promise<Unlisten> {
    return this.subscribe(this.toolCallListeners, cb);
  }
  onToolResult(cb: Listener<ToolResultEvent>): Promise<Unlisten> {
    return this.subscribe(this.toolResultListeners, cb);
  }

  // --- internals ---

  private append(
    sessionId: string,
    role: Message["role"],
    content: string,
  ): Message {
    const msg: Message = {
      id: uid(),
      sessionId,
      role,
      content,
      createdAt: now(),
    };
    this.messages.get(sessionId)?.push(msg);
    const s = this.sessions.get(sessionId);
    if (s) s.updatedAt = msg.createdAt;
    return msg;
  }

  private streamAssistant(sessionId: string): void {
    const assistant = this.append(sessionId, "assistant", "");
    const turn: ActiveTurn = {
      timers: [],
      messageId: assistant.id,
      pendingToolCalls: [],
    };
    this.activeTimers.set(sessionId, turn);

    // Simulate one read-only tool call before the text reply so the UIs tool
    // step rendering (running -> done) is exercised under VITE_FF_MOCK=1.
    const callId = uidShort();
    turn.pendingToolCalls.push(callId);
    this.emit(this.toolCallListeners, {
      sessionId,
      messageId: assistant.id,
      callId,
      tool: "view",
      args: { path: "README.md" },
    });

    const resultTimer = setInterval(() => {
      clearInterval(resultTimer);
      turn.pendingToolCalls = turn.pendingToolCalls.filter(
        (id) => id !== callId,
      );
      this.emit(this.toolResultListeners, {
        sessionId,
        messageId: assistant.id,
        callId,
        success: true,
        result:
          "# FlowForge\n\n(mocked file contents returned by the view tool)",
      });
      this.streamWords(sessionId, turn);
    }, TOKEN_INTERVAL_MS * 4);
    turn.timers.push(resultTimer);
  }

  private streamWords(sessionId: string, turn: ActiveTurn): void {
    const stored = this.messages
      .get(sessionId)
      ?.find((m) => m.id === turn.messageId);
    const words = MOCK_REPLY.split(" ");
    let i = 0;
    const timer = setInterval(() => {
      if (i >= words.length) {
        clearInterval(timer);
        this.activeTimers.delete(sessionId);
        this.emit(this.doneListeners, {
          sessionId,
          messageId: turn.messageId,
        });
        return;
      }
      const delta = (i === 0 ? "" : " ") + words[i];
      i += 1;
      if (stored) stored.content += delta;
      this.emit(this.tokenListeners, {
        sessionId,
        messageId: turn.messageId,
        delta,
      });
    }, TOKEN_INTERVAL_MS);
    turn.timers.push(timer);
  }

  private subscribe<T>(
    set: Set<Listener<T>>,
    cb: Listener<T>,
  ): Promise<Unlisten> {
    set.add(cb);
    return Promise.resolve(() => set.delete(cb));
  }

  private emit<T>(set: Set<Listener<T>>, payload: T): void {
    set.forEach((cb) => cb(payload));
  }
}
