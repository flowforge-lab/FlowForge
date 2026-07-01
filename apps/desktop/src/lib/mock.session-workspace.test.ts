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

// #628: the branch-switch commands. `checkoutBranch` mirrors the real command by
// routing the change through `workspace:branch-changed` (the #627 flash path), and
// `getSessionWorkspace` then reflects the new branch.
describe("MockIpc branch switch (#628)", () => {
  it("lists the offered branches", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    expect(await ipc.listBranches(s.id)).toEqual([
      "main",
      "develop",
      "feature/demo",
    ]);
  });

  it("checkoutBranch updates the workspace branch and emits branch-changed", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    await ipc.setSessionWorkspace(s.id, "/tmp/proj");
    // Drain setSessionWorkspace's own deferred branch-changed emit before we
    // subscribe, so we assert only on checkoutBranch's event.
    await new Promise((r) => setTimeout(r, 0));
    const events: SessionWorkspace[] = [];
    await ipc.onWorkspaceBranchChanged((e) => events.push(e));

    const ws = await ipc.checkoutBranch(s.id, "develop");
    expect(ws).toEqual({ path: "/tmp/proj", gitBranch: "develop" });
    // getSessionWorkspace now reflects the switch.
    expect((await ipc.getSessionWorkspace(s.id)).gitBranch).toBe("develop");

    await new Promise((r) => setTimeout(r, 0));
    expect(events).toEqual([{ path: "/tmp/proj", gitBranch: "develop" }]);
  });

  it("rejects an unknown branch and emits nothing", async () => {
    const ipc = new MockIpc();
    const s = await ipc.createSession();
    const events: SessionWorkspace[] = [];
    await ipc.onWorkspaceBranchChanged((e) => events.push(e));

    await expect(ipc.checkoutBranch(s.id, "nope")).rejects.toThrow(
      "unknown branch",
    );
    await new Promise((r) => setTimeout(r, 0));
    expect(events).toEqual([]);
    expect((await ipc.getSessionWorkspace(s.id)).gitBranch).toBeNull();
  });
});
