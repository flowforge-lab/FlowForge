import { describe, it, expect } from "vitest";
import {
  foldTurns,
  persistedStepToToolStep,
  segmentTurn,
} from "@/lib/turn-groups";
import type { TurnItem } from "@/lib/turn-groups";
import type { Message } from "@/bindings";
import type { ToolCall } from "@/bindings/ToolCall";
import type { ToolStep } from "@/store/chat";

const SID = "s1";
let seq = 0;

function msg(partial: Partial<Message> & Pick<Message, "role">): Message {
  return {
    id: partial.id ?? `m${seq++}`,
    sessionId: SID,
    content: "",
    createdAt: 0,
    ...partial,
  };
}

function call(id: string, name: string, args: unknown): ToolCall {
  return { id, name, arguments: JSON.stringify(args) };
}

describe("foldTurns — grouping", () => {
  it("emits a user message as a user group", () => {
    const groups = foldTurns([msg({ role: "user", content: "hi" })]);
    expect(groups).toHaveLength(1);
    expect(groups[0].kind).toBe("user");
  });

  it("folds the real shape — one assistant per call, interleaved with results — into one turn", () => {
    const groups = foldTurns([
      msg({ role: "user", content: "go" }),
      msg({
        id: "a1",
        role: "assistant",
        toolCalls: [call("c1", "view", { path: "README.md" })],
      }),
      msg({ role: "tool", toolCallId: "c1", content: "42 lines" }),
      msg({ id: "a2", role: "assistant", toolCalls: [call("c2", "grep", {})] }),
      msg({ role: "tool", toolCallId: "c2", content: "7 matches" }),
      msg({ id: "a3", role: "assistant", content: "all done" }),
    ]);
    expect(groups.map((g) => g.kind)).toEqual(["user", "assistant"]);
    const turn = groups[1];
    if (turn.kind !== "assistant") throw new Error("expected assistant turn");
    // Two tool results → two steps, each resolved to its call across the turn.
    expect(turn.steps).toHaveLength(2);
    expect(turn.steps.map((s) => s.tool)).toEqual(["view", "grep"]);
    expect(turn.steps[0].result).toBe("42 lines");
    // The final assistant message is the turn's representative (answer text).
    expect(turn.message.id).toBe("a3");
    expect(turn.message.content).toBe("all done");
  });

  it("matches parallel calls declared on a single assistant message", () => {
    const groups = foldTurns([
      msg({ role: "user", content: "go" }),
      msg({
        id: "a1",
        role: "assistant",
        content: "done",
        toolCalls: [call("c1", "view", {}), call("c2", "grep", {})],
      }),
      msg({ role: "tool", toolCallId: "c1", content: "r1" }),
      msg({ role: "tool", toolCallId: "c2", content: "r2" }),
    ]);
    const turn = groups[1];
    if (turn.kind !== "assistant") throw new Error("expected assistant turn");
    expect(turn.steps.map((s) => s.tool)).toEqual(["view", "grep"]);
  });

  it("ends a turn only at the next USER message, not at an assistant message", () => {
    const groups = foldTurns([
      msg({ role: "user", content: "q1" }),
      msg({ id: "a1", role: "assistant", toolCalls: [call("c1", "view", {})] }),
      msg({ role: "tool", toolCallId: "c1", content: "r1" }),
      msg({ id: "a2", role: "assistant", content: "answer 1" }),
      msg({ role: "user", content: "q2" }),
      msg({ id: "a3", role: "assistant", content: "answer 2" }),
    ]);
    expect(groups.map((g) => g.kind)).toEqual([
      "user",
      "assistant",
      "user",
      "assistant",
    ]);
    const t1 = groups[1];
    if (t1.kind !== "assistant") throw new Error("expected assistant turn");
    expect(t1.steps).toHaveLength(1);
    expect(t1.message.id).toBe("a2");
  });

  it("keeps a single-step turn (1 result) as a 1-step group", () => {
    const groups = foldTurns([
      msg({ role: "user", content: "go" }),
      msg({ id: "a1", role: "assistant", toolCalls: [call("c1", "view", {})] }),
      msg({ role: "tool", toolCallId: "c1", content: "result" }),
      msg({ id: "a2", role: "assistant", content: "ok" }),
    ]);
    const turn = groups[1];
    if (turn.kind !== "assistant") throw new Error("expected assistant turn");
    expect(turn.steps).toHaveLength(1);
  });

  it("renders an orphan tool/system message as a loose row (#331)", () => {
    const groups = foldTurns([msg({ role: "tool", content: "orphan" })]);
    expect(groups).toHaveLength(1);
    expect(groups[0].kind).toBe("loose");
  });

  it("carries the turn's opening reasoning", () => {
    const groups = foldTurns([
      msg({ role: "user", content: "go" }),
      msg({
        id: "a1",
        role: "assistant",
        reasoning: "thinking…",
        toolCalls: [call("c1", "view", {})],
      }),
      msg({ role: "tool", toolCallId: "c1", content: "r" }),
      msg({ id: "a2", role: "assistant", content: "done" }),
    ]);
    const turn = groups[1];
    if (turn.kind !== "assistant") throw new Error("expected assistant turn");
    expect(turn.reasoning).toBe("thinking…");
  });
});

