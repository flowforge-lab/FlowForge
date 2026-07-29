import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import type { Message } from "@/bindings";

const SID = "s-older";

const msg = (id: string, createdAt = 1): Message => ({
  id,
  sessionId: SID,
  role: "user",
  content: id,
  createdAt,
});

const ids = (sessionId = SID) =>
  (useChatStore.getState().messagesBySession[sessionId] ?? []).map((m) => m.id);

describe("chat store — loading older history (#1143)", () => {
  beforeEach(() => {
    useChatStore.setState({
      messagesBySession: { [SID]: [msg("m-10"), msg("m-11")] },
      hasMoreBySession: {},
      loadingOlderBySession: {},
      streamingBySession: {},
      toolStepsByMessage: {},
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("prepends the page above what is held, dropping the anchor", async () => {
    // The backend returns the anchor as the last element; keeping it would
    // duplicate the row that is already rendered.
    const spy = vi
      .spyOn(ipc, "getMessagesAround")
      .mockResolvedValue([msg("m-8"), msg("m-9"), msg("m-10")]);

    await useChatStore.getState().loadOlderMessages(SID);

    expect(ids()).toEqual(["m-8", "m-9", "m-10", "m-11"]);
    // Anchored on the oldest held message, walking backwards only.
    expect(spy).toHaveBeenCalledWith(SID, "m-10", expect.any(Number), 0);
  });

  it("stops asking once a short page proves the start of history is loaded", async () => {
    vi.spyOn(ipc, "getMessagesAround").mockResolvedValue([
      msg("m-9"),
      msg("m-10"),
    ]);

    await useChatStore.getState().loadOlderMessages(SID);
    expect(useChatStore.getState().hasMoreBySession[SID]).toBe(false);

    // A further scroll to the top must not produce another request, or the top of
    // a fully-loaded transcript would fetch forever.
    const spy = vi.spyOn(ipc, "getMessagesAround");
    spy.mockClear();
    await useChatStore.getState().loadOlderMessages(SID);
    expect(spy).not.toHaveBeenCalled();
  });

  it("does not stack concurrent requests while one is in flight", async () => {
    let release: (m: Message[]) => void = () => {};
    const spy = vi.spyOn(ipc, "getMessagesAround").mockReturnValue(
      new Promise<Message[]>((resolve) => {
        release = resolve;
      }),
    );

    // A fast scroll to the top fires handleScroll many times.
    const a = useChatStore.getState().loadOlderMessages(SID);
    const b = useChatStore.getState().loadOlderMessages(SID);
    const c = useChatStore.getState().loadOlderMessages(SID);
    expect(spy).toHaveBeenCalledTimes(1);
    expect(useChatStore.getState().loadingOlderBySession[SID]).toBe(true);

    release([msg("m-9"), msg("m-10")]);
    await Promise.all([a, b, c]);
    expect(ids()).toEqual(["m-9", "m-10", "m-11"]);
    // The guard must clear, or scrollback would jam permanently after one page.
    expect(useChatStore.getState().loadingOlderBySession[SID]).toBeUndefined();
  });

  it("clears the in-flight guard when the fetch rejects", async () => {
    vi.spyOn(ipc, "getMessagesAround").mockRejectedValue(new Error("ipc down"));

    await expect(
      useChatStore.getState().loadOlderMessages(SID),
    ).rejects.toThrow("ipc down");

    // A transient IPC failure must not wedge scrollback for the session.
    expect(useChatStore.getState().loadingOlderBySession[SID]).toBeUndefined();
  });

  it("does not duplicate rows when the window was replaced mid-flight", async () => {
    let release: (m: Message[]) => void = () => {};
    vi.spyOn(ipc, "getMessagesAround").mockReturnValue(
      new Promise<Message[]>((resolve) => {
        release = resolve;
      }),
    );

    const pending = useChatStore.getState().loadOlderMessages(SID);
    // loadSession re-pulls the window while the older page is still in flight,
    // and the re-pull already contains one of the messages coming back.
    useChatStore.setState({
      messagesBySession: { [SID]: [msg("m-9"), msg("m-10"), msg("m-11")] },
    });
    release([msg("m-8"), msg("m-9"), msg("m-10")]);
    await pending;

    expect(ids()).toEqual(["m-8", "m-9", "m-10", "m-11"]);
  });

  it("counts the page without the anchor when deciding if more remains", async () => {
    // The exact boundary: one short of a full page, plus the anchor. Stripped, 199
    // proves the start of history; counted with the anchor it reads as a full 200
    // and scrollback would keep requesting a page that never comes.
    const page = Array.from({ length: 199 }, (_, i) => msg(`p-${i}`));
    vi.spyOn(ipc, "getMessagesAround").mockResolvedValue([
      ...page,
      msg("m-10"),
    ]);

    await useChatStore.getState().loadOlderMessages(SID);

    expect(useChatStore.getState().hasMoreBySession[SID]).toBe(false);
  });

  it("no-ops when nothing is held yet, since there is no anchor", async () => {
    useChatStore.setState({ messagesBySession: {} });
    const spy = vi.spyOn(ipc, "getMessagesAround");

    await useChatStore.getState().loadOlderMessages(SID);

    expect(spy).not.toHaveBeenCalled();
  });
});

describe("chat store — reaching an out-of-window message (#1143)", () => {
  beforeEach(() => {
    useChatStore.setState({
      messagesBySession: { [SID]: [msg("m-50", 50), msg("m-51", 51)] },
      hasMoreBySession: {},
      loadingOlderBySession: {},
      streamingBySession: {},
      toolStepsByMessage: {},
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("is a no-op when the message is already loaded", async () => {
    const spy = vi.spyOn(ipc, "getMessagesAround");

    await expect(
      useChatStore.getState().ensureMessageLoaded(SID, "m-50"),
    ).resolves.toBe(true);
    expect(spy).not.toHaveBeenCalled();
  });

  it("pulls the neighbourhood of a hit outside the window and merges in order", async () => {
    // A search hit from the backend's full FTS index, far above the loaded tail.
    vi.spyOn(ipc, "getMessagesAround").mockResolvedValue([
      msg("m-4", 4),
      msg("m-5", 5),
      msg("m-6", 6),
    ]);

    await expect(
      useChatStore.getState().ensureMessageLoaded(SID, "m-5"),
    ).resolves.toBe(true);

    // Two disjoint ranges merged: chronological across both, no duplicates.
    expect(ids()).toEqual(["m-4", "m-5", "m-6", "m-50", "m-51"]);
  });

  it("reports failure for an id the backend cannot resolve", async () => {
    // Deleted between search and navigation — benign, but the caller must know
    // not to attempt a scroll.
    vi.spyOn(ipc, "getMessagesAround").mockResolvedValue([]);

    await expect(
      useChatStore.getState().ensureMessageLoaded(SID, "gone"),
    ).resolves.toBe(false);
    expect(ids()).toEqual(["m-50", "m-51"]);
  });
});
