import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useChatStore } from "@/store/chat";
import type { TurnStatsEvent } from "@/bindings";

// applyTurnStats plumbing for TTFT + promptLatencyMs (#960 FE follow-up).

const SID = "s1";

function stats(extra: Partial<TurnStatsEvent> = {}): TurnStatsEvent {
  return {
    sessionId: SID,
    roundTrips: 1,
    totalMs: 10_000,
    iterMs: [10_000],
    flushes: 0,
    outputTokens: 50,
    ...extra,
  };
}

beforeEach(() => {
  useChatStore.setState({
    ttftBySession: {},
    promptLatencyBySession: {},
  });
});

afterEach(() => {
  useChatStore.setState({
    ttftBySession: {},
    promptLatencyBySession: {},
  });
});

describe("applyTurnStats (#960)", () => {
  it("stores both firstTokenMs and promptLatencyMs", () => {
    useChatStore
      .getState()
      .applyTurnStats(stats({ firstTokenMs: 8200, promptLatencyMs: 6000 }));
    expect(useChatStore.getState().ttftBySession[SID]).toBe(8200);
    expect(useChatStore.getState().promptLatencyBySession[SID]).toBe(6000);
  });

  it("clears a prior promptLatency when a new turn reports TTFT without it", () => {
    useChatStore.setState({
      ttftBySession: { [SID]: 5000 },
      promptLatencyBySession: { [SID]: 4000 },
    });
    useChatStore.getState().applyTurnStats(stats({ firstTokenMs: 3000 }));
    expect(useChatStore.getState().ttftBySession[SID]).toBe(3000);
    expect(useChatStore.getState().promptLatencyBySession[SID]).toBeUndefined();
  });

  it("is a no-op when both latency fields are absent", () => {
    useChatStore.setState({
      ttftBySession: { [SID]: 1000 },
      promptLatencyBySession: { [SID]: 800 },
    });
    useChatStore.getState().applyTurnStats(stats({}));
    expect(useChatStore.getState().ttftBySession[SID]).toBe(1000);
    expect(useChatStore.getState().promptLatencyBySession[SID]).toBe(800);
  });
});
