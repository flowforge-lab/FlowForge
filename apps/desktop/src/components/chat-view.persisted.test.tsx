// @vitest-environment jsdom

import { render, screen, fireEvent, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ChatView } from "@/components/chat-view";
import { OUTPUT_FOLD_THRESHOLD } from "@/components/output-block";
import { useChatStore } from "@/store/chat";
import type { Message } from "@/bindings";

const SID = "s1";
const LONG = "y".repeat(OUTPUT_FOLD_THRESHOLD + 100);

function toolMsg(content: string): Message {
  return { id: "t1", sessionId: SID, role: "tool", content, createdAt: 0 };
}

function bashCall(id: string, command: string) {
  return { id, name: "bash", arguments: JSON.stringify({ command }) };
}

// A reloaded multi-step turn: an assistant message carrying tool calls + final
// answer, followed by the persisted tool result rows.
function multiStepTurn(createdAt: number[]): Message[] {
  const [aTs, t1Ts, t2Ts] = createdAt;
  return [
    { id: "u1", sessionId: SID, role: "user", content: "go", createdAt: aTs },
    {
      id: "a1",
      sessionId: SID,
      role: "assistant",
      content: "all done",
      toolCalls: [bashCall("c1", "ls"), bashCall("c2", "pwd")],
      createdAt: aTs,
    },
    {
      id: "tr1",
      sessionId: SID,
      role: "tool",
      toolCallId: "c1",
      content: "out1",
      createdAt: t1Ts,
    },
    {
      id: "tr2",
      sessionId: SID,
      role: "tool",
      toolCallId: "c2",
      content: "out2",
      createdAt: t2Ts,
    },
  ];
}

/** The "N steps" fold header (the only button carrying aria-expanded). */
function groupHeader(container: HTMLElement): HTMLElement | null {
  return container.querySelector("button[aria-expanded]");
}

function seed(messages: Message[]) {
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: { [SID]: messages },
    streamingBySession: {},
    turnStartBySession: {},
    turnStartByMessage: {},
    toolStepsByMessage: {},
    reasoningByMessage: {},
  });
}

afterEach(() => useChatStore.setState({ messagesBySession: {} }));

describe("ChatView persisted tool output (#331)", () => {
  it("renders a long persisted tool row neutral (not red) and folded, then expands", () => {
    seed([toolMsg(LONG)]);
    const { container } = render(<ChatView />);

    // Neutral, not the old destructive box.
    expect(container.querySelector(".border-destructive")).toBeNull();
    // Folded by default: no <pre> until expanded.
    expect(container.querySelector("pre")).toBeNull();

    fireEvent.click(screen.getByText("output"));
    const pre = container.querySelector("pre");
    expect(pre?.textContent).toBe(LONG); // full content, never truncated
    expect(pre?.className).toContain("max-h-64");
  });

  it("shows short persisted tool output inline", () => {
    seed([toolMsg("quick result")]);
    const { container } = render(<ChatView />);
    expect(container.querySelector("pre")?.textContent).toBe("quick result");
    expect(container.querySelector(".border-destructive")).toBeNull();
  });
});

describe("ChatView reloaded multi-step turns (#413)", () => {
  it("folds a reloaded turn into one 'N steps · duration' header, foldable on expand", () => {
    seed(multiStepTurn([1000, 2000, 9000]));
    const { container } = render(<ChatView />);

    const header = groupHeader(container);
    expect(header).not.toBeNull();
    expect(header!.textContent).toContain("2 steps");
    expect(header!.textContent).toContain("8s"); // 9000 − 1000

    // Collapsed by default once settled: the per-tool rows aren't shown yet.
    expect(within(container).queryByText("Run `ls`")).toBeNull();

    fireEvent.click(header!);
    // Expanding reveals both independently-foldable tool steps.
    expect(within(container).getByText("Run `ls`")).toBeTruthy();
    expect(within(container).getByText("Run `pwd`")).toBeTruthy();
  });

  it("hides the duration when persisted timestamps are absent", () => {
    seed(multiStepTurn([0, 0, 0]));
    const { container } = render(<ChatView />);
    const header = groupHeader(container);
    expect(header!.textContent).toContain("2 steps");
    expect(header!.textContent).not.toContain("·");
  });

  it("keeps a single-step historical turn bare (no group header)", () => {
    seed([
      { id: "u1", sessionId: SID, role: "user", content: "go", createdAt: 1 },
      {
        id: "a1",
        sessionId: SID,
        role: "assistant",
        content: "done",
        toolCalls: [bashCall("c1", "ls")],
        createdAt: 1,
      },
      {
        id: "tr1",
        sessionId: SID,
        role: "tool",
        toolCallId: "c1",
        content: "out",
        createdAt: 2,
      },
    ]);
    const { container } = render(<ChatView />);
    // No fold header; the lone step renders directly.
    expect(groupHeader(container)).toBeNull();
    expect(within(container).getByText("Run `ls`")).toBeTruthy();
  });
});

