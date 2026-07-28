// @vitest-environment jsdom
//
// #1110 follow-up: pinning a session survived within a run but not a real app
// restart — `localStorage.getItem("ff-session-prefs")` came back empty after
// quitting and relaunching `pnpm tauri dev`, confirmed live via devtools. A
// WKWebView's localStorage write isn't guaranteed to have flushed to disk by
// the time the process exits. `durableStorage` writes through
// `@tauri-apps/plugin-store` instead when running inside Tauri, with an
// explicit `save()` after every write (no debounce to lose), and falls back to
// plain `localStorage` outside Tauri (browser / `pnpm dev:mock` / tests).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Hoisted so the (hoisted) vi.mock factory can close over the spies. `backing`
// is the fake on-disk store contents; cleared in beforeEach for full test
// isolation (the real module also memoizes one store instance per process, so
// each test resets the module graph too — see beforeEach below).
const { set, get, save, deleteKey, load, backing } = vi.hoisted(() => {
  const backing = new Map<string, string>();
  return {
    set: vi.fn(async (key: string, value: string) => {
      backing.set(key, value);
    }),
    get: vi.fn(async (key: string) => backing.get(key)),
    save: vi.fn(async () => {}),
    deleteKey: vi.fn(async (key: string) => backing.delete(key)),
    load: vi.fn(),
    backing,
  };
});

vi.mock("@tauri-apps/plugin-store", () => ({
  Store: {
    load: load.mockImplementation(async () => ({
      set,
      get,
      save,
      delete: deleteKey,
    })),
  },
}));

function setTauri(on: boolean) {
  const w = globalThis.window as { __TAURI_INTERNALS__?: unknown };
  if (on) w.__TAURI_INTERNALS__ = {};
  else delete w.__TAURI_INTERNALS__;
}

beforeEach(() => {
  localStorage.clear();
  backing.clear();
  set.mockClear();
  get.mockClear();
  save.mockClear();
  deleteKey.mockClear();
  load.mockClear();
  // The module memoizes one store instance at module scope (mirrors the real
  // app loading the store once); reset the graph so each test gets its own.
  vi.resetModules();
});

afterEach(() => setTauri(false));

describe("durableStorage outside Tauri", () => {
  it("falls back to localStorage and never touches the store plugin", async () => {
    setTauri(false);
    const { durableStorage } = await import("@/lib/durable-storage");

    await durableStorage.setItem("k", "v");
    expect(localStorage.getItem("k")).toBe("v");
    expect(await durableStorage.getItem("k")).toBe("v");
    expect(load).not.toHaveBeenCalled();

    await durableStorage.removeItem("k");
    expect(localStorage.getItem("k")).toBeNull();
  });
});

describe("durableStorage inside Tauri", () => {
  it("writes through the store plugin and saves immediately (no debounce)", async () => {
    setTauri(true);
    const { durableStorage } = await import("@/lib/durable-storage");

    await durableStorage.setItem("ff-session-prefs", "PAYLOAD");

    expect(set).toHaveBeenCalledWith("ff-session-prefs", "PAYLOAD");
    // A save() call per write is the whole point: no debounced autosave that a
    // quit shortly afterward could race past.
    expect(save).toHaveBeenCalledTimes(1);
    expect(load).toHaveBeenCalledWith("prefs.json", { autoSave: false });

    expect(await durableStorage.getItem("ff-session-prefs")).toBe("PAYLOAD");

    await durableStorage.removeItem("ff-session-prefs");
    expect(deleteKey).toHaveBeenCalledWith("ff-session-prefs");
    expect(save).toHaveBeenCalledTimes(2);
  });

  it("migrates a value still sitting in localStorage from before this switch, then clears it", async () => {
    setTauri(true);
    localStorage.setItem("ff-session-prefs", "LEGACY");
    const { durableStorage } = await import("@/lib/durable-storage");

    // Store plugin has nothing yet — falls back to the legacy localStorage
    // value and adopts it into the store so it isn't lost.
    expect(await durableStorage.getItem("ff-session-prefs")).toBe("LEGACY");
    expect(set).toHaveBeenCalledWith("ff-session-prefs", "LEGACY");
    expect(save).toHaveBeenCalledTimes(1);
    // Left behind, a stale legacy value could resurrect itself if a future
    // regression in the durable path silently fell back to it — remove it so
    // any such regression instead fails visibly (empty, logged), not quietly.
    expect(localStorage.getItem("ff-session-prefs")).toBeNull();

    // Second read hits the now-populated store directly, no repeat migration.
    set.mockClear();
    expect(await durableStorage.getItem("ff-session-prefs")).toBe("LEGACY");
    expect(set).not.toHaveBeenCalled();
  });

  it("does not migrate when neither the store nor localStorage has a value", async () => {
    setTauri(true);
    const { durableStorage } = await import("@/lib/durable-storage");

    expect(await durableStorage.getItem("missing-key")).toBeNull();
    expect(set).not.toHaveBeenCalled();
  });

  // A plugin failure collapsing to `null` is indistinguishable from "nothing
  // persisted yet" — every pin silently disappearing with no diagnostic is
  // the exact failure mode this module exists to fix, so a failure must at
  // least be logged rather than swallowed bare.
  it("logs and degrades to null/no-op when the store plugin throws, instead of swallowing bare", async () => {
    setTauri(true);
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    get.mockRejectedValueOnce(new Error("disk full"));
    set.mockRejectedValueOnce(new Error("disk full"));
    deleteKey.mockRejectedValueOnce(new Error("disk full"));
    const { durableStorage } = await import("@/lib/durable-storage");

    await expect(
      durableStorage.getItem("ff-session-prefs"),
    ).resolves.toBeNull();
    await expect(
      durableStorage.setItem("ff-session-prefs", "x"),
    ).resolves.toBeUndefined();
    await expect(
      durableStorage.removeItem("ff-session-prefs"),
    ).resolves.toBeUndefined();

    expect(errorSpy).toHaveBeenCalledTimes(3);
    errorSpy.mockRestore();
  });
});
