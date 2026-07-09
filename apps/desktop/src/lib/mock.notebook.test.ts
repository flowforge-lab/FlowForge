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

  it("notebookStop flips a running kernel to dead and is idempotent when no kernel exists", async () => {
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
    expect(after.state).toBe("dead");
    expect(after.raw).toContain("— dead");

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
    expect((await ipc.notebookStatus("s1")).state).toBe("dead");

    seeder("s1", {
      hasKernel: true,
      state: "running",
      kernelId: "kernel-cccc",
      pid: 11,
      executionCount: 0,
    });
    expect((await ipc.notebookStatus("s1")).state).toBe("running");
  });
});
