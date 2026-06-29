// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
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

describe("chat store — refreshSessions (#543)", () => {
  beforeEach(() => {
    useChatStore.setState({ sessions: [], activeSessionId: null });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("re-pulls the session list so an out-of-band session appears", async () => {
    useChatStore.setState({ sessions: [session("a")] });
    // A scheduled fire created session "fired" backend-side.
    vi.spyOn(ipc, "listSessions").mockResolvedValue([
      session("fired"),
      session("a"),
    ]);

    await useChatStore.getState().refreshSessions();

    expect(useChatStore.getState().sessions.map((s) => s.id)).toEqual([
      "fired",
      "a",
    ]);
  });
});
