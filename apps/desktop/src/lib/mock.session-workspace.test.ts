import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";

describe("MockIpc session workspace (#200)", () => {
  it("returns the default workspace for an unset session", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    const path = await ipc.getSessionWorkspace(s.id);
    expect(path).toMatch(/projects\/flowforge$/);
  });

  it("round-trips a set workspace and trims it", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    const returned = await ipc.setSessionWorkspace(s.id, "  /tmp/project  ");
    expect(returned).toBe("/tmp/project");
    expect(await ipc.getSessionWorkspace(s.id)).toBe("/tmp/project");
  });

  it("rejects an empty path", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    await expect(ipc.setSessionWorkspace(s.id, "   ")).rejects.toThrow(
      "cannot resolve directory",
    );
  });

  it("keeps workspaces isolated per session", async () => {
    const ipc = new MockIpc();
    const a = await ipc.createSession();
    const b = await ipc.createSession();
    await ipc.setSessionWorkspace(a.id, "/tmp/a");
    expect(await ipc.getSessionWorkspace(a.id)).toBe("/tmp/a");
    // b is untouched and still reports the default.
    expect(await ipc.getSessionWorkspace(b.id)).toMatch(/projects\/flowforge$/);
  });
});
