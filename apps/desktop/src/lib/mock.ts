// In-browser mock backend. Fulfils the FfIpc contract with canned data and a faked
// token stream, so the frontend runs standalone via `VITE_FF_MOCK=1 pnpm dev`.
//
// Set `VITE_FF_MOCK_SLOW=1` to stream at 300 ms/word instead of 40 ms/word —
// gives you enough time to hit Stop and verify the cancelTurn path.

import type {
  Message,
  ProviderConfig,
  ProviderKind,
  Session,
  TokenEvent,
  TurnDoneEvent,
  TurnErrorEvent,
  IntentionSignal,
  ToolApprovalRequestEvent,
  ToolCallEvent,
  ToolResultEvent,
  SkillInfo,
  SkillsChangedEvent,
} from "../bindings";
import type { FfIpc, Unlisten } from "./ipc";
import { autoTitle } from "./auto-title";

type Listener<T> = (e: T) => void;

const uid = () => crypto.randomUUID();
const now = () => Date.now();

// 300 ms/word in slow mode — long enough to see the Stop button and click it.
const TOKEN_INTERVAL_MS = import.meta.env.VITE_FF_MOCK_SLOW === "1" ? 300 : 40;

// A small Markdown document so the renderer's features (headings, lists,
// emphasis, inline code, a fenced + highlighted code block, a table, a link)
// are all exercised under `VITE_FF_MOCK=1`. Uses single spaces between words so
// the word-by-word fake stream reconstructs it faithfully.
const MOCK_REPLY = `### Mocked assistant reply

This is a **mocked assistant reply** streamed token by token so the UI can be built without a running backend. It now renders _Markdown_ — including inline \`code\` and the block below.

- First a short list
- Then some \`inline code\`
- And a [link](https://tauri.app)

\`\`\`ts
// fenced code block with syntax highlighting
export function greet(name: string): string {
  return \`hello, \${name}\`;
}
\`\`\`

| Feature | Status |
| --- | --- |
| Headings | done |
| Code blocks | done |`;

// Canned installed skills so the command palette skill source (#11/#16) is
// exercisable offline. `active`/`score` are placeholders — the methods below
// overlay live active state and search scores.
const MOCK_SKILLS: SkillInfo[] = [
  {
    name: "rust-debugging",
    description: "Systematic Rust debugging with bash, view, and edit.",
    version: "0.1.0",
    keywords: ["rust", "debug"],
    active: false,
    score: 0,
  },
  {
    name: "create-pr",
    description: "Open a GitHub pull request following CONTRIBUTING.",
    version: "0.2.0",
    keywords: ["git", "github", "pr"],
    active: false,
    score: 0,
  },
  {
    name: "write-tests",
    description: "Generate unit tests with coverage analysis.",
    version: "0.1.0",
    keywords: ["test", "coverage"],
    active: false,
    score: 0,
  },
];

// Mirrors `ff_skills::search_skills` scoring so the mock ranks like the backend:
// exact keyword (4) > name prefix (3) > name substring (2) > description (1).
function scoreSkill(skill: SkillInfo, q: string): number {
  const name = skill.name.toLowerCase();
  if (skill.keywords.some((k) => k.toLowerCase() === q)) return 4;
  if (name.startsWith(q)) return 3;
  if (name.includes(q)) return 2;
  if (skill.description.toLowerCase().includes(q)) return 1;
  return 0;
}

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

// Composite key mirroring the backend's `(session_id, call_id)` pending key.
const approvalKey = (sessionId: string, callId: string) =>
  `${sessionId}\u0000${callId}`;

export class MockIpc implements FfIpc {
  private sessions = new Map<string, Session>();
  private messages = new Map<string, Message[]>();
  // One active timer per session so cancelTurn can stop it.
  private activeTimers = new Map<string, ActiveTurn>();

  // Provider settings (Issue #8). Defaults mirror the backend's out-of-the-box
  // local candle-vllm config; persistence is in-memory for the mock session.
  private providerConfig: ProviderConfig = {
    kind: "candleVllm",
    model: "Qwen3-4B-Instruct-2507",
    hasKey: false,
  };

  private tokenListeners = new Set<Listener<TokenEvent>>();
  private doneListeners = new Set<Listener<TurnDoneEvent>>();
  private errorListeners = new Set<Listener<TurnErrorEvent>>();
  private intentionListeners = new Set<Listener<IntentionSignal>>();
  private toolCallListeners = new Set<Listener<ToolCallEvent>>();
  private toolResultListeners = new Set<Listener<ToolResultEvent>>();
  private approvalRequestListeners = new Set<
    Listener<ToolApprovalRequestEvent>
  >();
  /** `(sessionId, callId)` -> resume callback. Set when a write tool emits an
   *  approval request; the matching `respondApproval` resolves it. Keyed by both
   *  so colliding call ids across sessions stay isolated (mirrors the backend). */
  private pendingApprovals = new Map<string, (approved: boolean) => void>();
  private skillsChangedListeners = new Set<Listener<SkillsChangedEvent>>();
  private activeSkills = new Set<string>();

