// In-browser mock backend. Fulfils the FfIpc contract with canned data and a faked
// token stream, so the frontend runs standalone via `VITE_FF_MOCK=1 pnpm dev`.

import type {
  Message,
  Session,
  TokenEvent,
  TurnDoneEvent,
  TurnErrorEvent,
  IntentionSignal,
} from "../bindings";
import type { FfIpc, Unlisten } from "./ipc";

type Listener<T> = (e: T) => void;

const uid = () => crypto.randomUUID();
const now = () => Date.now();

const MOCK_REPLY =
  "This is a mocked assistant reply streamed token by token so the UI can be " +
  "built without a running backend.";

export class MockIpc implements FfIpc {
  private sessions = new Map<string, Session>();
  private messages = new Map<string, Message[]>();

  private tokenListeners = new Set<Listener<TokenEvent>>();
  private doneListeners = new Set<Listener<TurnDoneEvent>>();
  private errorListeners = new Set<Listener<TurnErrorEvent>>();
  private intentionListeners = new Set<Listener<IntentionSignal>>();

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
    return [...this.sessions.values()].sort((a, b) => b.updatedAt - a.updatedAt);
  }

  async getMessages(sessionId: string): Promise<Message[]> {
    return [...(this.messages.get(sessionId) ?? [])];
  }

  async sendMessage(sessionId: string, content: string): Promise<string> {
    const user = this.append(sessionId, "user", content);
    this.streamAssistant(sessionId);
    return user.id;
  }

  async cancelTurn(_sessionId: string): Promise<void> {
    // Mock turns are short; nothing to cancel.
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

  // --- internals ---

  private append(sessionId: string, role: Message["role"], content: string): Message {
    const msg: Message = { id: uid(), sessionId, role, content, createdAt: now() };
    this.messages.get(sessionId)?.push(msg);
    const s = this.sessions.get(sessionId);
    if (s) s.updatedAt = msg.createdAt;
    return msg;
  }

  private streamAssistant(sessionId: string): void {
    const assistant = this.append(sessionId, "assistant", "");
    const words = MOCK_REPLY.split(" ");
    let i = 0;
    const timer = setInterval(() => {
      if (i >= words.length) {
        clearInterval(timer);
        this.emit(this.doneListeners, { sessionId, messageId: assistant.id });
        return;
      }
      const delta = (i === 0 ? "" : " ") + words[i];
      assistant.content += delta;
      i += 1;
      this.emit(this.tokenListeners, { sessionId, messageId: assistant.id, delta });
    }, 40);
  }

  private subscribe<T>(set: Set<Listener<T>>, cb: Listener<T>): Promise<Unlisten> {
    set.add(cb);
    return Promise.resolve(() => set.delete(cb));
  }

  private emit<T>(set: Set<Listener<T>>, payload: T): void {
    set.forEach((cb) => cb(payload));
  }
}
