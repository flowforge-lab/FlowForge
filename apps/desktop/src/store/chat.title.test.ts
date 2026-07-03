import { beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import type { Session } from "@/bindings";

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

describe("chat store — patchSessionTitle (#671 item 2b)", () => {
  beforeEach(() => {
    useChatStore.setState({
      sessions: [session("a", "old title"), session("b", null)],
    });
  });

  it("patches the cached title in place without re-persisting via IPC", () => {
    const rename = vi.spyOn(ipc, "renameSession");
    useChatStore.getState().patchSessionTitle("a", "Fix the parser bug");

    const a = useChatStore.getState().sessions.find((s) => s.id === "a");
    expect(a?.title).toBe("Fix the parser bug");
    // Unlike setSessionTitle (a user rename), the backend already persisted this,
    // so there must be no write-back.
    expect(rename).not.toHaveBeenCalled();
  });

  it("leaves other sessions untouched", () => {
    useChatStore.getState().patchSessionTitle("b", "New chat");
    const s = useChatStore.getState().sessions;
    expect(s.find((x) => x.id === "a")?.title).toBe("old title");
    expect(s.find((x) => x.id === "b")?.title).toBe("New chat");
  });

  it("is a no-op for an unknown session", () => {
    useChatStore.getState().patchSessionTitle("ghost", "nope");
    const titles = useChatStore.getState().sessions.map((s) => s.title);
    expect(titles).toEqual(["old title", null]);
  });
});