describe("ChatView intermediate prose — top-level, split groups (#619, supersedes #415)", () => {
  // Substantive intermediate prose (#687): long, unformatted, no operational prefix,
  // so each chunk hoists to a top-level block (splitting the steps) rather than folding
  // into the step group as a thought. Kept unformatted so the Markdown renderer emits a
  // single plain text node the DOM assertions can match exactly.
  const PROSE_A =
    "Read the target file from top to bottom and confirmed its layout matches what the reconstruction expects, so the remaining work is limited to wiring the new classifier into the existing segmentation path without disturbing how reloaded turns are folded back into the render model that the live stream and the persisted path already share across every open pane today.";
  const PROSE_B =
    "Searched the whole component tree for other readers of this data and found none beyond the step group itself, which means the change stays contained to that one file and its unit tests and does not ripple outward into the shared inter-process contract or the generated type bindings that the two engineers deliberately keep frozen for the duration of this milestone.";
  function interleavedTurn(): Message[] {
    return [
      { id: "u1", sessionId: SID, role: "user", content: "go", createdAt: 1 },
      {
        id: "a1",
        sessionId: SID,
        role: "assistant",
        content: PROSE_A,
        toolCalls: [bashCall("c1", "ls")],
        createdAt: 1,
      },
      {
        id: "tr1",
        sessionId: SID,
        role: "tool",
        toolCallId: "c1",
        content: "out1",
        createdAt: 2,
      },
      {
        id: "a2",
        sessionId: SID,
        role: "assistant",
        content: PROSE_B,
        toolCalls: [bashCall("c2", "pwd")],
        createdAt: 2,
      },
      {
        id: "tr2",
        sessionId: SID,
        role: "tool",
        toolCallId: "c2",
        content: "out2",
        createdAt: 3,
      },
      {
        id: "a3",
        sessionId: SID,
        role: "assistant",
        content: "Done.",
        createdAt: 3,
      },
    ];
  }

  it("hoists prose to top-level blocks and splits the steps into per-iteration groups", () => {
    seed(interleavedTurn());
    const { container } = render(<ChatView />);
    const q = within(container);

    // The turn splits into two collapsed "1 step" groups (not one "2 steps").
    const headers = Array.from(
      container.querySelectorAll<HTMLElement>("button[aria-expanded]"),
    ).filter((b) => /\d+\s+steps?/.test(b.textContent ?? ""));
    expect(headers).toHaveLength(2);
    for (const h of headers) {
      expect(h.getAttribute("aria-expanded")).toBe("false");
      expect(h.textContent).toContain("1 step");
    }

    // Intermediate prose is visible at top level WITHOUT expanding any group…
    expect(q.getByText(PROSE_A)).toBeTruthy();
    expect(q.getByText(PROSE_B)).toBeTruthy();
    // …while the steps stay folded until their group is expanded.
    expect(q.queryByText("Run `ls`")).toBeNull();
    expect(q.queryByText("Run `pwd`")).toBeNull();

    // Chronological order: prose₁ → group₁ → prose₂ → group₂.
    const body = container.textContent ?? "";
    expect(body.indexOf(PROSE_A)).toBeGreaterThanOrEqual(0);
    expect(body.indexOf(PROSE_B)).toBeGreaterThan(body.indexOf(PROSE_A));

    // Expanding each group reveals only its own step.
    fireEvent.click(headers[0]);
    expect(q.getByText("Run `ls`")).toBeTruthy();
    fireEvent.click(headers[1]);
    expect(q.getByText("Run `pwd`")).toBeTruthy();
  });
});

describe("ChatView persisted reasoning (#549 W2)", () => {
  // A reloaded single-message turn whose assistant answer carries persisted
  // chain-of-thought (#375/#558). No tool calls → the Thought block stands alone.
  function reasoningTurn(reasoning: string): Message[] {
    return [
      { id: "u1", sessionId: SID, role: "user", content: "go", createdAt: 1 },
      {
        id: "a1",
        sessionId: SID,
        role: "assistant",
        content: "all done",
        reasoning,
        createdAt: 1,
      },
    ];
  }

  it("renders a Thought block from a loaded message's reasoning, with no live event", () => {
    // seed() sets reasoningByMessage: {} — nothing streamed this session, so the
    // block can only come from the persisted Message.reasoning (the W2 contract).
    seed(reasoningTurn("plan-the-approach-7f3"));
    const { container } = render(<ChatView />);

    const q = within(container);
    expect(q.getByText("Thinking")).toBeTruthy();
    expect(q.getByText(/plan-the-approach-7f3/)).toBeTruthy();
  });

  it("lets a live reasoning event override the persisted value while streaming", () => {
    // Live accumulator present for a1 → it wins over Message.reasoning
    // (chat-view.tsx: reasoningByMessage[m.id] ?? g.reasoning).
    useChatStore.setState({
      activeSessionId: SID,
      messagesBySession: { [SID]: reasoningTurn("persisted-cot-old") },
      streamingBySession: { [SID]: "a1" },
      turnStartBySession: {},
      turnStartByMessage: {},
      toolStepsByMessage: {},
      reasoningByMessage: { a1: "live-cot-new" },
    });
    const { container } = render(<ChatView />);

    const q = within(container);
    expect(q.getByText(/live-cot-new/)).toBeTruthy();
    expect(q.queryByText(/persisted-cot-old/)).toBeNull();
  });

  it("renders persisted reasoning inside a reloaded multi-step fold", () => {
    const msgs = multiStepTurn([1000, 2000, 9000]);
    // Attach reasoning to the turn's assistant message (a1).
    msgs[1] = { ...msgs[1], reasoning: "folded-cot-9k2" };
    seed(msgs);
    const { container } = render(<ChatView />);

    // Folded by default; expand the "N steps" header to reveal the Thought block.
    fireEvent.click(groupHeader(container)!);
    const q = within(container);
    expect(q.getByText("Thinking")).toBeTruthy();
    expect(q.getByText(/folded-cot-9k2/)).toBeTruthy();
  });
});
