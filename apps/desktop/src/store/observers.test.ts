// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ipc } from "@/lib/ipc";
import type { ObserverInfo } from "@/bindings";
import { useObserversStore } from "@/store/observers";

function observer(
  partial: Partial<ObserverInfo> & { id: number },
): ObserverInfo {
  return {
    label: `obs-${partial.id}`,
    kind: "file",
    target: "src/lib.rs",
    startedAt: "2026-07-23T00:00:00Z",
    ...partial,
  };
}

describe("useObserversStore", () => {
  beforeEach(() => {
    useObserversStore.setState({ bySession: {} });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  describe("load", () => {
    it("fetches a session's observers and caches them by session", async () => {
      const list = [observer({ id: 1 }), observer({ id: 2, kind: "http" })];
      const spy = vi.spyOn(ipc, "listObservers").mockResolvedValue(list);

      await useObserversStore.getState().load("s1");

      expect(spy).toHaveBeenCalledWith("s1");
      expect(useObserversStore.getState().bySession.s1).toEqual(list);
    });

    it("replaces the cached list on reload (not appends)", async () => {
      vi.spyOn(ipc, "listObservers")
        .mockResolvedValueOnce([observer({ id: 1 })])
        .mockResolvedValueOnce([observer({ id: 2 })]);

      await useObserversStore.getState().load("s1");
      await useObserversStore.getState().load("s1");

      expect(useObserversStore.getState().bySession.s1).toEqual([
        observer({ id: 2 }),
      ]);
    });

    it("keeps sessions isolated", async () => {
      vi.spyOn(ipc, "listObservers").mockImplementation(async (sessionId) =>
        sessionId === "s1" ? [observer({ id: 1 })] : [observer({ id: 9 })],
      );

      await useObserversStore.getState().load("s1");
      await useObserversStore.getState().load("s2");

      expect(useObserversStore.getState().bySession.s1).toEqual([
        observer({ id: 1 }),
      ]);
      expect(useObserversStore.getState().bySession.s2).toEqual([
        observer({ id: 9 }),
      ]);
    });
  });

  describe("stop", () => {
    it("calls stopObserver then reloads the session", async () => {
      const stopSpy = vi.spyOn(ipc, "stopObserver").mockResolvedValue();
      // After the stop, the reload returns the reduced list.
      const listSpy = vi
        .spyOn(ipc, "listObservers")
        .mockResolvedValue([observer({ id: 2 })]);

      await useObserversStore.getState().stop(1, "s1");

      expect(stopSpy).toHaveBeenCalledWith(1, "s1");
      expect(listSpy).toHaveBeenCalledWith("s1");
      expect(useObserversStore.getState().bySession.s1).toEqual([
        observer({ id: 2 }),
      ]);
    });
  });

  describe("refresh", () => {
    it("re-lists the session (the observer:changed handler path)", async () => {
      const spy = vi
        .spyOn(ipc, "listObservers")
        .mockResolvedValue([observer({ id: 3 })]);

      await useObserversStore.getState().refresh("s1");

      expect(spy).toHaveBeenCalledWith("s1");
      expect(useObserversStore.getState().bySession.s1).toEqual([
        observer({ id: 3 }),
      ]);
    });
  });

  describe("clear", () => {
    it("drops one session's observers without touching others", () => {
      useObserversStore.setState({
        bySession: { s1: [observer({ id: 1 })], s2: [observer({ id: 2 })] },
      });

      useObserversStore.getState().clear("s1");

      expect(useObserversStore.getState().bySession.s1).toBeUndefined();
      expect(useObserversStore.getState().bySession.s2).toEqual([
        observer({ id: 2 }),
      ]);
    });

    it("is a no-op for an unknown session", () => {
      const before = { s1: [observer({ id: 1 })] };
      useObserversStore.setState({ bySession: before });

      useObserversStore.getState().clear("nope");

      expect(useObserversStore.getState().bySession).toBe(before);
    });
  });
});
