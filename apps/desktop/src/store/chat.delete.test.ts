// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import { useSessionPrefsStore } from "@/store/session-prefs";
import type { Session } from "@/bindings";

function session(id: string): Session {
  return {
    id,
    goal: null,
    title: null,
    summary: null,
    status: "active",
    createdAt: 0,
    updatedAt: 0,
  };
}

function seed(sessions: Session[], active: string | null) {
  useChatStore.setState({
    sessions,
    activeSessionId: active,
    messagesBySession: Object.fromEntries(sessions.map((s) => [s.id, []])),
    streamingBySession: {},
    toolStepsByMessage: {},
    bootstrapError: null,
  });
}

describe("chat store — deleteSession (#168)", () => {
  beforeEach(() => {
    useSessionPrefsStore.setState({ pinned: [], dismissed: [] });
    // loadSession (via reconcile / selectSession) pulls history.
    vi.spyOn(ipc, "getMessages").mockResolvedValue([]);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("removes a non-active session and keeps the active one", async () => {
    seed([session("a"), session("b")], "a");
    localStorage.setItem("ff-msg-cache:b", "[]");
    const spy = vi.spyOn(ipc, "deleteSession").mockResolvedValue();

    const ok = await useChatStore.getState().deleteSession("b");

    expect(ok).toBe(true);
    expect(spy).toHaveBeenCalledWith("b");
    expect(useChatStore.getState().sessions.map((s) => s.id)).toEqual(["a"]);
    expect(useChatStore.getState().activeSessionId).toBe("a");
    expect(useChatStore.getState().messagesBySession.b).toBeUndefined();
    expect(localStorage.getItem("ff-msg-cache:b")).toBeNull();
  });

  it("reassigns the active session when the active one is deleted", async () => {
    seed([session("a"), session("b")], "a");
    vi.spyOn(ipc, "deleteSession").mockResolvedValue();

    await useChatStore.getState().deleteSession("a");

    expect(useChatStore.getState().sessions.map((s) => s.id)).toEqual(["b"]);
    expect(useChatStore.getState().activeSessionId).toBe("b");
  });

  it("rolls back when the backend rejects", async () => {
    seed([session("a"), session("b")], "a");
    localStorage.setItem("ff-msg-cache:b", "[]");
    vi.spyOn(ipc, "deleteSession").mockRejectedValue(new Error("io"));

    const ok = await useChatStore.getState().deleteSession("b");

    expect(ok).toBe(false);
    expect(useChatStore.getState().sessions.map((s) => s.id)).toEqual([
      "a",
      "b",
    ]);
    expect(useChatStore.getState().activeSessionId).toBe("a");
    expect(localStorage.getItem("ff-msg-cache:b")).toBe("[]");
  });

  it("is a no-op (false) for an unknown id and never calls the backend", async () => {
    seed([session("a")], "a");
    const spy = vi.spyOn(ipc, "deleteSession").mockResolvedValue();

    const ok = await useChatStore.getState().deleteSession("ghost");

    expect(ok).toBe(false);
    expect(spy).not.toHaveBeenCalled();
  });

  it("creates a fresh session when the last one is deleted", async () => {
    seed([session("a")], "a");
    vi.spyOn(ipc, "deleteSession").mockResolvedValue();
    vi.spyOn(ipc, "createSession").mockResolvedValue(session("fresh"));

    await useChatStore.getState().deleteSession("a");

    const ids = useChatStore.getState().sessions.map((s) => s.id);
    expect(ids).toContain("fresh");
    expect(ids).not.toContain("a");
    expect(useChatStore.getState().activeSessionId).toBe("fresh");
  });

  it("purges the deleted session's sidebar prefs", async () => {
    seed([session("a"), session("b")], "a");
    useSessionPrefsStore.setState({ pinned: ["b"], dismissed: ["b"] });
    vi.spyOn(ipc, "deleteSession").mockResolvedValue();

    await useChatStore.getState().deleteSession("b");

    expect(useSessionPrefsStore.getState().pinned).toEqual([]);
    expect(useSessionPrefsStore.getState().dismissed).toEqual([]);
  });
});
