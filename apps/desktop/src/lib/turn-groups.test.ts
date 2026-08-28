import { describe, it, expect } from "vitest";
import {
  foldTurns,
  isSubstantiveProse,
  lastTurnStart,
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
          : it.kind === "thought"
            ? `thought:${it.text}`
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
          : it.kind === "thought"
            ? `thought:${it.text}`
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
  // Prose that classifies as substantive (#687): ≥SHORT, no operational prefix, and
  // formatted (inline code), so it stays hoisted as a top-level block. Short/plain
  // narration would instead fold into the step group as a thought.
  const SUB_A =
    "Reconstructed the turn from persisted messages and matched each tool result to its assistant call so render order stays stable — see `foldTurns`.";
  const SUB_B =
    "The `grep` sweep confirmed the helper is only referenced from the step group and its own tests, so widening the window is safe to land here.";

  it("hoists substantive prose into standalone segments, splitting the steps around it", () => {
    const items: TurnItem[] = [
      { kind: "prose", text: SUB_A, key: "a1" },
      { kind: "step", step: step("c1", "view") },
      { kind: "prose", text: SUB_B, key: "a2" },
      { kind: "step", step: step("c2", "grep") },
    ];
    expect(segmentTurn(items).map(tag)).toEqual([
      `prose:${SUB_A}`,
      "steps[1]:step",
      `prose:${SUB_B}`,
      "steps[1]:step",
    ]);
  });

  it("keeps an iteration's reasoning contiguous with its steps in one segment", () => {
    const items: TurnItem[] = [
      { kind: "prose", text: SUB_A, key: "a1" },
      { kind: "reasoning", text: "thinking", key: "a1" },
      { kind: "step", step: step("c1") },
      { kind: "step", step: step("c2") },
    ];
    const segs = segmentTurn(items);
    expect(segs.map(tag)).toEqual([
      `prose:${SUB_A}`,
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

  it("interleaves multiple substantive prose blocks with their step groups in order", () => {
    const items: TurnItem[] = [
      { kind: "prose", text: SUB_A, key: "a1" },
      { kind: "step", step: step("c1") },
      { kind: "prose", text: SUB_B, key: "a2" },
      { kind: "step", step: step("c2") },
      { kind: "prose", text: SUB_A, key: "a3" },
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

  it("keeps a steps segment's key stable when reasoning streams in after the first step (#629)", () => {
    const beforeReasoning: TurnItem[] = [{ kind: "step", step: step("c1") }];
    const afterReasoning: TurnItem[] = [
      { kind: "reasoning", text: "thinking", key: "a1" },
      { kind: "step", step: step("c1") },
    ];
    expect(segmentTurn(afterReasoning)[0].key).toBe(
      segmentTurn(beforeReasoning)[0].key,
    );
  });
});

describe("isSubstantiveProse (#687)", () => {
  it("treats very short prose as a thought regardless of content", () => {
    expect(isSubstantiveProse("Let me check the helper.")).toBe(false); // 24ch
    expect(isSubstantiveProse("Alright, moving on.")).toBe(false); // short, no signal
  });

  it("surfaces short prose with a conclusion signal (#818)", () => {
    // Bold text = always a finding, regardless of length.
    expect(isSubstantiveProse("**Cannot simply delete.**")).toBe(true);
    // Declarative starters surface short conclusions.
    expect(isSubstantiveProse("Found the bug: X was null.")).toBe(true);
    expect(isSubstantiveProse("The fix is a one-line role check.")).toBe(true);
    expect(isSubstantiveProse("Both CRs submitted.")).toBe(true);
    expect(isSubstantiveProse("Created PR #814.")).toBe(true);
    expect(isSubstantiveProse("Cannot delete — role is active.")).toBe(true);
    // But operational noise stays folded even if short.
    expect(isSubstantiveProse("Let me check the fix.")).toBe(false);
  });

  it("treats an operational-prefixed medium chunk as a thought", () => {
    const t =
      "Wait — the integration test expects the row to still render its label after the toggle, so the assertion has to move below the click.";
    expect(t.length).toBeGreaterThanOrEqual(120);
    expect(isSubstantiveProse(t)).toBe(false); // starts with "Wait"
  });

  it("treats a formatted medium chunk as substantive", () => {
    const t =
      "Critical finding: **model selection is the backend source of truth**, so the composer must gate on the resolved caps rather than the active connection.";
    expect(t.length).toBeGreaterThanOrEqual(120);
    expect(t.length).toBeLessThan(350);
    expect(isSubstantiveProse(t)).toBe(true); // has **bold**
  });

  it("treats a long unformatted chunk as substantive", () => {
    const t = "word ".repeat(80).trim(); // ~399ch, no formatting, no op prefix
    expect(t.length).toBeGreaterThanOrEqual(350);
    expect(isSubstantiveProse(t)).toBe(true);
  });

  it("counts paragraph breaks as formatting", () => {
    const t =
      "Verified the reader path against the persisted transcript.\n\nThe reconstructed steps line up with the live model, so reload renders identically.";
    expect(t.length).toBeGreaterThanOrEqual(120);
    expect(isSubstantiveProse(t)).toBe(true); // \n\n
  });
});

describe("segmentTurn thought folding (#687)", () => {
  function step(callId: string, tool = "bash"): ToolStep {
    return { callId, tool, args: {}, status: "done" };
  }

  it("folds short/operational prose into the surrounding steps segment as a thought", () => {
    const items: TurnItem[] = [
      { kind: "prose", text: "Now let me verify.", key: "a1" },
      { kind: "step", step: step("c1") },
    ];
    const segs = segmentTurn(items);
    expect(segs).toHaveLength(1);
    expect(segs[0].kind).toBe("steps");
    const steps = segs[0] as Extract<
      ReturnType<typeof segmentTurn>[number],
      { kind: "steps" }
    >;
    expect(steps.items.map((it) => it.kind)).toEqual(["thought", "step"]);
    // The demoted thought carries the prose text and does not count as a step.
    expect(steps.steps).toHaveLength(1);
  });

  it("still hoists a substantive chunk in the same turn", () => {
    const substantive =
      "The `foldTurns` reconstruction matched every persisted tool result to its assistant call, so the reloaded turn renders in the original order.";
    const items: TurnItem[] = [
      { kind: "prose", text: "Let me look.", key: "a1" },
      { kind: "step", step: step("c1") },
      { kind: "prose", text: substantive, key: "a2" },
      { kind: "step", step: step("c2") },
    ];
    const kinds = segmentTurn(items).map((s) => s.kind);
    // thought folds into the first steps group; substantive prose splits the turn.
    expect(kinds).toEqual(["steps", "prose", "steps"]);
  });
});

describe("foldTurns — mode-switch marker filtering (#848)", () => {
  it("hides user-role mode-switch markers from the UI", () => {
    const groups = foldTurns([
      msg({ role: "user", content: "Plan this for me" }),
      msg({
        role: "assistant",
        content: "I'm in Plan mode. Switch to Act to execute.",
      }),
      // Mode-switch marker injected by set_session_mode (#848)
      msg({
        role: "user",
        content: "[system: Mode switched to Act. Full tool access enabled.]",
      }),
      msg({ role: "user", content: "Go" }),
    ]);
    // Only the real user messages appear; the marker is hidden.
    const userGroups = groups.filter((g) => g.kind === "user");
    expect(userGroups).toHaveLength(2); // "Plan this for me" + "Go"
    expect(userGroups[0].kind === "user" && userGroups[0].message.content).toBe(
      "Plan this for me",
    );
    expect(userGroups[1].kind === "user" && userGroups[1].message.content).toBe(
      "Go",
    );
  });

  it("does not filter regular user messages starting with [", () => {
    const groups = foldTurns([
      msg({ role: "user", content: "[code block] here is some text" }),
    ]);
    expect(groups).toHaveLength(1);
    expect(groups[0].kind).toBe("user");
  });
});

describe("lastTurnStart + split-fold equivalence (#1022)", () => {
  // A transcript exercising every branch of the fold: multi-block turns with
  // interleaved prose/reasoning/steps (#619), a mode-switch marker boundary (#848),
  // leading orphan tool rows before an assistant, and a run with no assistant (#331).
  const transcript: Message[] = [
    msg({ role: "tool", content: "seeded orphan" }), // leading loose (#331)
    msg({ role: "user", content: "first" }),
    msg({
      id: "a1",
      role: "assistant",
      content: "checking",
      toolCalls: [call("c1", "view", { path: "x" })],
    }),
    msg({ role: "tool", toolCallId: "c1", content: "42 lines" }),
    msg({
      id: "a2",
      role: "assistant",
      content: "the answer",
      reasoning: "hmm",
    }),
    msg({ role: "user", content: "[system: mode switch]" }), // marker boundary (#848)
    msg({ role: "user", content: "second" }),
    msg({ id: "a3", role: "assistant", content: "streaming tail" }),
  ];
  const liveSteps: Record<string, ToolStep[]> = {
    a3: [
      { callId: "c9", tool: "grep", args: {}, status: "running", result: "" },
    ],
  };
  const liveReasoning: Record<string, string> = { a3: "live thinking" };

  it("lastTurnStart returns the last user index (incl. mode-switch markers)", () => {
    expect(lastTurnStart(transcript)).toBe(6); // the "second" user message
    expect(lastTurnStart([])).toBe(-1);
    expect(
      lastTurnStart([msg({ role: "assistant", content: "no user" })]),
    ).toBe(-1);
  });

  it("split fold equals whole fold at every user boundary", () => {
    const whole = foldTurns(transcript, liveSteps, liveReasoning);
    // Every user index is a valid split point; the render must be identical.
    for (let i = 0; i < transcript.length; i++) {
      if (transcript[i].role !== "user") continue;
      const split = [
        ...foldTurns(transcript.slice(0, i), liveSteps, liveReasoning),
        ...foldTurns(transcript.slice(i), liveSteps, liveReasoning),
      ];
      expect(split).toEqual(whole);
    }
  });

  it("splitting at lastTurnStart matches the whole fold (the prefix/tail seam)", () => {
    const b = lastTurnStart(transcript);
    const split = [
      ...foldTurns(transcript.slice(0, b), liveSteps, liveReasoning),
      ...foldTurns(transcript.slice(b), liveSteps, liveReasoning),
    ];
    expect(split).toEqual(foldTurns(transcript, liveSteps, liveReasoning));
  });
});
