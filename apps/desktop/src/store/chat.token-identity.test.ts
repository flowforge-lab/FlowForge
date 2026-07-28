import { beforeEach, describe, expect, it } from "vitest";

import { useChatStore } from "@/store/chat";

// The store's session-keyed maps are replaced by reference on every commit, which is
// what makes zustand's Object.is selectors work. `streamingPatch` runs on EVERY token,
// though, so reallocating `streamingBySession` / `turnStartByMessage` per delta handed
// a fresh reference to every whole-map subscriber (the sidebar) on each token of any
// background session — the jank in #1122. Both maps must now be returned by reference
// once their entry is already correct, while the per-token message churn stays.

const SESSION = "s1";
const OTHER = "s2";
const MESSAGE = "m1";
const NEXT_MESSAGE = "m2";

const token = (sessionId: string, messageId: string, delta: string): void =>
  useChatStore.getState().applyToken({ sessionId, messageId, delta });

const snapshot = () => {
  const s = useChatStore.getState();
  return {
    streaming: s.streamingBySession,
    turnStarts: s.turnStartByMessage,
    messages: s.messagesBySession,
  };
};

describe("chat store — per-token map identity (#1122)", () => {
  beforeEach(() => {
    useChatStore.setState({
      messagesBySession: {},
      streamingBySession: {},
      turnStartBySession: {},
      turnStartByMessage: {},
      toolStepsByMessage: {},
    });
  });

  it("keeps streamingBySession and turnStartByMessage identical across tokens of one message", () => {
    token(SESSION, MESSAGE, "He");
    const first = snapshot();

    token(SESSION, MESSAGE, "llo");
    token(SESSION, MESSAGE, " world");
    const later = snapshot();

    expect(later.streaming).toBe(first.streaming);
    expect(later.turnStarts).toBe(first.turnStarts);
    // Values are still correct, not just stable.
    expect(later.streaming[SESSION]).toBe(MESSAGE);
    expect(later.turnStarts[MESSAGE]).toBe(first.turnStarts[MESSAGE]);
  });

  it("still replaces both maps when a new message starts streaming", () => {
    token(SESSION, MESSAGE, "hi");
    const first = snapshot();

    token(SESSION, NEXT_MESSAGE, "next");
    const later = snapshot();

    expect(later.streaming).not.toBe(first.streaming);
    expect(later.turnStarts).not.toBe(first.turnStarts);
    expect(later.streaming[SESSION]).toBe(NEXT_MESSAGE);
    expect(later.turnStarts[NEXT_MESSAGE]).toBeTypeOf("number");
  });

  it("still replaces streamingBySession when a second session starts streaming", () => {
    token(SESSION, MESSAGE, "hi");
    const first = snapshot();

    token(OTHER, NEXT_MESSAGE, "hi");
    const later = snapshot();

    expect(later.streaming).not.toBe(first.streaming);
    expect(later.streaming[SESSION]).toBe(MESSAGE);
    expect(later.streaming[OTHER]).toBe(NEXT_MESSAGE);
  });

  it("keeps both maps identical when a tool call lands on an already-streaming message", () => {
    token(SESSION, MESSAGE, "hi");
    const first = snapshot();

    useChatStore.getState().applyToolCall({
      sessionId: SESSION,
      messageId: MESSAGE,
      callId: "c1",
      tool: "bash",
      args: { command: "cargo build" },
    });
    const later = snapshot();

    expect(later.streaming).toBe(first.streaming);
    expect(later.turnStarts).toBe(first.turnStarts);
  });

  it("still replaces messagesBySession on every token (immutability is unchanged)", () => {
    token(SESSION, MESSAGE, "He");
    const first = snapshot();

    token(SESSION, MESSAGE, "llo");
    const later = snapshot();

    expect(later.messages).not.toBe(first.messages);
    expect(later.messages[SESSION][0].content).toBe("Hello");
  });
});
