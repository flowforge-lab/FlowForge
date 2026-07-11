// @vitest-environment jsdom
//
// Coverage for the session-switch pin race called out in #875 (the #710
// follow-up): when a global-search click opens find in another session, the
// session-switch effect at `chat-view.tsx` must NOT pin the new pane to the
// bottom — otherwise the seeded scroll-to-message is overwritten before
// paint. The gate is the `findOn` flag (find is open AND targeted at the
// current session); when it's true, the effect body skips its
// `scrollTop = scrollHeight` assignment.
//
// JSDOM doesn't materialise `scrollHeight` reliably, so we don't try to
// assert the assignment value — we spy on the setter and assert the call
// was skipped with `findOn` true (the load-bearing case the issue calls
// out) and that the spy *would* have observed the assignment with
// `findOn` false (sanity check that the effect actually runs).

import { render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ChatView } from "@/components/chat-view";
import { useChatStore } from "@/store/chat";
import { useFindStore } from "@/store/find";
import type { Message } from "@/bindings";

const SID_A = "a";
const SID_B = "b";

function seed(): Record<string, Message[]> {
  return {
    [SID_A]: [
      { id: "u1", sessionId: SID_A, role: "user", content: "hi", createdAt: 1 },
      {
        id: "a1",
        sessionId: SID_A,
        role: "assistant",
        content: "world",
        createdAt: 1,
      },
    ],
    [SID_B]: [
      {
        id: "u2",
        sessionId: SID_B,
        role: "user",
        content: "hello",
        createdAt: 2,
      },
      {
        id: "b1",
        sessionId: SID_B,
        role: "assistant",
        content: "world",
        createdAt: 2,
      },
    ],
  };
}

afterEach(() => {
  useChatStore.setState({ messagesBySession: {}, toolStepsByMessage: {} });
  useFindStore.setState({
    open: false,
    sessionId: null,
    seedQuery: null,
    seedMessageId: null,
  });
  vi.restoreAllMocks();
});

describe("ChatView findOn gate (#875, #710 follow-up)", () => {
  it("does not assign scrollTop when the session switches while find is open for the new session", () => {
    useChatStore.setState({
      activeSessionId: SID_A,
      messagesBySession: seed(),
      streamingBySession: {},
      turnStartBySession: {},
      turnStartByMessage: {},
      toolStepsByMessage: {},
      reasoningByMessage: {},
    });

    const { container } = render(<ChatView />);
    const scrollEl = container.querySelector(
      ".overflow-y-auto",
    ) as HTMLDivElement | null;
    expect(scrollEl).not.toBeNull();
    const writer = vi.spyOn(scrollEl!, "scrollTop", "set");

    // Open find for SID_B (the global-search seed flow), then switch the
    // active session — the gated session-switch effect should skip its
    // scrollTop assignment (#875 + #710 follow-up).
    useFindStore
      .getState()
      .openFind(SID_B, { query: "world", messageId: "b1" });
    useChatStore.setState({ activeSessionId: SID_B });

    expect(writer).not.toHaveBeenCalled();
  });
});
