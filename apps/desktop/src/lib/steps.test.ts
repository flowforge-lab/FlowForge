import { describe, it, expect } from "vitest";
import {
  answerPreview,
  formatDuration,
  groupDurationMs,
  liveElapsedMs,
  resolveGroupOpen,
  selectStepWindow,
  turnStartMs,
} from "@/lib/steps";
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

describe("answerPreview", () => {
  it("strips headings, emphasis, and collapses whitespace into prose", () => {
    expect(
      answerPreview("### Mocked reply\n\nThis is a **bold** answer."),
    ).toBe("Mocked reply This is a bold answer.");
  });

  it("unwraps inline code and links, drops fenced blocks", () => {
    expect(
      answerPreview("Run `npm test` then see [docs](http://x) ```code\nx\n```"),
    ).toBe("Run npm test then see docs");
  });

  it("flattens list and blockquote markers", () => {
    expect(answerPreview("- first\n- second\n> quote")).toBe(
      "first second quote",
    );
  });

  it("leaves identifiers with underscores intact", () => {
    expect(answerPreview("Edited `snake_case_var` in file")).toBe(
      "Edited snake_case_var in file",
    );
  });

  it("is empty for whitespace-only content", () => {
    expect(answerPreview("   \n  ")).toBe("");
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

describe("turnStartMs / liveElapsedMs", () => {
  it("turnStartMs is the earliest startedAt", () => {
    expect(turnStartMs([])).toBeNull();
    expect(
      turnStartMs([step({ startedAt: 2000 }), step({ startedAt: 1000 })]),
    ).toBe(1000);
  });

  it("liveElapsedMs ticks from the first start to now", () => {
    expect(liveElapsedMs([step({ startedAt: 1000 })], 4500)).toBe(3500);
  });

  it("liveElapsedMs uses wall-clock send time before the first tool:call", () => {
    expect(liveElapsedMs([], 5000, 1000)).toBe(4000);
  });

  it("liveElapsedMs uses the earliest of wall-clock and step start", () => {
    expect(liveElapsedMs([step({ startedAt: 3000 })], 5000, 1000)).toBe(4000);
    expect(liveElapsedMs([step({ startedAt: 1500 })], 5000, 2000)).toBe(3500);
  });
});

describe("selectStepWindow", () => {
  const many = Array.from({ length: 5 }, (_, i) =>
    step({ callId: `c${i}`, startedAt: i * 100 }),
  );

  it("shows only the last 3 while streaming", () => {
    const { visible, hiddenCount } = selectStepWindow(many, {
      streaming: true,
      awaiting: false,
      peekExpanded: false,
    });
    expect(visible.map((s) => s.callId)).toEqual(["c2", "c3", "c4"]);
    expect(hiddenCount).toBe(2);
  });

  it("shows all steps when peek-expanded, settled, or ≤ window", () => {
    expect(
      selectStepWindow(many, {
        streaming: true,
        awaiting: false,
        peekExpanded: true,
      }).hiddenCount,
    ).toBe(0);
    expect(
      selectStepWindow(many, {
        streaming: false,
        awaiting: false,
        peekExpanded: false,
      }).visible.length,
    ).toBe(5);
    expect(
      selectStepWindow(many.slice(0, 2), {
        streaming: true,
        awaiting: false,
        peekExpanded: false,
      }).hiddenCount,
    ).toBe(0);
  });

  it("shows all steps while awaiting approval or answer", () => {
    const awaiting = many.map((s, i) =>
      i === 4 ? { ...s, status: "awaiting-approval" as const } : s,
    );
    const { visible, hiddenCount } = selectStepWindow(awaiting, {
      streaming: true,
      awaiting: true,
      peekExpanded: false,
    });
    expect(visible.length).toBe(5);
    expect(hiddenCount).toBe(0);
  });
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
