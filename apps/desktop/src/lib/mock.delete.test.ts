import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";

describe("MockIpc deleteSession (#168)", () => {
  it("removes the session and its transcript", async () => {
    const ipc = new MockIpc();
    const a = await ipc.createSession();
    const b = await ipc.createSession();
    await ipc.sendMessage(a.id, "hello");

    await ipc.deleteSession(a.id);

    const ids = (await ipc.listSessions()).map((s) => s.id);
    expect(ids).not.toContain(a.id);
    expect(ids).toContain(b.id);
    expect(await ipc.getMessages(a.id)).toEqual([]);
  });

  it("is a no-op for an unknown session", async () => {
    const ipc = new MockIpc();
    const a = await ipc.createSession();
    await expect(ipc.deleteSession("ghost")).resolves.toBeUndefined();
    expect((await ipc.listSessions()).map((s) => s.id)).toEqual([a.id]);
  });
});
