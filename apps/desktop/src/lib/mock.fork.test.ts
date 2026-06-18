import { afterEach, describe, expect, it, vi } from "vitest";

import { MockIpc } from "./mock";

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe("MockIpc forkSession (#149)", () => {
  it("clones the transcript into a new session with fresh message ids", async () => {
    // Freeze the assistant stream so only the persisted user message exists.
    vi.useFakeTimers();
    const ipc = new MockIpc();
    const src = await ipc.createSession();
    await ipc.sendMessage(src.id, "first");
    await ipc.sendMessage(src.id, "second");

    const forked = await ipc.forkSession(src.id);
    expect(forked.id).not.toBe(src.id);

    const srcMsgs = await ipc.getMessages(src.id);
    const forkedMsgs = await ipc.getMessages(forked.id);
    expect(forkedMsgs.map((m) => m.content)).toEqual(
      srcMsgs.map((m) => m.content),
    );
    // Cloned: same content, new ids, re-keyed to the forked session.
    expect(forkedMsgs.every((m, i) => m.id !== srcMsgs[i].id)).toBe(true);
    expect(forkedMsgs.every((m) => m.sessionId === forked.id)).toBe(true);
  });

  it("suffixes a titled session and leaves the source untouched", async () => {
    const ipc = new MockIpc();
    const src = await ipc.createSession();
    await ipc.renameSession(src.id, "Design doc");

    const forked = await ipc.forkSession(src.id);
    expect(forked.title).toBe("Design doc (copy)");
    expect((await ipc.getMessages(forked.id)).length).toBe(0);

    // Forked transcript is independent of the source.
    await ipc.sendMessage(forked.id, "only in the fork");
    vi.clearAllTimers();
    expect(await ipc.getMessages(src.id)).toHaveLength(0);
  });

  it("rejects forking an unknown session", async () => {
    const ipc = new MockIpc();
    await expect(ipc.forkSession("ghost")).rejects.toThrow("unknown session");
  });
});
