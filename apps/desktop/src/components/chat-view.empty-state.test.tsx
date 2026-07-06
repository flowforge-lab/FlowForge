// @vitest-environment jsdom

// Tests for ChatView's empty-state guard (#785):
// messagesBySession[id] === undefined  → not loaded yet → render nothing (no flash)
// messagesBySession[id] === []         → genuinely empty session → show prompt

import { render, screen, cleanup } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ChatView } from "@/components/chat-view";
import { useChatStore } from "@/store/chat";
import type { Message } from "@/bindings";

const SID = "sess-flash-test";

function userMsg(content: string): Message {
  return { id: "m1", sessionId: SID, role: "user", content, createdAt: 0 };
}

afterEach(() => {
  cleanup();
  useChatStore.setState({
    activeSessionId: null,
    messagesBySession: {},
    streamingBySession: {},
    turnStartBySession: {},
    turnStartByMessage: {},
    toolStepsByMessage: {},
    reasoningByMessage: {},
  });
});

describe("ChatView empty-state guard (#785)", () => {
  it("renders nothing when messages are not loaded yet (undefined) — no cold-start flash", () => {
    // Simulate the window between app:ready and loadSession() completing:
    // activeSessionId is set but messagesBySession has no entry yet.
    useChatStore.setState({
      activeSessionId: SID,
      messagesBySession: {}, // SID key absent → selector returns undefined
    });
    const { container } = render(<ChatView />);
    expect(container.firstChild).toBeNull();
    expect(screen.queryByText("What are you working on?")).toBeNull();
  });

  it("shows the empty-state prompt when messages are loaded but the session is genuinely empty", () => {
    // Simulate a fresh draft after loadSession() resolved with [].
    useChatStore.setState({
      activeSessionId: SID,
      messagesBySession: { [SID]: [] }, // key present, empty array
    });
    render(<ChatView />);
    expect(screen.getByText("What are you working on?")).not.toBeNull();
  });

  it("renders the transcript when messages are loaded and non-empty", () => {
    useChatStore.setState({
      activeSessionId: SID,
      messagesBySession: { [SID]: [userMsg("hello")] },
    });
    render(<ChatView />);
    expect(screen.getByText("hello")).not.toBeNull();
    expect(screen.queryByText("What are you working on?")).toBeNull();
  });
});
