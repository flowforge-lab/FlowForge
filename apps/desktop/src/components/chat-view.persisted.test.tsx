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