  async createSession(goal?: string): Promise<Session> {
    const ts = now();
    const session: Session = {
      id: uid(),
      goal: goal ?? null,
      title: null,
      summary: null,
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

  async renameSession(sessionId: string, title: string): Promise<void> {
    const s = this.sessions.get(sessionId);
    if (s) {
      s.title = title;
      s.updatedAt = now();
    }
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
      // Drop any awaiting-approval entry — its tool:result was just emitted.
      this.pendingApprovals.delete(approvalKey(sessionId, callId));
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
  onApprovalRequest(cb: Listener<ToolApprovalRequestEvent>): Promise<Unlisten> {
    return this.subscribe(this.approvalRequestListeners, cb);
  }
  onSkillsChanged(cb: Listener<SkillsChangedEvent>): Promise<Unlisten> {
    return this.subscribe(this.skillsChangedListeners, cb);
  }

  async respondApproval(
    sessionId: string,
    callId: string,
    approved: boolean,
  ): Promise<void> {
    const key = approvalKey(sessionId, callId);
    const resume = this.pendingApprovals.get(key);
    if (!resume) return;
    this.pendingApprovals.delete(key);
    resume(approved);
  }

  async getProviderConfig(): Promise<ProviderConfig> {
    return { ...this.providerConfig };
  }

  async setProviderConfig(
    kind: ProviderKind,
    baseUrl: string | undefined,
    model: string,
  ): Promise<ProviderConfig> {
    const trimmed = baseUrl?.trim();
    this.providerConfig = {
      kind,
      baseUrl: trimmed ? trimmed : undefined,
      model,
      // Secrets are a later phase; the mock never has a key.
      hasKey: false,
    };
    return { ...this.providerConfig };
  }

  async listModels(): Promise<string[]> {
    // Canned per-provider suggestions so the picker is exercisable offline.
    return this.providerConfig.kind === "ollama"
      ? ["llama3.2", "qwen2.5", "mistral"]
      : ["Qwen3-4B-Instruct-2507", "Qwen3-8B-Instruct"];
  }

  async warmup(): Promise<void> {
    // No-op: there is no real server behind the mock.
  }

  async listSkills(): Promise<SkillInfo[]> {
    return [...MOCK_SKILLS]
      .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
      .map((s) => ({ ...s, active: this.activeSkills.has(s.name), score: 0 }));
  }

  async searchSkills(query: string): Promise<SkillInfo[]> {
    const q = query.trim().toLowerCase();
    return MOCK_SKILLS.map((s) => ({
      s,
      score: q === "" ? 0 : scoreSkill(s, q),
    }))
      .filter(({ score }) => q === "" || score > 0)
      .sort(
        (a, b) =>
          b.score - a.score ||
          (a.s.name < b.s.name ? -1 : a.s.name > b.s.name ? 1 : 0),
      )
      .map(({ s, score }) => ({
        ...s,
        active: this.activeSkills.has(s.name),
        score,
      }));
  }

  async activateSkill(name: string): Promise<void> {
    if (!MOCK_SKILLS.some((s) => s.name === name)) {
      throw new Error(`unknown skill: ${name}`);
    }
    this.activeSkills.add(name);
    this.emitSkillsChanged();
  }

  async deactivateSkill(name: string): Promise<void> {
    this.activeSkills.delete(name);
    this.emitSkillsChanged();
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
    const list = this.messages.get(sessionId);
    const isFirstUserMsg =
      role === "user" && !(list ?? []).some((m) => m.role === "user");
    list?.push(msg);
    const s = this.sessions.get(sessionId);
    if (s) {
      s.updatedAt = msg.createdAt;
      // Mirror the backend: first user message seeds an untitled session's title.
      if (isFirstUserMsg && !s.title) s.title = autoTitle(content);
    }
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

    // A couple of auto-resolving read steps before the approval-gated write, so
    // a turn is genuinely multi-step and exercises the StepGroup fold (#17): the
    // "N steps" header, live count while streaming, and collapse on turn:done.
    this.emitAutoStep(
      sessionId,
      assistant.id,
      "view",
      { path: "README.md" },
      "(mocked) read 42 lines from README.md",
    );
    this.emitAutoStep(
      sessionId,
      assistant.id,
      "grep",
      { pattern: "FlowForge", path: "." },
      "(mocked) 7 matches across 3 files",
    );

    // Simulate one write tool call that requires approval, exercising the
    // tool:call -> tool:approval-request -> respondApproval -> tool:result path
    // under VITE_FF_MOCK=1.
    const callId = uidShort();
    turn.pendingToolCalls.push(callId);
    this.emit(this.toolCallListeners, {
      sessionId,
      messageId: assistant.id,
      callId,
      tool: "edit",
      args: { path: "README.md", old_str: "FlowForge", new_str: "FlowForge!" },
    });
    this.emit(this.approvalRequestListeners, {
      sessionId,
      messageId: assistant.id,
      callId,
      tool: "edit",
      args: { path: "README.md", old_str: "FlowForge", new_str: "FlowForge!" },
      safety: "write",
    });
    this.pendingApprovals.set(approvalKey(sessionId, callId), (approved) => {
      turn.pendingToolCalls = turn.pendingToolCalls.filter(
        (id) => id !== callId,
      );
      this.emit(this.toolResultListeners, {
        sessionId,
        messageId: assistant.id,
        callId,
        success: approved,
        result: approved
          ? "(mocked) edited README.md"
          : "call to `edit` was not approved",
      });
      this.streamWords(sessionId, turn);
    });
  }

  // Emit a read-only tool step that resolves immediately (no approval gate).
  // Used to pad a turn to multiple steps so the StepGroup fold is exercised.
  private emitAutoStep(
    sessionId: string,
    messageId: string,
    tool: string,
    args: unknown,
    result: string,
  ): void {
    const callId = uidShort();
    this.emit(this.toolCallListeners, {
      sessionId,
      messageId,
      callId,
      tool,
      args,
    });
    this.emit(this.toolResultListeners, {
      sessionId,
      messageId,
      callId,
      success: true,
      result,
    });
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

  private emitSkillsChanged(): void {
    this.emit(this.skillsChangedListeners, {
      active: [...this.activeSkills].sort(),
    });
  }

  private emit<T>(set: Set<Listener<T>>, payload: T): void {
    set.forEach((cb) => cb(payload));
  }
}
