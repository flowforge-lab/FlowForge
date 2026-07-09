// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import type { NotebookKernelState } from "@/bindings";
import { useNotebookStore } from "@/store/notebook";

function snapshot(
  partial: Partial<NotebookKernelState> & { sessionId: string },
): NotebookKernelState {
  return {
    hasKernel: false,
    state: null,
    kernelId: null,
    pid: null,
    executionCount: 0,
    raw: "",
    ...partial,
  };
}

describe("useNotebookStore", () => {
  beforeEach(() => {
    useNotebookStore.setState({ bySession: {} });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  describe("hydrate", () => {
    it("upserts the latest snapshot for the session (#871 FE-1)", async () => {
      const live = snapshot({
        sessionId: "s1",
        hasKernel: true,
        state: "running",
        kernelId: "kernel-aaaa",
        executionCount: 3,
        raw: "kernel kernel-aaaa — running; pid=42; cells executed=3",
      });
      const spy = vi.spyOn(ipc, "notebookStatus").mockResolvedValue(live);

      await useNotebookStore.getState().hydrate("s1");

      expect(spy).toHaveBeenCalledWith("s1");
      expect(useNotebookStore.getState().bySession.s1).toEqual(live);
    });

    it("records `null` (no kernel) rather than skipping the session", async () => {
      // Distinguishes a never-polled session (`undefined`) from "polled and no
      // kernel" so the panel can render its quiet line instead of staying
      // blank.
      vi.spyOn(ipc, "notebookStatus").mockResolvedValue(
        snapshot({ sessionId: "s2", hasKernel: false, raw: "" }),
      );
      await useNotebookStore.getState().hydrate("s2");
      expect(useNotebookStore.getState().bySession.s2).toMatchObject({
        hasKernel: false,
      });
    });

    it("swallows an IPC failure (entry stays undefined for the next mount)", async () => {
      const spy = vi
        .spyOn(ipc, "notebookStatus")
        .mockRejectedValue(new Error("backend offline"));
      const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
      await useNotebookStore.getState().hydrate("s3");
      expect(spy).toHaveBeenCalled();
      expect(useNotebookStore.getState().bySession.s3).toBeUndefined();
      expect(errSpy).toHaveBeenCalled();
    });
  });

  describe("refresh", () => {
    it("replaces the snapshot in place", async () => {
      useNotebookStore.setState({
        bySession: { s1: snapshot({ sessionId: "s1", executionCount: 1 }) },
      });
      vi.spyOn(ipc, "notebookStatus").mockResolvedValue(
        snapshot({
          sessionId: "s1",
          hasKernel: true,
          state: "running",
          kernelId: "kernel-aaaa",
          executionCount: 2,
        }),
      );
      await useNotebookStore.getState().refresh("s1");
      expect(useNotebookStore.getState().bySession.s1?.executionCount).toBe(2);
    });

    it("drops the cached entry when the backend rejects (panel falls back to no-kernel)", async () => {
      useNotebookStore.setState({
        bySession: {
          s1: snapshot({
            sessionId: "s1",
            hasKernel: true,
            state: "running",
            kernelId: "kernel-aaaa",
          }),
        },
      });
      vi.spyOn(ipc, "notebookStatus").mockRejectedValue(
        new Error("session deleted"),
      );
      await useNotebookStore.getState().refresh("s1");
      expect(useNotebookStore.getState().bySession.s1).toBeUndefined();
    });
  });

  describe("stop", () => {
    it("calls notebookStop then refreshes the snapshot", async () => {
      const stop = vi.spyOn(ipc, "notebookStop").mockResolvedValue();
      const status = vi.spyOn(ipc, "notebookStatus").mockResolvedValue(
        snapshot({
          sessionId: "s1",
          hasKernel: true,
          state: "dead",
          kernelId: "kernel-aaaa",
        }),
      );

      await useNotebookStore.getState().stop("s1");

      expect(stop).toHaveBeenCalledWith("s1");
      expect(status).toHaveBeenCalledWith("s1");
      expect(useNotebookStore.getState().bySession.s1?.state).toBe("dead");
    });

    it("propagates the backend rejection", async () => {
      vi.spyOn(ipc, "notebookStop").mockRejectedValue(new Error("not allowed"));
      await expect(useNotebookStore.getState().stop("s1")).rejects.toThrow(
        "not allowed",
      );
    });
  });

  describe("clear", () => {
    it("removes the cached entry without IPC", () => {
      useNotebookStore.setState({
        bySession: { s1: snapshot({ sessionId: "s1" }) },
      });
      const spy = vi.spyOn(ipc, "notebookStatus");
      useNotebookStore.getState().clear("s1");
      expect(useNotebookStore.getState().bySession.s1).toBeUndefined();
      expect(spy).not.toHaveBeenCalled();
    });

    it("is a no-op when the session id has no entry", () => {
      useNotebookStore.setState({ bySession: {} });
      useNotebookStore.getState().clear("ghost");
      expect(useNotebookStore.getState().bySession).toEqual({});
    });
  });
});
