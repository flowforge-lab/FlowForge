// @vitest-environment jsdom

import { Profiler } from "react";
import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ChatView } from "@/components/chat-view";
import { useChatStore } from "@/store/chat";
import type { Message } from "@/bindings";

// Two independent sessions, as in a split-pane layout (#148/#1009).
const A = "sess-a";
const B = "sess-b";

function userMsg(id: string, sessionId: string): Message {
  return { id, sessionId, role: "user", content: "hi", createdAt: 0 };
}
function assistantMsg(id: string, sessionId: string): Message {
  return { id, sessionId, role: "assistant", content: "", createdAt: 1 };
}

function seed() {
  useChatStore.setState({
    activeSessionId: A,
    messagesBySession: {
      [A]: [userMsg("a-u1", A), assistantMsg("a-a1", A)],
      [B]: [userMsg("b-u1", B), assistantMsg("b-a1", B)],
    },
    streamingBySession: { [A]: "a-a1", [B]: "b-a1" },
    turnStartBySession: { [A]: 100, [B]: 100 },
    turnStartByMessage: {},
    toolStepsByMessage: {},
    reasoningByMessage: {},
  });
}

describe("ChatView session isolation (#1009)", () => {
  beforeEach(seed);
  afterEach(() => useChatStore.setState({ messagesBySession: {} }));

  it("does not re-render a pane when a *foreign* session streams a token", () => {
    let renders = 0;
    render(
      <Profiler id="chat-b" onRender={() => (renders += 1)}>
        <ChatView sessionId={B} />
      </Profiler>,
    );
    const afterMount = renders;

    // Simulate pane A streaming: each of these replaces the shared map's
    // top-level ref with a new A-keyed entry, exactly like streamingPatch /
    // applyReasoning / tool events do on every token.
    act(() => {
      useChatStore.setState((s) => ({
        turnStartByMessage: { ...s.turnStartByMessage, "a-a1": 100 },
        reasoningByMessage: { ...s.reasoningByMessage, "a-a1": "thinking…" },
        toolStepsByMessage: { ...s.toolStepsByMessage, "a-a1": [] },
      }));
    });

    // Pane B owns none of those message ids, so its scoped selectors are
    // shallow-equal and the pane must not have re-rendered.
    expect(renders).toBe(afterMount);
  });

  it("still re-renders the pane when its *own* session streams (positive control)", () => {
    let renders = 0;
    render(
      <Profiler id="chat-b" onRender={() => (renders += 1)}>
        <ChatView sessionId={B} />
      </Profiler>,
    );
    const afterMount = renders;

    act(() => {
      useChatStore.setState((s) => ({
        reasoningByMessage: { ...s.reasoningByMessage, "b-a1": "thinking…" },
      }));
    });

    expect(renders).toBeGreaterThan(afterMount);
  });
});
