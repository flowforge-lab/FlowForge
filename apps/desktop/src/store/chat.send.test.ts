import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";

const ACTIVE = "active-session";
const BG = "background-session";

describe("chat store — session-scoped send/cancel (#148)", () => {
  beforeEach(() => {
    useChatStore.setState({
      sessions: [],
      activeSessionId: ACTIVE,
      messagesBySession: {},
      streamingBySession: {},
      turnStartBySession: {},
      turnStartByMessage: {},
      toolStepsByMessage: {},
      sessionTitles: {},
      bootstrapError: null,
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("send defaults to the active session", async () => {
    const spy = vi.spyOn(ipc, "sendMessage").mockResolvedValue("m1");
    await useChatStore.getState().send("hi");
    expect(spy).toHaveBeenCalledWith(ACTIVE, "hi");
  });

  it("send marks the turn pending (started, not yet streaming) - the send-button spinner window", async () => {
    vi.spyOn(ipc, "sendMessage").mockResolvedValue("m1");
    await useChatStore.getState().send("hi");
    const s = useChatStore.getState();
    // Turn timing is set the moment we send; streaming only flips on the first
    // token. The input bar shows a spinner precisely in this window.
    expect(s.turnStartBySession[ACTIVE]).toBeTypeOf("number");
    expect(s.streamingBySession[ACTIVE]).toBeUndefined();
  });

  it("send targets an explicit background session without touching the active one", async () => {
    const spy = vi.spyOn(ipc, "sendMessage").mockResolvedValue("m2");
    await useChatStore.getState().send("bg work", BG);
    expect(spy).toHaveBeenCalledWith(BG, "bg work");
    expect(useChatStore.getState().activeSessionId).toBe(ACTIVE);
    expect(useChatStore.getState().messagesBySession[BG]).toHaveLength(1);
    expect(useChatStore.getState().messagesBySession[ACTIVE]).toBeUndefined();
  });

  it("send is a no-op while the target session is already streaming", async () => {
    useChatStore.setState({ streamingBySession: { [BG]: "m9" } });
    const spy = vi.spyOn(ipc, "sendMessage").mockResolvedValue("x");
    await useChatStore.getState().send("ignored", BG);
    expect(spy).not.toHaveBeenCalled();
  });

  it("cancelTurn cancels a specific session's streaming turn", async () => {
    useChatStore.setState({
      streamingBySession: { [ACTIVE]: "ma", [BG]: "mb" },
    });
    const spy = vi.spyOn(ipc, "cancelTurn").mockResolvedValue();
    await useChatStore.getState().cancelTurn(BG);
    expect(spy).toHaveBeenCalledWith(BG);
    expect(useChatStore.getState().streamingBySession).toEqual({
      [ACTIVE]: "ma",
    });
  });

  it("cancelTurn cancels a turn still in the pending window (pre-first-token)", async () => {
    // No streaming yet, only turn timing — the gap a Stop press lands in for a
    // cold model. The backend registered the token at send time, so cancel must
    // still fire and clear the timing.
    useChatStore.setState({ turnStartBySession: { [BG]: 123 } });
    const spy = vi.spyOn(ipc, "cancelTurn").mockResolvedValue();
    await useChatStore.getState().cancelTurn(BG);
    expect(spy).toHaveBeenCalledWith(BG);
    expect(useChatStore.getState().turnStartBySession).toEqual({});
  });

  it("cancelTurn is a no-op for an idle session (no turn in flight)", async () => {
    const spy = vi.spyOn(ipc, "cancelTurn").mockResolvedValue();
    await useChatStore.getState().cancelTurn(BG);
    expect(spy).not.toHaveBeenCalled();
  });

  it("cancelActiveTurn delegates to the active session", async () => {
    useChatStore.setState({ streamingBySession: { [ACTIVE]: "ma" } });
    const spy = vi.spyOn(ipc, "cancelTurn").mockResolvedValue();
    await useChatStore.getState().cancelActiveTurn();
    expect(spy).toHaveBeenCalledWith(ACTIVE);
    expect(useChatStore.getState().streamingBySession).toEqual({});
  });

  it("setActiveSession sets the active id without pulling history", () => {
    const spy = vi.spyOn(ipc, "getMessages").mockResolvedValue([]);
    useChatStore.getState().setActiveSession(BG);
    expect(useChatStore.getState().activeSessionId).toBe(BG);
    expect(spy).not.toHaveBeenCalled();
  });

  it("loadSession pulls history without changing the active session", async () => {
    const spy = vi.spyOn(ipc, "getMessages").mockResolvedValue([
      {
        id: "m1",
        sessionId: BG,
        role: "user",
        content: "hello",
        createdAt: 1,
      },
    ]);
    await useChatStore.getState().loadSession(BG);
    expect(spy).toHaveBeenCalledWith(BG);
    expect(useChatStore.getState().activeSessionId).toBe(ACTIVE);
    expect(useChatStore.getState().messagesBySession[BG]).toHaveLength(1);
  });
});
