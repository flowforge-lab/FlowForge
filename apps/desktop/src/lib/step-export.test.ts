import { describe, it, expect } from "vitest";
import {
  buildTimeline,
  timelineToCsv,
  timelineToJson,
  timelineFilename,
  type TimelineMeta,
} from "@/lib/step-export";
import type { ToolStep } from "@/store/chat";
import type { TurnItem } from "@/lib/turn-groups";

function step(partial: Partial<ToolStep>): ToolStep {
  return { callId: "c", tool: "bash", args: {}, status: "done", ...partial };
}

// The export now takes the turn's interleaved `TurnItem[]` (foldTurns order, #593);
// these wrap fixtures into that shape so the tests read close to the on-screen list.
function stepItem(partial: Partial<ToolStep>): TurnItem {
  return { kind: "step", step: step(partial) };
}
function reasoningItem(text: string, key = "a"): TurnItem {
  return { kind: "reasoning", text, key };
}

const META: TimelineMeta = {
  sessionId: "06d8ba6e-0159-4024",
  model: "Qwen3-4B",
  timing: "exact",
  capturedAt: 1_700_000_000_000,
};

describe("buildTimeline", () => {
  it("summarizes each step with duration, result bytes, and args", () => {
    const dump = buildTimeline(
      [
        stepItem({
          tool: "bash",
          args: { command: "ls" },
          startedAt: 1000,
          finishedAt: 1500,
          result: "out",
        }),
        stepItem({
          tool: "grep",
          args: { pattern: "x" },
          startedAt: 1500,
          finishedAt: 4000,
          result: "seven matches",
        }),
      ],
      META,
    );
    expect(dump.rows).toHaveLength(2);
    expect(dump.rows[0]).toMatchObject({
      kind: "step",
      index: 0,
      tool: "bash",
      status: "done",
      durationMs: 500,
      resultBytes: 3,
      argsSummary: '{ "command": "ls" }',
    });
    expect(dump.rows[1]).toMatchObject({ index: 1, durationMs: 2500 });
    expect(dump.totals).toEqual({
      steps: 2,
      reasoning: 0,
      totalMs: 3000, // first start 1000 → last finish 4000
      resultBytes: 16,
    });
    expect(dump.timing).toBe("exact");
    expect(dump.capturedAt).toBe("2023-11-14T22:13:20.000Z");
  });

  it("interleaves each iteration's reasoning before its steps (#593)", () => {
    // Iteration 1 reasons then calls two tools; iteration 2 reasons then answers
    // (no tool call) — mirrors foldTurns' `reasoning → steps` order per iteration.
    const dump = buildTimeline(
      [
        reasoningItem("plan: read the file", "a1"),
        stepItem({ callId: "c1", tool: "view" }),
        stepItem({ callId: "c2", tool: "grep" }),
        reasoningItem("now I can answer", "a2"),
      ],
      META,
    );
    expect(dump.rows.map((r) => r.kind)).toEqual([
      "reasoning",
      "step",
      "step",
      "reasoning",
    ]);
    // A single running index spans the interleaved rows, matching render order.
    expect(dump.rows.map((r) => r.index)).toEqual([0, 1, 2, 3]);
    expect(dump.rows[0]).toMatchObject({
      kind: "reasoning",
      text: "plan: read the file",
      chars: 19,
    });
    expect(dump.totals).toMatchObject({ steps: 2, reasoning: 2 });
  });

  it("emits no reasoning rows when the turn has none", () => {
    const dump = buildTimeline([stepItem({ tool: "bash" })], META);
    expect(dump.rows).toHaveLength(1);
    expect(dump.rows[0].kind).toBe("step");
    expect(dump.totals.reasoning).toBe(0);
  });

  it("drops intermediate prose items — only steps + reasoning are exported", () => {
    const dump = buildTimeline(
      [
        { kind: "prose", text: "narration", key: "a1" },
        reasoningItem("thinking", "a1"),
        stepItem({ tool: "bash" }),
      ],
      META,
    );
    expect(dump.rows.map((r) => r.kind)).toEqual(["reasoning", "step"]);
  });

  it("emits null durations when a step is missing timing (approx fallback)", () => {
    const dump = buildTimeline([stepItem({ startedAt: undefined })], {
      ...META,
      timing: "approx-created-at",
    });
    expect(dump.rows[0]).toMatchObject({
      kind: "step",
      durationMs: null,
      startedAt: null,
    });
    expect(dump.totals.totalMs).toBeNull();
    expect(dump.timing).toBe("approx-created-at");
  });
});

describe("timelineToJson", () => {
  it("pretty-prints the dump round-trippable to the same object", () => {
    const dump = buildTimeline(
      [stepItem({ startedAt: 0, finishedAt: 10 })],
      META,
    );
    const json = timelineToJson(dump);
    expect(json).toContain('"timing": "exact"');
    expect(JSON.parse(json)).toEqual(dump);
  });
});

describe("timelineToCsv", () => {
  it("writes a header row + one row per item, with numeric durationMs", () => {
    const dump = buildTimeline(
      [
        stepItem({
          tool: "bash",
          args: { command: "ls" },
          startedAt: 1000,
          finishedAt: 1500,
          result: "out",
        }),
      ],
      META,
    );
    const csv = timelineToCsv(dump);
    const lines = csv.trimEnd().split("\n");
    expect(lines[0]).toBe(
      "index,kind,tool,status,durationMs,startedAt,finishedAt,resultBytes,argsSummary,reasoning",
    );
    expect(lines[1]).toContain("0,step,bash,done,500,1000,1500,3,");
  });

  it("interleaves reasoning rows in order with a blank step-only columns (#593)", () => {
    const dump = buildTimeline(
      [
        reasoningItem("plan:\n  read", "a1"),
        stepItem({ tool: "view", startedAt: 0, finishedAt: 5 }),
      ],
      META,
    );
    const rows = timelineToCsv(dump).trimEnd().split("\n").slice(1);
    // Reasoning row: kind=reasoning, no tool/status/timing, text in the last column
    // (whitespace collapsed to one line).
    expect(rows[0]).toBe("0,reasoning,,,,,,,,plan: read");
    expect(rows[1]).toContain("1,step,view,done,5,0,5,0,");
  });

  it("quotes cells containing commas or quotes (RFC 4180)", () => {
    const dump = buildTimeline(
      [
        stepItem({
          tool: "bash",
          args: { command: "a, b" },
          startedAt: 0,
          finishedAt: 1,
        }),
      ],
      META,
    );
    const row = timelineToCsv(dump).trimEnd().split("\n")[1];
    // The args summary has a comma → the whole cell is double-quoted.
    expect(row).toContain('"{ ');
    expect(row).toContain("a, b");
  });

  it("renders an empty cell for a null duration", () => {
    const dump = buildTimeline([stepItem({ startedAt: undefined })], META);
    const row = timelineToCsv(dump).trimEnd().split("\n")[1];
    // index,kind,tool,status,durationMs(empty),startedAt(empty),...
    expect(row.startsWith("0,step,bash,done,,,")).toBe(true);
  });
});

describe("timelineFilename", () => {
  it("uses the session prefix and the format extension", () => {
    const dump = buildTimeline([], META);
    expect(timelineFilename(dump, "csv")).toBe("step-timeline-06d8ba6e.csv");
    expect(timelineFilename(dump, "json")).toBe("step-timeline-06d8ba6e.json");
  });
});
