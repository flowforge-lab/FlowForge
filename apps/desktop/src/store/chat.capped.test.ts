import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import type { Message } from "@/bindings";

const SID = "s-cap";

const assistant = (id: string, content: string): Message => ({
  id,
  sessionId: SID,
  role: "assistant",
  content,
  createdAt: 1,
});

const done = (messageId: string) => ({
  sessionId: SID,
  messageId,
  tokenCount: null,
});

describe("chat store — capped-turn detection (#513)", () => {
  beforeEach(() => {
    useChatStore.setState({
      messagesBySession: {},
      streamingBySession: {},
      turnStartBySession: {},
      cappedBySession: {},
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("flags the session when a turn ends empty and the refetched final message is a stop notice", async () => {
    // The capped finalizer leaves no streamed content; the notice only arrives on
    // a history re-pull.
    vi.spyOn(ipc, "getMessages").mockResolvedValue([
      assistant("m1", "[stopped: reached tool-call limit]"),
    ]);
    useChatStore.getState().finishTurn(done("m1"));
    // finishTurn schedules an async loadSession; let it settle.
    await vi.waitFor(() =>
      expect(useChatStore.getState().cappedBySession[SID]).toBe(true),
    );
  });

  it("does NOT flag when the turn produced real content", async () => {
    const getMessages = vi.spyOn(ipc, "getMessages");
    useChatStore.setState({
      messagesBySession: { [SID]: [assistant("m1", "Here is the answer.")] },
    });
    useChatStore.getState().finishTurn(done("m1"));
    await Promise.resolve();
    expect(useChatStore.getState().cappedBySession[SID]).toBeUndefined();
    // No empty turn → no refetch.
    expect(getMessages).not.toHaveBeenCalled();
  });

  it("does NOT flag a bare [stopped] (deliberate user cancel)", async () => {
    vi.spyOn(ipc, "getMessages").mockResolvedValue([
      assistant("m1", "[stopped]"),
    ]);
    useChatStore.getState().finishTurn(done("m1"));
    await vi.waitFor(() => expect(ipc.getMessages).toHaveBeenCalled());
    expect(useChatStore.getState().cappedBySession[SID]).toBeUndefined();
  });

  it("does NOT resurrect the flag if a new turn supersedes during the refetch (review nit)", async () => {
    vi.spyOn(ipc, "getMessages").mockResolvedValue([
      assistant("m1", "[stopped: reached tool-call limit]"),
    ]);
    useChatStore.getState().finishTurn(done("m1"));
    // The user clicks Continue / sends before the async refetch resolves — a new
    // turn is now in flight. The in-flight .then must not re-set the flag.
    useChatStore.setState({ turnStartBySession: { [SID]: Date.now() } });
    await new Promise((r) => setTimeout(r, 0)); // drain the refetch microtasks
    expect(useChatStore.getState().cappedBySession[SID]).toBeUndefined();
  });

  it("does NOT flag when a newer assistant message replaced the done one (review nit)", async () => {
    // The refetch returns a newer, real-content turn (m2) past the done message (m1),
    // e.g. a superseding turn that already finished — the done turn is no longer last.
    vi.spyOn(ipc, "getMessages").mockResolvedValue([
      assistant("m1", "[stopped: reached tool-call limit]"),
      assistant("m2", "All done — here is the result."),
    ]);
    useChatStore.getState().finishTurn(done("m1"));
    await vi.waitFor(() => expect(ipc.getMessages).toHaveBeenCalled());
    await new Promise((r) => setTimeout(r, 0));
    expect(useChatStore.getState().cappedBySession[SID]).toBeUndefined();
  });

  it("send into the session clears a prior capped flag", async () => {
    vi.spyOn(ipc, "sendMessage").mockResolvedValue("u1");
    useChatStore.setState({ cappedBySession: { [SID]: true } });
    await useChatStore.getState().send("continue", SID);
    expect(useChatStore.getState().cappedBySession[SID]).toBeUndefined();
  });
});
