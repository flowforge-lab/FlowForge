import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import type { Message } from "@/bindings";

const S = "session-edit";

function msg(id: string, role: Message["role"], content: string): Message {
  return { id, sessionId: S, role, content, createdAt: 1 };
}

// A small transcript: u1 -> a1 -> u2 -> a2. Editing u1 should drop a1/u2/a2.
const transcript: Message[] = [
  msg("u1", "user", "first prompt"),
  msg("a1", "assistant", "first answer"),
  msg("u2", "user", "second prompt"),
  msg("a2", "assistant", "second answer"),
];

describe("chat store — in-place editMessage (#463)", () => {
  beforeEach(() => {
    useChatStore.setState({
      sessions: [],
      activeSessionId: S,
      messagesBySession: { [S]: [...transcript] },
      streamingBySession: {},
      turnStartBySession: {},
      turnStartByMessage: {},
      toolStepsByMessage: {},
      bootstrapError: null,
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("replaces the edited message in place and truncates everything after it", async () => {
    vi.spyOn(ipc, "editMessage").mockResolvedValue("u1");
    await useChatStore.getState().editMessage(S, "u1", "edited prompt");
    const msgs = useChatStore.getState().messagesBySession[S];
    expect(msgs).toHaveLength(1);
    expect(msgs[0]).toMatchObject({ id: "u1", content: "edited prompt" });
  });

  it("forwards the edit to the backend and marks the turn pending (re-run)", async () => {
    const spy = vi.spyOn(ipc, "editMessage").mockResolvedValue("u2");
    await useChatStore.getState().editMessage(S, "u2", "revised");
    expect(spy).toHaveBeenCalledWith(S, "u2", "revised", undefined);
    const s = useChatStore.getState();
    // Editing u2 keeps u1/a1 and replaces u2, dropping a2.
    expect(s.messagesBySession[S].map((m) => m.id)).toEqual(["u1", "a1", "u2"]);
    expect(s.turnStartBySession[S]).toBeTypeOf("number");
    expect(s.streamingBySession[S]).toBeUndefined();
  });

  it("garbage-collects per-message maps for the truncated messages", async () => {
    // Seed tool steps / turn timing for the assistant messages that the edit
    // will drop (a1, a2) and one that survives (a1 is kept when editing u2).
    useChatStore.setState({
      toolStepsByMessage: {
        a1: [
          {
            callId: "c1",
            tool: "read",
            args: {},
            status: "done",
          },
        ],
        a2: [
          {
            callId: "c2",
            tool: "read",
            args: {},
            status: "done",
          },
        ],
      },
      turnStartByMessage: { a1: 1, a2: 2 },
    });
    vi.spyOn(ipc, "editMessage").mockResolvedValue("u1");
    await useChatStore.getState().editMessage(S, "u1", "edited");
    const s = useChatStore.getState();
    // Editing u1 truncates a1/u2/a2 — their orphaned entries are removed.
    expect(s.toolStepsByMessage).toEqual({});
    expect(s.turnStartByMessage).toEqual({});
  });

  it("keeps per-message maps for messages that survive the edit", async () => {
    useChatStore.setState({
      toolStepsByMessage: {
        a1: [{ callId: "c1", tool: "read", args: {}, status: "done" }],
        a2: [{ callId: "c2", tool: "read", args: {}, status: "done" }],
      },
    });
    vi.spyOn(ipc, "editMessage").mockResolvedValue("u2");
    // Editing u2 keeps u1/a1, drops a2 — a1's steps survive, a2's are GC'd.
    await useChatStore.getState().editMessage(S, "u2", "revised");
    const steps = useChatStore.getState().toolStepsByMessage;
    expect(Object.keys(steps)).toEqual(["a1"]);
  });

  it("reconciles the optimistic id with the backend's edited id", async () => {
    vi.spyOn(ipc, "editMessage").mockResolvedValue("u1-new");
    await useChatStore.getState().editMessage(S, "u1", "edited");
    expect(useChatStore.getState().messagesBySession[S][0].id).toBe("u1-new");
  });

  it("is a no-op when the message id is not in the transcript", async () => {
    const spy = vi.spyOn(ipc, "editMessage").mockResolvedValue("x");
    await useChatStore.getState().editMessage(S, "missing", "nope");
    expect(spy).not.toHaveBeenCalled();
    expect(useChatStore.getState().messagesBySession[S]).toHaveLength(4);
  });

  it("restores the original transcript and surfaces an error when the edit fails", async () => {
    vi.spyOn(ipc, "editMessage").mockRejectedValue(new Error("boom"));
    await useChatStore.getState().editMessage(S, "u1", "edited");
    const msgs = useChatStore.getState().messagesBySession[S];
    // The four original messages are restored, plus a trailing system error.
    expect(msgs.slice(0, 4).map((m) => m.id)).toEqual(["u1", "a1", "u2", "a2"]);
    expect(msgs[msgs.length - 1].role).toBe("system");
    expect(msgs[msgs.length - 1].content).toContain("Failed to edit");
    expect(useChatStore.getState().turnStartBySession[S]).toBeUndefined();
  });
});
