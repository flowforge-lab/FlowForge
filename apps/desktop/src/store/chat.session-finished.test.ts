import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useChatStore } from "@/store/chat";
import { useSessionToastStore } from "@/store/session-toast";
import { usePrefsStore } from "@/store/prefs";
import type { Session, TurnDoneEvent } from "@/bindings";

// Session-activity signals on turn completion (#703): the streaming -> idle
// transition sets a transient "done" checkmark + a completion toast, but ONLY
// for a session the user isn't currently viewing. #994 extends the toast to
// error / approval / stopped kinds, all background-only and prefs-gated.

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
  useSessionToastStore.setState({ toasts: [] });
  // Default gates on: master + all sub-toggles enabled (sound off).
  usePrefsStore.setState({
    notifications: {
      enabled: true,
      messageComplete: true,
      approvalRequests: true,
      sound: false,
    },
  });
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

    const toasts = useSessionToastStore.getState().toasts;
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
    expect(useSessionToastStore.getState().toasts).toHaveLength(0);
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

describe("chat store — expanded toast kinds (#994)", () => {
  const toasts = () => useSessionToastStore.getState().toasts;

  it("failTurn raises an error toast for a background session only", () => {
    useChatStore.getState().failTurn({ sessionId: BG, message: "boom" });
    expect(toasts()).toHaveLength(1);
    expect(toasts()[0]).toMatchObject({ sessionId: BG, kind: "error" });

    useSessionToastStore.setState({ toasts: [] });
    useChatStore.getState().failTurn({ sessionId: ACTIVE, message: "boom" });
    expect(toasts()).toHaveLength(0);
  });

  it("error fires even with 'Message complete' off (master-only gate)", () => {
    usePrefsStore.setState({
      notifications: {
        enabled: true,
        messageComplete: false,
        approvalRequests: false,
        sound: false,
      },
    });
    useChatStore.getState().failTurn({ sessionId: BG, message: "boom" });
    expect(toasts()).toHaveLength(1);
    expect(toasts()[0].kind).toBe("error");
  });

  it("master switch off silences everything", () => {
    usePrefsStore.setState({
      notifications: {
        enabled: false,
        messageComplete: true,
        approvalRequests: true,
        sound: false,
      },
    });
    useChatStore.getState().failTurn({ sessionId: BG, message: "boom" });
    useChatStore.getState().finishTurn(doneEvent(BG));
    expect(toasts()).toHaveLength(0);
  });

  it("'Message complete' off suppresses the done toast", () => {
    usePrefsStore.setState({
      notifications: {
        enabled: true,
        messageComplete: false,
        approvalRequests: true,
        sound: false,
      },
    });
    useChatStore.getState().finishTurn(doneEvent(BG));
    expect(toasts()).toHaveLength(0);
  });

  it("applyApprovalRequest raises an approval toast for a background session", () => {
    // The step must be tracked (matches the action's no-op guard).
    useChatStore.setState({
      toolStepsByMessage: {
        [`${BG}-m1`]: [
          { callId: "c1", tool: "write", args: {}, status: "running" },
        ],
      },
    });
    useChatStore.getState().applyApprovalRequest({
      sessionId: BG,
      messageId: `${BG}-m1`,
      callId: "c1",
      tool: "write",
      args: {},
      safety: "write",
    });
    expect(toasts()).toHaveLength(1);
    expect(toasts()[0]).toMatchObject({ sessionId: BG, kind: "approval" });
  });

  it("approval toast is gated by 'Approval requests' and never for the active session", () => {
    useChatStore.setState({
      toolStepsByMessage: {
        [`${ACTIVE}-m1`]: [
          { callId: "c1", tool: "write", args: {}, status: "running" },
        ],
      },
    });
    // Active session: no toast even with the gate on.
    useChatStore.getState().applyApprovalRequest({
      sessionId: ACTIVE,
      messageId: `${ACTIVE}-m1`,
      callId: "c1",
      tool: "write",
      args: {},
      safety: "write",
    });
    expect(toasts()).toHaveLength(0);

    // Background but 'Approval requests' off: still silent.
    usePrefsStore.setState({
      notifications: {
        enabled: true,
        messageComplete: true,
        approvalRequests: false,
        sound: false,
      },
    });
    useChatStore.setState({
      toolStepsByMessage: {
        [`${BG}-m1`]: [
          { callId: "c1", tool: "write", args: {}, status: "running" },
        ],
      },
    });
    useChatStore.getState().applyApprovalRequest({
      sessionId: BG,
      messageId: `${BG}-m1`,
      callId: "c1",
      tool: "write",
      args: {},
      safety: "write",
    });
    expect(toasts()).toHaveLength(0);
  });

  it("applyAskRequest shares the approval toast kind", () => {
    useChatStore.setState({
      toolStepsByMessage: {
        [`${BG}-m1`]: [
          { callId: "c1", tool: "ask_user", args: {}, status: "running" },
        ],
      },
    });
    useChatStore.getState().applyAskRequest({
      sessionId: BG,
      messageId: `${BG}-m1`,
      callId: "c1",
      question: "Which file?",
      secret: false,
    });
    expect(toasts()).toHaveLength(1);
    expect(toasts()[0].kind).toBe("approval");
  });

  it("a resumable stop raises a stopped toast for a background session", async () => {
    // Empty final bubble + a structured stopReason = the resumable stop path.
    useChatStore.setState({
      messagesBySession: {
        [BG]: [
          {
            id: `${BG}-m1`,
            sessionId: BG,
            role: "assistant",
            content: "",
            createdAt: 0,
            stopReason: "toolLimit",
          },
        ],
      },
      streamingBySession: { [BG]: `${BG}-m1` },
      // Stub the refetch so the seeded resumable message is what the .then sees.
      loadSession: async () => {},
      turnStartBySession: {},
    });

    useChatStore.getState().finishTurn(doneEvent(BG));
    // Flush the loadSession().then() microtask chain.
    await Promise.resolve();
    await Promise.resolve();

    expect(useChatStore.getState().resumableBySession[BG]).toBe(true);
    const stopped = toasts().filter((t) => t.kind === "stopped");
    expect(stopped).toHaveLength(1);
    expect(stopped[0].sessionId).toBe(BG);
  });
});
