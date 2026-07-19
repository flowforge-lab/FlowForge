// @vitest-environment jsdom

import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Wrap the real `foldTurns` in a spy so we can count how much of the transcript
// gets re-folded on a streamed token (#1022). The prefix — everything before the
// active turn — must be folded once at mount and never again while the tail streams.
const foldTurns = vi.hoisted(() => vi.fn());
vi.mock("@/lib/turn-groups", async () => {
  const actual =
    await vi.importActual<typeof import("@/lib/turn-groups")>(
      "@/lib/turn-groups",
    );
  foldTurns.mockImplementation(actual.foldTurns);
  return { ...actual, foldTurns };
});

import { ChatView } from "@/components/chat-view";
import { useChatStore } from "@/store/chat";
import type { Message } from "@/bindings";

const S = "sess";

function userMsg(id: string): Message {
  return { id, sessionId: S, role: "user", content: "hi", createdAt: 0 };
}
function assistantMsg(id: string, content = ""): Message {
  return { id, sessionId: S, role: "assistant", content, createdAt: 1 };
}

// Two committed turns (the prefix) + a third, actively streaming turn (the tail).
const PREFIX_LEN = 4; // [u1, a1, u2, a2]
function seed() {
  useChatStore.setState({
    activeSessionId: S,
    messagesBySession: {
      [S]: [
        userMsg("u1"),
        assistantMsg("a1", "first answer"),
        userMsg("u2"),
        assistantMsg("a2", "second answer"),
        userMsg("u3"),
        assistantMsg("a3", "streaming"),
      ],
    },
    streamingBySession: { [S]: "a3" },
    turnStartBySession: { [S]: 100 },
    turnStartByMessage: {},
    toolStepsByMessage: {},
    reasoningByMessage: {},
  });
}

// A fold call is "over the prefix" when its input includes the committed turns
// (length >= PREFIX_LEN). The per-frame tail fold only ever sees the active turn.
const foldedPrefix = () =>
  foldTurns.mock.calls.filter((c) => (c[0] as Message[]).length >= PREFIX_LEN)
    .length;

describe("ChatView incremental fold (#1022)", () => {
  beforeEach(() => {
    seed();
    foldTurns.mockClear();
  });
  afterEach(() => useChatStore.setState({ messagesBySession: {} }));

  it("does not re-fold the committed prefix when the active turn streams a token", () => {
    render(<ChatView sessionId={S} />);
    const prefixFoldsAtMount = foldedPrefix();
    expect(prefixFoldsAtMount).toBe(1); // prefix folded exactly once

    // Stream three tokens onto the active (tail) message, as applyToken does:
    // a new array with only the last message's content changed.
    for (const delta of [" one", " two", " three"]) {
      act(() => {
        useChatStore.setState((s) => {
          const msgs = s.messagesBySession[S];
          const next = msgs.map((m, i) =>
            i === msgs.length - 1 ? { ...m, content: m.content + delta } : m,
          );
          return { messagesBySession: { ...s.messagesBySession, [S]: next } };
        });
      });
    }

    // The prefix was not re-folded — only the tail re-folds per token.
    expect(foldedPrefix()).toBe(prefixFoldsAtMount);
  });
});