describe("foldTurns — duration", () => {
  it("derives durationMs from the createdAt spread across the whole turn", () => {
    const groups = foldTurns([
      msg({ role: "user", content: "go", createdAt: 900 }),
      msg({
        id: "a1",
        role: "assistant",
        createdAt: 1000,
        toolCalls: [call("c1", "view", {})],
      }),
      msg({ role: "tool", toolCallId: "c1", content: "r1", createdAt: 1500 }),
      msg({
        id: "a2",
        role: "assistant",
        createdAt: 1600,
        toolCalls: [call("c2", "grep", {})],
      }),
      msg({ role: "tool", toolCallId: "c2", content: "r2", createdAt: 4000 }),
    ]);
    const turn = groups[1];
    if (turn.kind !== "assistant") throw new Error("expected assistant turn");
    // first start (a1 createdAt 1000) → last finish (tool createdAt 4000)
    expect(turn.durationMs).toBe(3000);
  });

  it("is null when timestamps are absent (createdAt 0 sentinel)", () => {
    const groups = foldTurns([
      msg({ role: "user", content: "go", createdAt: 0 }),
      msg({
        id: "a1",
        role: "assistant",
        createdAt: 0,
        toolCalls: [call("c1", "view", {})],
      }),
      msg({ role: "tool", toolCallId: "c1", content: "r", createdAt: 0 }),
    ]);
    const turn = groups[1];
    if (turn.kind !== "assistant") throw new Error("expected assistant turn");
    expect(turn.durationMs).toBeNull();
  });
});

describe("persistedStepToToolStep — adapter", () => {
  it("maps a matched call to tool name + parsed args, with createdAt timing", () => {
    const step = persistedStepToToolStep(
      msg({ role: "tool", toolCallId: "c1", content: "out", createdAt: 1800 }),
      call("c1", "bash", { command: "ls" }),
      0,
      "a1",
      1000,
    );
    expect(step.tool).toBe("bash");
    expect(step.args).toEqual({ command: "ls" });
    expect(step.callId).toBe("c1");
    expect(step.status).toBe("done");
    expect(step.result).toBe("out");
    expect(step.startedAt).toBe(1000);
    expect(step.finishedAt).toBe(1800);
  });

  it("falls back to a synthetic callId and the role as tool when unmatched", () => {
    const step = persistedStepToToolStep(
      msg({ role: "tool", content: "out", createdAt: 0 }),
      undefined,
      2,
      "a1",
      undefined,
    );
    expect(step.callId).toBe("a1:step-2");
    expect(step.tool).toBe("tool");
    expect(step.args).toBeNull();
    expect(step.startedAt).toBeUndefined();
    expect(step.finishedAt).toBeUndefined();
  });
});

