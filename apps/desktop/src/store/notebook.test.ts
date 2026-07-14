// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import type { NotebookKernelState } from "@/bindings/NotebookKernelState";
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
    useNotebookStore.setState({ bySession: {}, ipcUnavailable: false });
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
      const debugSpy = vi.spyOn(console, "debug").mockImplementation(() => {});
      await useNotebookStore.getState().hydrate("s3");
      expect(spy).toHaveBeenCalled();
      expect(useNotebookStore.getState().bySession.s3).toBeUndefined();
      expect(debugSpy).toHaveBeenCalled();
    });

    it("trips `ipcUnavailable` on the first rejection, short-circuiting later hydrate/refresh/stop calls", async () => {
      // Mirrors a real (non-mock) build before the backend PR lands: every
      // `notebook_status` invoke rejects. The first failure must stop the FE
      // from re-invoking a command that will never resolve on every other
      // session pane's mount.
      const status = vi
        .spyOn(ipc, "notebookStatus")
        .mockRejectedValue(new Error("command notebook_status not found"));
      const stopIpc = vi.spyOn(ipc, "notebookStop").mockResolvedValue();
      vi.spyOn(console, "debug").mockImplementation(() => {});

      await useNotebookStore.getState().hydrate("s1");
      expect(useNotebookStore.getState().ipcUnavailable).toBe(true);
      expect(status).toHaveBeenCalledTimes(1);

      // A second session's mount short-circuits before ever calling ipc.
      await useNotebookStore.getState().hydrate("s2");
      expect(status).toHaveBeenCalledTimes(1);

      await useNotebookStore.getState().refresh("s1");
      expect(status).toHaveBeenCalledTimes(1);

      await useNotebookStore.getState().stop("s1");
      expect(stopIpc).not.toHaveBeenCalled();

      const restartIpc = vi.spyOn(ipc, "notebookRestart");
      await useNotebookStore.getState().restart("s1");
      expect(restartIpc).not.toHaveBeenCalled();
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
      // The real backend's `stop()` removes the kernel entirely, so the
      // post-stop `notebookStatus` call reports "no kernel" — not a
      // `state: "dead"` tombstone (that's reserved for a kernel that died on
      // its own). The store just relays whatever `notebookStatus` returns.
      const stop = vi.spyOn(ipc, "notebookStop").mockResolvedValue();
      const status = vi.spyOn(ipc, "notebookStatus").mockResolvedValue(
        snapshot({
          sessionId: "s1",
          hasKernel: false,
        }),
      );

      await useNotebookStore.getState().stop("s1");

      // No kernelId forwarded when stopping the whole session.
      expect(stop).toHaveBeenCalledWith("s1", undefined);
      expect(status).toHaveBeenCalledWith("s1");
      expect(useNotebookStore.getState().bySession.s1?.hasKernel).toBe(false);
    });

    it("forwards a kernelId to stop a single kernel (#871 FE-2)", async () => {
      const stop = vi.spyOn(ipc, "notebookStop").mockResolvedValue();
      vi.spyOn(ipc, "notebookStatus").mockResolvedValue(
        snapshot({ sessionId: "s1", hasKernel: false }),
      );
      await useNotebookStore.getState().stop("s1", "kernel-bbbb");
      expect(stop).toHaveBeenCalledWith("s1", "kernel-bbbb");
    });

    it("propagates the backend rejection", async () => {
      vi.spyOn(ipc, "notebookStop").mockRejectedValue(new Error("not allowed"));
      await expect(useNotebookStore.getState().stop("s1")).rejects.toThrow(
        "not allowed",
      );
    });
  });

  describe("restart", () => {
    it("writes the post-restart snapshot straight into bySession (no extra status call)", async () => {
      // The command returns the fresh snapshot, so the store writes it directly
      // rather than round-tripping through `notebookStatus`.
      const fresh = snapshot({
        sessionId: "s1",
        hasKernel: true,
        state: "running",
        kernelId: "kernel-bbbb",
        executionCount: 0,
      });
      const restart = vi.spyOn(ipc, "notebookRestart").mockResolvedValue(fresh);
      const status = vi.spyOn(ipc, "notebookStatus");

      await useNotebookStore.getState().restart("s1", "kernel-aaaa");

      expect(restart).toHaveBeenCalledWith("s1", "kernel-aaaa");
      expect(status).not.toHaveBeenCalled();
      expect(useNotebookStore.getState().bySession.s1).toEqual(fresh);
    });

    it("propagates the backend rejection", async () => {
      vi.spyOn(ipc, "notebookRestart").mockRejectedValue(
        new Error("not allowed"),
      );
      await expect(useNotebookStore.getState().restart("s1")).rejects.toThrow(
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
