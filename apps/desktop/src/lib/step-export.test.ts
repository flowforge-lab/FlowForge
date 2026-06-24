import { describe, it, expect } from "vitest";
import {
  buildTimeline,
  timelineToCsv,
  timelineToJson,
  timelineFilename,
  type TimelineMeta,
} from "@/lib/step-export";
import type { ToolStep } from "@/store/chat";

function step(partial: Partial<ToolStep>): ToolStep {
  return { callId: "c", tool: "bash", args: {}, status: "done", ...partial };
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
        step({
          tool: "bash",
          args: { command: "ls" },
          startedAt: 1000,
          finishedAt: 1500,
          result: "out",
        }),
        step({
          tool: "grep",
          args: { pattern: "x" },
          startedAt: 1500,
          finishedAt: 4000,
          result: "seven matches",
        }),
      ],
      META,
    );
    expect(dump.steps).toHaveLength(2);
    expect(dump.steps[0]).toMatchObject({
      index: 0,
      tool: "bash",
      status: "done",
      durationMs: 500,
      resultBytes: 3,
      argsSummary: '{ "command": "ls" }',
    });
    expect(dump.steps[1].durationMs).toBe(2500);
    expect(dump.totals).toEqual({
      steps: 2,
      totalMs: 3000, // first start 1000 → last finish 4000
      resultBytes: 16,
    });
    expect(dump.timing).toBe("exact");
    expect(dump.capturedAt).toBe("2023-11-14T22:13:20.000Z");
  });

  it("emits null durations when a step is missing timing (approx fallback)", () => {
    const dump = buildTimeline([step({ startedAt: undefined })], {
      ...META,
      timing: "approx-created-at",
    });
    expect(dump.steps[0].durationMs).toBeNull();
    expect(dump.steps[0].startedAt).toBeNull();
    expect(dump.totals.totalMs).toBeNull();
    expect(dump.timing).toBe("approx-created-at");
  });
});

describe("timelineToJson", () => {
  it("pretty-prints the dump round-trippable to the same object", () => {
    const dump = buildTimeline([step({ startedAt: 0, finishedAt: 10 })], META);
    const json = timelineToJson(dump);
    expect(json).toContain('"timing": "exact"');
    expect(JSON.parse(json)).toEqual(dump);
  });
});

describe("timelineToCsv", () => {
  it("writes a header row + one row per step, with numeric durationMs", () => {
    const dump = buildTimeline(
      [
        step({
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
      "index,tool,status,durationMs,startedAt,finishedAt,resultBytes,argsSummary",
    );
    expect(lines[1]).toContain("0,bash,done,500,1000,1500,3,");
  });

  it("quotes cells containing commas or quotes (RFC 4180)", () => {
    const dump = buildTimeline(
      [
        step({
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
    const dump = buildTimeline([step({ startedAt: undefined })], META);
    const row = timelineToCsv(dump).trimEnd().split("\n")[1];
    // index,tool,status,durationMs(empty),startedAt(empty),...
    expect(row.startsWith("0,bash,done,,,")).toBe(true);
  });
});

describe("timelineFilename", () => {
  it("uses the session prefix and the format extension", () => {
    const dump = buildTimeline([], META);
    expect(timelineFilename(dump, "csv")).toBe("step-timeline-06d8ba6e.csv");
    expect(timelineFilename(dump, "json")).toBe("step-timeline-06d8ba6e.json");
  });
});