describe("foldTurns — interleaved prose (#415)", () => {
  function tags(turn: ReturnType<typeof foldTurns>[number]): string[] {
    if (turn.kind !== "assistant") throw new Error("expected assistant turn");
    return turn.items.map((it) =>
      it.kind === "reasoning"
        ? `reasoning:${it.text}`
        : it.kind === "prose"
          ? `prose:${it.text}`
          : `step:${it.step.tool}`,
    );
  }

  it("interleaves intermediate prose between steps, in message order", () => {
    const groups = foldTurns([
      msg({ role: "user", content: "go" }),
      msg({
        id: "a1",
        role: "assistant",
        content: "Let me read the file.",
        toolCalls: [call("c1", "view", {})],
      }),
      msg({ role: "tool", toolCallId: "c1", content: "r1" }),
      msg({
        id: "a2",
        role: "assistant",
        content: "Found it. Now searching.",
        toolCalls: [call("c2", "grep", {})],
      }),
      msg({ role: "tool", toolCallId: "c2", content: "r2" }),
      msg({ id: "a3", role: "assistant", content: "All done." }),
    ]);
    const turn = groups[1];
    expect(tags(turn)).toEqual([
      "prose:Let me read the file.",
      "step:view",
      "prose:Found it. Now searching.",
      "step:grep",
    ]);
    if (turn.kind !== "assistant") throw new Error("expected assistant turn");
    // The final assistant message is the answer, never a prose row.
    expect(turn.message.id).toBe("a3");
    expect(turn.message.content).toBe("All done.");
    expect(turn.steps).toHaveLength(2);
  });

  it("emits no prose for a single-assistant turn (its content is the answer)", () => {
    const groups = foldTurns([
      msg({ role: "user", content: "go" }),
      msg({
        id: "a1",
        role: "assistant",
        content: "answer only",
        toolCalls: [call("c1", "view", {})],
      }),
      msg({ role: "tool", toolCallId: "c1", content: "r1" }),
    ]);
    expect(tags(groups[1])).toEqual(["step:view"]);
  });

  it("prefers live steps over reconstruction, per assistant message", () => {
    const liveStep = {
      callId: "c1",
      tool: "bash",
      args: {},
      status: "done" as const,
      result: "live result",
      startedAt: 1000,
      finishedAt: 1200,
    };
    const groups = foldTurns(
      [
        msg({ role: "user", content: "go" }),
        msg({
          id: "a1",
          role: "assistant",
          content: "Working on it.",
          toolCalls: [call("c1", "view", {})],
        }),
        msg({ id: "a2", role: "assistant", content: "done" }),
      ],
      { a1: [liveStep] },
    );
    const turn = groups[1];
    if (turn.kind !== "assistant") throw new Error("expected assistant turn");
    expect(turn.steps).toHaveLength(1);
    expect(turn.steps[0].result).toBe("live result");
    expect(tags(turn)).toEqual(["prose:Working on it.", "step:bash"]);
  });
});

describe("foldTurns — interleaved reasoning (#574)", () => {
  // Includes the reasoning variant so we can assert chronological position.
  function tags(turn: ReturnType<typeof foldTurns>[number]): string[] {
    if (turn.kind !== "assistant") throw new Error("expected assistant turn");
    return turn.items.map((it) =>
      it.kind === "reasoning"
        ? `reasoning:${it.text}`
        : it.kind === "prose"
          ? `prose:${it.text}`
          : `step:${it.step.tool}`,
    );
  }

  it("places each iteration's reasoning immediately before its steps, in order", () => {
    // Reasoning on iterations 1 and 3; none on iteration 2 → two reasoning items.
    const groups = foldTurns([
      msg({ role: "user", content: "go" }),
      msg({
        id: "a1",
        role: "assistant",
        reasoning: "plan: read the file",
        toolCalls: [call("c1", "view", {})],
      }),
      msg({ role: "tool", toolCallId: "c1", content: "r1" }),
      msg({
        id: "a2",
        role: "assistant",
        toolCalls: [call("c2", "grep", {})],
      }),
      msg({ role: "tool", toolCallId: "c2", content: "r2" }),
      msg({
        id: "a3",
        role: "assistant",
        reasoning: "now I can answer",
        content: "All done.",
      }),
    ]);
    const turn = groups[1];
    expect(tags(turn)).toEqual([
      "reasoning:plan: read the file",
      "step:view",
      "step:grep",
      "reasoning:now I can answer",
    ]);
    if (turn.kind !== "assistant") throw new Error("expected assistant turn");
    // The final answer text stays on the turn's representative message, not a row.
    expect(turn.message.id).toBe("a3");
    expect(turn.message.content).toBe("All done.");
  });

  it("orders prose before reasoning before steps within one iteration (#619)", () => {
    const groups = foldTurns([
      msg({ role: "user", content: "go" }),
      msg({
        id: "a1",
        role: "assistant",
        reasoning: "thinking",
        content: "Let me check.",
        toolCalls: [call("c1", "view", {})],
      }),
      msg({ role: "tool", toolCallId: "c1", content: "r1" }),
      msg({ id: "a2", role: "assistant", content: "done" }),
    ]);
    // Prose leads so `segmentTurn` can hoist it to a top-level block, leaving the
    // iteration's reasoning contiguous with the steps it produced.
    expect(tags(groups[1])).toEqual([
      "prose:Let me check.",
      "reasoning:thinking",
      "step:view",
    ]);
  });

  it("prefers live reasoning over the persisted copy, per assistant message", () => {
    const groups = foldTurns(
      [
        msg({ role: "user", content: "go" }),
        msg({
          id: "a1",
          role: "assistant",
          reasoning: "persisted",
          toolCalls: [call("c1", "view", {})],
        }),
        msg({ role: "tool", toolCallId: "c1", content: "r1" }),
        msg({ id: "a2", role: "assistant", content: "done" }),
      ],
      undefined,
      { a1: "live stream" },
    );
    expect(tags(groups[1])).toEqual(["reasoning:live stream", "step:view"]);
  });

  it("emits no reasoning item when an iteration has no reasoning", () => {
    const groups = foldTurns([
      msg({ role: "user", content: "go" }),
      msg({ id: "a1", role: "assistant", toolCalls: [call("c1", "view", {})] }),
      msg({ role: "tool", toolCallId: "c1", content: "r1" }),
      msg({ id: "a2", role: "assistant", content: "done" }),
    ]);
    expect(tags(groups[1])).toEqual(["step:view"]);
  });
});

