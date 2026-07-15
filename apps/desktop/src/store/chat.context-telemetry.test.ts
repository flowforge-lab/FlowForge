import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useChatStore } from "@/store/chat";
import type {
  ContextBreakdown,
  Session,
  TurnDoneEvent,
  TurnUsage,
} from "@/bindings";

// Context-usage telemetry accumulation on turn completion (#931): finishTurn
// records the latest breakdown + authoritative input tokens, and sums each turn's
// provider usage into a running per-session total for the SESSION TOTALS block.

const SID = "s1";

function session(id: string): Session {
  return {
    id,
    goal: null,
    title: "Work",
    summary: null,
    status: "active",
    createdAt: 0,
    updatedAt: 0,
  };
}

function done(sessionId: string, extra: Partial<TurnDoneEvent>): TurnDoneEvent {
  return {
    sessionId,
    messageId: `${sessionId}-m1`,
    tokenCount: null,
    ...extra,
  } as TurnDoneEvent;
}

const BREAKDOWN: ContextBreakdown = {
  systemTokens: 12_000,
  toolTokens: 2_900,
  toolSpecs: 1,
  messageTokens: 158_000,
  messageCount: 122,
};

const USAGE: TurnUsage = {
  inputTokens: 100,
  outputTokens: 20,
  cacheReadTokens: 40,
  cacheWriteTokens: 5,
};

beforeEach(() => {
  vi.useFakeTimers();
  useChatStore.setState({
    sessions: [session(SID)],
    activeSessionId: SID,
    // Non-empty transcript so finishTurn skips the empty-turn refetch path.
    messagesBySession: {
      [SID]: [
        {
          id: `${SID}-m1`,
          sessionId: SID,
          role: "assistant",
          content: "done",
          createdAt: 0,
        },
      ],
    },
    streamingBySession: { [SID]: `${SID}-m1` },
    contextBreakdownBySession: {},
    contextInputTokensBySession: {},
    sessionTotalsBySession: {},
  });
});

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe("chat store — context telemetry (#931)", () => {
  it("records the breakdown and authoritative input tokens from a turn", () => {
    useChatStore
      .getState()
      .finishTurn(done(SID, { breakdown: BREAKDOWN, usage: USAGE }));

    const s = useChatStore.getState();
    expect(s.contextBreakdownBySession[SID]).toEqual(BREAKDOWN);
    expect(s.contextInputTokensBySession[SID]).toBe(100);
  });

  it("accumulates session totals across turns, field by field", () => {
    const store = useChatStore.getState();
    store.finishTurn(done(SID, { usage: USAGE }));
    store.finishTurn(
      done(SID, {
        usage: {
          inputTokens: 900,
          outputTokens: 80,
          cacheReadTokens: 60,
          cacheWriteTokens: 15,
        },
      }),
    );

    expect(useChatStore.getState().sessionTotalsBySession[SID]).toEqual({
      inputTokens: 1_000,
      outputTokens: 100,
      cacheReadTokens: 100,
      cacheWriteTokens: 20,
    });
  });

  it("leaves prior breakdown/usage intact on a turn that carries none", () => {
    const store = useChatStore.getState();
    store.finishTurn(done(SID, { breakdown: BREAKDOWN, usage: USAGE }));
    // A non-telemetry turn (no breakdown/usage) must not clobber prior values.
    store.finishTurn(done(SID, {}));

    const s = useChatStore.getState();
    expect(s.contextBreakdownBySession[SID]).toEqual(BREAKDOWN);
    expect(s.sessionTotalsBySession[SID]).toEqual(USAGE);
    expect(s.contextInputTokensBySession[SID]).toBe(100);
  });
});
