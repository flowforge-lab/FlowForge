import { describe, it, expect } from "vitest";
import { formatDuration, groupDurationMs, resolveGroupOpen } from "@/lib/steps";
import type { ToolStep } from "@/store/chat";

function step(partial: Partial<ToolStep>): ToolStep {
  return { callId: "c", tool: "t", args: {}, status: "done", ...partial };
}

describe("groupDurationMs", () => {
  it("is null when nothing has started or finished", () => {
    expect(groupDurationMs([])).toBeNull();
  });

  it("is null mid-stream (started, no result yet)", () => {
    expect(groupDurationMs([step({ startedAt: 100 })])).toBeNull();
  });

  it("spans the first start to the last finish across steps", () => {
    const steps = [
      step({ startedAt: 1000, finishedAt: 1500 }),
      step({ startedAt: 1200, finishedAt: 4000 }),
    ];
    expect(groupDurationMs(steps)).toBe(3000);
  });

  it("clamps a skewed clock to 0 rather than going negative", () => {
    expect(groupDurationMs([step({ startedAt: 5000, finishedAt: 4000 })])).toBe(
      0,
    );
  });
});

describe("formatDuration", () => {
  it("collapses sub-second to <1s", () =>
    expect(formatDuration(400)).toBe("<1s"));
  it("renders whole seconds", () => expect(formatDuration(8000)).toBe("8s"));
  it("rounds to the nearest second", () =>
    expect(formatDuration(8600)).toBe("9s"));
  it("renders minutes and seconds", () =>
    expect(formatDuration(63000)).toBe("1m 3s"));
  it("drops the seconds on a whole minute", () =>
    expect(formatDuration(120000)).toBe("2m"));
});

describe("resolveGroupOpen", () => {
  it("forces open while awaiting approval, ignoring user + streaming", () => {
    expect(
      resolveGroupOpen({ awaiting: true, userOpen: false, streaming: false }),
    ).toBe(true);
  });

  it("follows the turn when untouched: open while streaming, closed once settled", () => {
    expect(
      resolveGroupOpen({ awaiting: false, userOpen: null, streaming: true }),
    ).toBe(true);
    expect(
      resolveGroupOpen({ awaiting: false, userOpen: null, streaming: false }),
    ).toBe(false);
  });

  it("persists a manual expand through turn settle (no auto-collapse override)", () => {
    expect(
      resolveGroupOpen({ awaiting: false, userOpen: true, streaming: false }),
    ).toBe(true);
  });

  it("persists a manual collapse mid-stream", () => {
    expect(
      resolveGroupOpen({ awaiting: false, userOpen: false, streaming: true }),
    ).toBe(false);
  });
});