describe("segmentTurn (#619)", () => {
  function step(callId: string, tool = "bash"): ToolStep {
    return { callId, tool, args: {}, status: "done" };
  }
  // A compact tag per segment: `prose:<text>` or `steps[<n>]:<itemKinds>`.
  function tag(seg: ReturnType<typeof segmentTurn>[number]): string {
    if (seg.kind === "prose") return `prose:${seg.text}`;
    const kinds = seg.items
      .map((it) => (it.kind === "step" ? "step" : it.kind))
      .join(",");
    return `steps[${seg.steps.length}]:${kinds}`;
  }

  it("hoists prose into standalone segments, splitting the steps around it", () => {
    const items: TurnItem[] = [
      { kind: "prose", text: "Let me read the file.", key: "a1" },
      { kind: "step", step: step("c1", "view") },
      { kind: "prose", text: "Found it. Now searching.", key: "a2" },
      { kind: "step", step: step("c2", "grep") },
    ];
    expect(segmentTurn(items).map(tag)).toEqual([
      "prose:Let me read the file.",
      "steps[1]:step",
      "prose:Found it. Now searching.",
      "steps[1]:step",
    ]);
  });

  it("keeps an iteration's reasoning contiguous with its steps in one segment", () => {
    const items: TurnItem[] = [
      { kind: "prose", text: "narration", key: "a1" },
      { kind: "reasoning", text: "thinking", key: "a1" },
      { kind: "step", step: step("c1") },
      { kind: "step", step: step("c2") },
    ];
    const segs = segmentTurn(items);
    expect(segs.map(tag)).toEqual([
      "prose:narration",
      "steps[2]:reasoning,step,step",
    ]);
  });

  it("returns a single steps segment when there is no prose", () => {
    const items: TurnItem[] = [
      { kind: "step", step: step("c1") },
      { kind: "step", step: step("c2") },
    ];
    expect(segmentTurn(items).map(tag)).toEqual(["steps[2]:step,step"]);
  });

  it("returns one steps segment for a single step", () => {
    const items: TurnItem[] = [{ kind: "step", step: step("c1") }];
    const segs = segmentTurn(items);
    expect(segs).toHaveLength(1);
    expect(segs[0]).toMatchObject({ kind: "steps" });
  });

  it("interleaves multiple prose blocks with their step groups in order", () => {
    const items: TurnItem[] = [
      { kind: "prose", text: "p1", key: "a1" },
      { kind: "step", step: step("c1") },
      { kind: "prose", text: "p2", key: "a2" },
      { kind: "step", step: step("c2") },
      { kind: "prose", text: "p3", key: "a3" },
      { kind: "step", step: step("c3") },
    ];
    const segs = segmentTurn(items);
    // Three prose blocks, three steps groups, strictly alternating.
    expect(segs.map((s) => s.kind)).toEqual([
      "prose",
      "steps",
      "prose",
      "steps",
      "prose",
      "steps",
    ]);
  });

  it("returns nothing for an empty turn", () => {
    expect(segmentTurn([])).toEqual([]);
  });
});
