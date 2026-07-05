import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useChatStore } from "@/store/chat";
import { useSessionDoneToastStore } from "@/store/session-done-toast";
import type { Session, TurnDoneEvent } from "@/bindings";

// Session-activity signals on turn completion (#703): the streaming -> idle
// transition sets a transient "done" checkmark + a completion toast, but ONLY
// for a session the user isn't currently viewing.

const BG = "background-session";
const ACTIVE = "active-session";

function session(id: string, title: string | null): Session {
  return {
    id,
    goal: null,
    title,
    summary: null,
    status: "active",
    createdAt: 0,
    updatedAt: 0,
  };
}

function doneEvent(sessionId: string): TurnDoneEvent {
  return {
    sessionId,
    messageId: `${sessionId}-m1`,
    tokenCount: null,
  } as TurnDoneEvent;
}

beforeEach(() => {
  vi.useFakeTimers();
  useChatStore.setState({
    sessions: [session(BG, "Parser cleanup"), session(ACTIVE, "Active work")],
    activeSessionId: ACTIVE,
    // Seed a non-empty transcript so finishTurn's empty-turn refetch path is
    // skipped — we're only exercising the activity signals here.
    messagesBySession: {
      [BG]: [
        {
          id: `${BG}-m1`,
          sessionId: BG,
          role: "assistant",
          content: "done",
          createdAt: 0,
        },
      ],
    },
    streamingBySession: { [BG]: `${BG}-m1` },
    recentlyFinishedBySession: {},
  });
  useSessionDoneToastStore.setState({ toasts: [] });
});

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe("chat store — session activity signals (#703)", () => {
  it("flags a background session finished and raises one toast", () => {
    useChatStore.getState().finishTurn(doneEvent(BG));

    const state = useChatStore.getState();
    expect(state.recentlyFinishedBySession[BG]).toBeGreaterThan(0);
    // Streaming was cleared as part of the same transition.
    expect(state.streamingBySession[BG]).toBeUndefined();

    const toasts = useSessionDoneToastStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toMatchObject({ sessionId: BG, title: "Parser cleanup" });
  });

  it("does NOT flag or toast the active session (result already on screen)", () => {
    useChatStore.setState({ streamingBySession: { [ACTIVE]: `${ACTIVE}-m1` } });
    useChatStore.setState({
      messagesBySession: {
        [ACTIVE]: [
          {
            id: `${ACTIVE}-m1`,
            sessionId: ACTIVE,
            role: "assistant",
            content: "done",
            createdAt: 0,
          },
        ],
      },
    });

    useChatStore.getState().finishTurn(doneEvent(ACTIVE));

    expect(
      useChatStore.getState().recentlyFinishedBySession[ACTIVE],
    ).toBeUndefined();
    expect(useSessionDoneToastStore.getState().toasts).toHaveLength(0);
  });

  it("auto-clears the checkmark after the TTL", () => {
    useChatStore.getState().finishTurn(doneEvent(BG));
    expect(useChatStore.getState().recentlyFinishedBySession[BG]).toBeDefined();

    vi.advanceTimersByTime(8000);

    expect(
      useChatStore.getState().recentlyFinishedBySession[BG],
    ).toBeUndefined();
  });

  it("clearSessionFinished drops the entry and cancels the pending timer", () => {
    useChatStore.getState().finishTurn(doneEvent(BG));
    useChatStore.getState().clearSessionFinished(BG);
    expect(
      useChatStore.getState().recentlyFinishedBySession[BG],
    ).toBeUndefined();

    // Timer was cancelled: advancing past the TTL must not throw or resurrect.
    vi.advanceTimersByTime(8000);
    expect(
      useChatStore.getState().recentlyFinishedBySession[BG],
    ).toBeUndefined();
  });
});
