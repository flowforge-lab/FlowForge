import { describe, expect, it } from "vitest";
import { MockIpc } from "./mock";

describe("MockIpc notebook (#871 FE-1)", () => {
  it("returns a fresh 'no kernel' snapshot for an unknown session", async () => {
    const ipc = new MockIpc();
    const s = await ipc.notebookStatus("ghost");
    expect(s).toEqual({
      sessionId: "ghost",
      hasKernel: false,
      state: null,
      kernelId: null,
      pid: null,
      executionCount: 0,
      raw: "",
    });
  });

  it("seeds a running kernel via the test hook and reports it back", async () => {
    const ipc = new MockIpc();
    const seeded = (
      ipc as unknown as {
        __seedNotebookKernel: (
          sessionId: string,
          patch: Record<string, unknown>,
        ) => Record<string, unknown>;
      }
    ).__seedNotebookKernel("s1", {
      hasKernel: true,
      state: "running",
      kernelId: "kernel-aaaa",
      pid: 4242,
      executionCount: 3,
    });
    expect(seeded.raw).toBe(
      "kernel kernel-aaaa — running; pid=4242; cells executed=3",
    );

    const observed = await ipc.notebookStatus("s1");
    expect(observed).toEqual(seeded);
    // Defensive copy: mutating the returned snapshot mustn't bleed into
    // `notebookKernels`.
    observed.executionCount = 99;
    const again = await ipc.notebookStatus("s1");
    expect(again.executionCount).toBe(3);
  });

  it("notebookStop removes the kernel (collapses to 'no kernel'), and is idempotent when none exists", async () => {
    // Matches the real backend: `KernelSupervisor::stop` does
    // `kernels.remove(session_id)`, so a `status()` call after Stop reports
    // "no kernel" for that session, never a `state: "dead"` tombstone.
    const ipc = new MockIpc();
    const seed = (
      ipc as unknown as {
        __seedNotebookKernel: (
          sessionId: string,
          patch: Record<string, unknown>,
        ) => Record<string, unknown>;
      }
    ).__seedNotebookKernel("s1", {
      hasKernel: true,
      state: "running",
      kernelId: "kernel-bbbb",
      pid: 7,
      executionCount: 2,
    });
    expect(seed.state).toBe("running");

    await ipc.notebookStop("s1");
    const after = await ipc.notebookStatus("s1");
    expect(after.hasKernel).toBe(false);
    expect(after.state).toBeNull();

    // Calling stop on a session with no kernel is a no-op (mirrors backend).
    await expect(ipc.notebookStop("ghost")).resolves.toBeUndefined();
  });

  it("notebookStop survives re-seeding (test hook reinstates a fresh live kernel)", async () => {
    const ipc = new MockIpc();
    const seeder = (
      ipc as unknown as {
        __seedNotebookKernel: (
          sessionId: string,
          patch: Record<string, unknown>,
        ) => Record<string, unknown>;
      }
    ).__seedNotebookKernel;
    seeder("s1", {
      hasKernel: true,
      state: "running",
      kernelId: "kernel-aaaa",
      pid: 1,
      executionCount: 4,
    });
    await ipc.notebookStop("s1");
    expect((await ipc.notebookStatus("s1")).hasKernel).toBe(false);

    seeder("s1", {
      hasKernel: true,
      state: "running",
      kernelId: "kernel-cccc",
      pid: 11,
      executionCount: 0,
    });
    expect((await ipc.notebookStatus("s1")).state).toBe("running");
  });

  it("a self-died kernel (seeded dead, not stopped) still reports state: 'dead'", async () => {
    // `state: "dead"` is real backend behavior for a kernel that died on its
    // own — exercised here via the seed hook directly, since `notebookStop`
    // never produces it (see the test above).
    const ipc = new MockIpc();
    (
      ipc as unknown as {
        __seedNotebookKernel: (
          sessionId: string,
          patch: Record<string, unknown>,
        ) => Record<string, unknown>;
      }
    ).__seedNotebookKernel("s1", {
      hasKernel: true,
      state: "dead",
      kernelId: "kernel-dddd",
      pid: 99,
      executionCount: 1,
    });
    const s = await ipc.notebookStatus("s1");
    expect(s.state).toBe("dead");
    expect(s.raw).toContain("— dead");
  });
});
