// @vitest-environment jsdom

import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ChatView } from "@/components/chat-view";
import { useChatStore } from "@/store/chat";
import type { Message } from "@/bindings";

const SID = "s1";

function msg(
  partial: Partial<Message> & Pick<Message, "id" | "role">,
): Message {
  return { sessionId: SID, content: "", createdAt: 1, ...partial };
}

// A settled, reloaded two-iteration turn (no live steps): each assistant message
// carries its intermediate prose in `content` + one tool call, and the final
// assistant message carries the answer. Mirrors the persisted transcript shape.
function seedMultiIterationTurn() {
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: {
      [SID]: [
        msg({ id: "u1", role: "user", content: "go" }),
        msg({
          id: "a1",
          role: "assistant",
          content: "First let me look around.",
          toolCalls: [{ id: "c1", name: "view", arguments: "{}" }],
        }),
        msg({ id: "t1", role: "tool", toolCallId: "c1", content: "result 1" }),
        msg({
          id: "a2",
          role: "assistant",
          content: "Now searching the code.",
          toolCalls: [{ id: "c2", name: "grep", arguments: "{}" }],
        }),
        msg({ id: "t2", role: "tool", toolCallId: "c2", content: "result 2" }),
        msg({ id: "a3", role: "assistant", content: "Here is the answer." }),
      ],
    },
    streamingBySession: {},
    turnStartBySession: {},
    turnStartByMessage: {},
    toolStepsByMessage: {},
    reasoningByMessage: {},
  });
}

// Collapsed "N steps" group headers (StepGroup's toggle button).
function stepHeaders(): HTMLElement[] {
  return screen
    .getAllByRole("button", { expanded: false })
    .filter((b) => /\d+\s+steps?/.test(b.textContent ?? ""));
}

describe("ChatView intermediate prose (#619)", () => {
  beforeEach(() => seedMultiIterationTurn());
  afterEach(() => {
    cleanup();
    useChatStore.setState({ messagesBySession: {} });
  });

  it("shows intermediate prose at top level without expanding any group", () => {
    render(<ChatView />);

    // Both intermediate prose blocks are visible even though the turn is settled
    // and every step group is collapsed.
    expect(screen.getByText("First let me look around.")).toBeTruthy();
    expect(screen.getByText("Now searching the code.")).toBeTruthy();

    const headers = stepHeaders();
    expect(headers.length).toBeGreaterThanOrEqual(2);
    for (const h of headers) {
      expect(h.getAttribute("aria-expanded")).toBe("false");
    }
  });

  it("renders one collapsed '1 step' group per iteration, in chronological order", () => {
    const { container } = render(<ChatView />);

    const headers = stepHeaders();
    expect(headers.length).toBe(2);
    for (const h of headers) {
      expect(h.textContent).toMatch(/1 step/);
    }

    // Chronological order: prose → group → prose → group, top to bottom.
    const order = Array.from(
      container.querySelectorAll<HTMLElement>("p, button[aria-expanded]"),
    )
      .map((el) => el.textContent ?? "")
      .filter(
        (t) =>
          t.includes("First let me look around.") ||
          t.includes("Now searching the code.") ||
          /1 step/.test(t),
      );
    const firstProse = order.findIndex((t) =>
      t.includes("First let me look around."),
    );
    const secondProse = order.findIndex((t) =>
      t.includes("Now searching the code."),
    );
    expect(firstProse).toBeGreaterThanOrEqual(0);
    expect(secondProse).toBeGreaterThan(firstProse);
  });
});
