// @vitest-environment jsdom

import { render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ChatView } from "@/components/chat-view";
import { useChatStore } from "@/store/chat";
import type { Message } from "@/bindings";

const SID = "s1";

function userMsg(id: string): Message {
  return { id, sessionId: SID, role: "user", content: "hi", createdAt: 0 };
}

type ChatStatePartial = Partial<ReturnType<typeof useChatStore.getState>>;

function seed(partial: ChatStatePartial = {}) {
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: { [SID]: [userMsg("u1")] },
    streamingBySession: {},
    turnStartBySession: {},
    turnStartByMessage: {},
    toolStepsByMessage: {},
    ...partial,
  });
}

describe("ChatView thinking indicator", () => {
  beforeEach(() => seed({}));
  afterEach(() => useChatStore.setState({ messagesBySession: {} }));

  it("shows the thinking indicator while the turn is pending (no token yet)", () => {
    seed({ turnStartBySession: { [SID]: 123 } });
    render(<ChatView />);
    expect(screen.getByRole("status", { name: "Thinking" })).toBeTruthy();
  });

  it("hides the indicator once the turn streams", () => {
    seed({
      turnStartBySession: { [SID]: 123 },
      streamingBySession: { [SID]: "a1" },
    });
    render(<ChatView />);
    expect(screen.queryByRole("status", { name: "Thinking" })).toBeNull();
  });

  it("shows no indicator for an idle session", () => {
    seed({});
    render(<ChatView />);
    expect(screen.queryByRole("status", { name: "Thinking" })).toBeNull();
  });
});
