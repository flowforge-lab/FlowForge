// @vitest-environment jsdom

import { render, screen, fireEvent } from "@testing-library/react";
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
