import { describe, it, expect } from "vitest";
import { foldTurns, persistedStepToToolStep } from "@/lib/turn-groups";
import type { Message } from "@/bindings";
import type { ToolCall } from "@/bindings/ToolCall";

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
