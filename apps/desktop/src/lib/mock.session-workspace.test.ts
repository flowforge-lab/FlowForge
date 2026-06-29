import { describe, expect, it } from "vitest";

import { MockIpc } from "./mock";
import type { SessionWorkspace } from "@/bindings";

describe("MockIpc session workspace (#200, #211)", () => {
  it("returns the default workspace for an unset session", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    const ws = await ipc.getSessionWorkspace(s.id);
    expect(ws.path).toMatch(/projects\/flowforge$/);
    expect(ws.gitBranch).toBeNull();
  });

  it("round-trips a set workspace and trims it", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    const returned = await ipc.setSessionWorkspace(s.id, "  /tmp/project  ");
    expect(returned).toBe("/tmp/project");
    expect((await ipc.getSessionWorkspace(s.id)).path).toBe("/tmp/project");
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
    expect((await ipc.getSessionWorkspace(a.id)).path).toBe("/tmp/a");
    // b is untouched and still reports the default.
    expect((await ipc.getSessionWorkspace(b.id)).path).toMatch(
      /projects\/flowforge$/,
    );
  });
});

// The mock has no filesystem, so `GitHeadWatcher` is simulated: a successful
// `setSessionWorkspace` emits `workspace:branch-changed` on the next macrotask
// with a synthetic branch derived from the path (#561). This verifies the
// dev/mock trigger the live `pnpm dev:mock` click-through relies on.
describe("MockIpc workspace:branch-changed (#561)", () => {
  it("emits a synthetic branch after setSessionWorkspace", async () => {
    const ipc = new MockIpc();
    const events: SessionWorkspace[] = [];
    await ipc.onWorkspaceBranchChanged((e) => events.push(e));

    await ipc.setSessionWorkspace("sess", "/tmp/my-proj");

    // emitBranchChanged defers to a macrotask so the store's `set` runs `load`
    // (which resets the branch) before the reactive patch lands.
    await new Promise((r) => setTimeout(r, 0));
    expect(events).toEqual([
      { path: "/tmp/my-proj", gitBranch: "mock-my-proj" },
    ]);
  });

  it("does not emit when the path is rejected (empty path)", async () => {
    const ipc = new MockIpc();
    const events: SessionWorkspace[] = [];
    await ipc.onWorkspaceBranchChanged((e) => events.push(e));

    await expect(ipc.setSessionWorkspace("sess", "   ")).rejects.toThrow();
    await new Promise((r) => setTimeout(r, 0));
    expect(events).toEqual([]);
  });
});
