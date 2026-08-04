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

// Every store load stamps the file's schema version, so `set`/`save` each carry
// one extra call that has nothing to do with the key under test. Count only the
// calls for a real key.
const setsFor = (key: string) =>
  set.mock.calls.filter(([k]) => k === key).length;

// Hoisted so the (hoisted) vi.mock factory can close over the spies. `backing`
// is the fake on-disk store contents; cleared in beforeEach for full test
// isolation (the real module also memoizes one store instance per process, so
// each test resets the module graph too — see beforeEach below).
const { set, get, save, deleteKey, load, backing } = vi.hoisted(() => {
  // `unknown` values: the schema stamp is a number, every store blob a string.
  const backing = new Map<string, unknown>();
  return {
    set: vi.fn(async (key: string, value: unknown) => {
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
  // `mockClear` keeps whatever implementation a test installed (see
  // `failReadsFor`), so put the plain one back explicitly.
  get.mockImplementation(async (key: string) => backing.get(key));
  save.mockClear();
  // Same reason as `get` above: the drain tests gate `save` on a promise they
  // control, and `mockClear` would leave that gate in place for every test
  // after them.
  save.mockImplementation(async () => {});
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
    // quit shortly afterward could race past. Two here: the schema stamp on
    // load, then the write itself.
    expect(save).toHaveBeenCalledTimes(2);
    expect(load).toHaveBeenCalledWith("prefs.json", { autoSave: false });

    expect(await durableStorage.getItem("ff-session-prefs")).toBe("PAYLOAD");

    await durableStorage.removeItem("ff-session-prefs");
    expect(deleteKey).toHaveBeenCalledWith("ff-session-prefs");
    expect(save).toHaveBeenCalledTimes(3);
  });

  // The file's own layout version, distinct from each store's zustand `version`.
  // Stamped now so a later re-keying or split of `prefs.json` can migrate rather
  // than guess what shape it's reading.
  it("stamps the schema version on the file, once per load", async () => {
    setTauri(true);
    const { durableStorage } = await import("@/lib/durable-storage");

    await durableStorage.setItem("ff-prefs", "A");
    expect(set).toHaveBeenCalledWith("__ff_schema", 1);
    expect(setsFor("__ff_schema")).toBe(1);

    // A second operation reuses the memoized store — no re-stamp.
    await durableStorage.setItem("ff-prefs", "B");
    expect(setsFor("__ff_schema")).toBe(1);
    expect(backing.get("__ff_schema")).toBe(1);
  });

  it("migrates a value still sitting in localStorage from before this switch, then clears it", async () => {
    setTauri(true);
    localStorage.setItem("ff-session-prefs", "LEGACY");
    const { durableStorage } = await import("@/lib/durable-storage");

    // Store plugin has nothing yet — falls back to the legacy localStorage
    // value and adopts it into the store so it isn't lost.
    expect(await durableStorage.getItem("ff-session-prefs")).toBe("LEGACY");
    expect(set).toHaveBeenCalledWith("ff-session-prefs", "LEGACY");
    // Schema stamp + the adopted value.
    expect(save).toHaveBeenCalledTimes(2);
    // Left behind, a stale legacy value could resurrect itself if a future
    // regression in the durable path silently fell back to it — remove it so
    // any such regression instead fails visibly (empty, logged), not quietly.
    expect(localStorage.getItem("ff-session-prefs")).toBeNull();

    // Second read hits the now-populated store directly, no repeat migration.
    set.mockClear();
    expect(await durableStorage.getItem("ff-session-prefs")).toBe("LEGACY");
    expect(setsFor("ff-session-prefs")).toBe(0);
  });

  it("does not migrate when neither the store nor localStorage has a value", async () => {
    setTauri(true);
    const { durableStorage } = await import("@/lib/durable-storage");

    expect(await durableStorage.getItem("missing-key")).toBeNull();
    expect(setsFor("missing-key")).toBe(0);
  });

  // A plugin failure collapsing to `null` is indistinguishable from "nothing
  // persisted yet" — every pin silently disappearing with no diagnostic is
  // the exact failure mode this module exists to fix, so a failure must at
  // least be logged rather than swallowed bare.
  it("logs and degrades to null/no-op when the store plugin throws, instead of swallowing bare", async () => {
    setTauri(true);
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    failReadsFor("ff-session-prefs");
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

    expect(errorSpy).toHaveBeenCalled();
    errorSpy.mockRestore();
  });

  // A read failure is often transient (lock contention, a momentarily busy fs).
  // Losing every setting for the rest of the session over one blip isn't worth
  // it, so a failed read gets exactly one retry before the key is given up on.
  it("retries a failed read once and uses the value the retry returns", async () => {
    setTauri(true);
    backing.set("ff-prefs", "REAL");
    let failed = false;
    get.mockImplementation(async (key: string) => {
      if (key === "ff-prefs" && !failed) {
        failed = true;
        throw new Error("busy");
      }
      return backing.get(key);
    });
    const { durableStorage, storageDegraded } =
      await import("@/lib/durable-storage");

    expect(await durableStorage.getItem("ff-prefs")).toBe("REAL");
    expect(storageDegraded("ff-prefs")).toBe(false);
  });

  // The failure this guards is subtle: a failed read makes zustand hydrate
  // DEFAULTS, and the next write would then persist those defaults over the
  // value still sitting on disk — a transient error turned permanent.
  it("suppresses writes for a key whose read failed, leaving the on-disk value intact", async () => {
    setTauri(true);
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    backing.set("ff-prefs", "REAL");
    failReadsFor("ff-prefs");
    const { durableStorage, storageDegraded } =
      await import("@/lib/durable-storage");

    expect(await durableStorage.getItem("ff-prefs")).toBeNull();
    expect(storageDegraded("ff-prefs")).toBe(true);

    await durableStorage.setItem("ff-prefs", "DEFAULTS");
    await durableStorage.removeItem("ff-prefs");
    expect(backing.get("ff-prefs")).toBe("REAL");
    expect(errorSpy).toHaveBeenCalled();

    // Untouched keys keep working — one bad key doesn't freeze the whole file.
    await durableStorage.setItem("ff-experimental", "FLAGS");
    expect(backing.get("ff-experimental")).toBe("FLAGS");
    errorSpy.mockRestore();
  });

  it("clears the degraded mark once a later read succeeds", async () => {
    setTauri(true);
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    backing.set("ff-prefs", "REAL");
    const stopFailing = failReadsFor("ff-prefs");
    const { durableStorage, storageDegraded } =
      await import("@/lib/durable-storage");

    expect(await durableStorage.getItem("ff-prefs")).toBeNull();
    expect(storageDegraded("ff-prefs")).toBe(true);

    stopFailing();
    expect(await durableStorage.getItem("ff-prefs")).toBe("REAL");
    expect(storageDegraded("ff-prefs")).toBe(false);

    await durableStorage.setItem("ff-prefs", "NEW");
    expect(backing.get("ff-prefs")).toBe("NEW");
    errorSpy.mockRestore();
  });
});

// #1184: writes are issued fire-and-forget (`writeDurable` discards the promise,
// and so does zustand's `persist`), so the interval between an action returning
// and its write settling is a loss window if the app goes away inside it.
// `flushDurableWrites` is what window teardown awaits to close that window.
describe("flushDurableWrites (#1184)", () => {
  it("waits for a write that no call site awaited", async () => {
    setTauri(true);
    const { durableStorage, flushDurableWrites } =
      await import("@/lib/durable-storage");

    const release = gateSave();
    // Issued exactly the way `writeDurable` issues it: the promise is dropped.
    void durableStorage.setItem("ff-panes", "LAYOUT");
    await settle();

    let drained = false;
    const flush = flushDurableWrites().then(() => {
      drained = true;
    });
    await settle();

    // The assertion that fails if the write isn't tracked: with nothing
    // registered, the drain has nothing to wait on and resolves immediately —
    // "a drain that no mutation can detect is not a fix".
    expect(drained).toBe(false);
    expect(save).toHaveBeenCalled();

    release();
    await flush;
    expect(drained).toBe(true);
  });

  it("resolves promptly when nothing is in flight", async () => {
    setTauri(true);
    const { durableStorage, flushDurableWrites } =
      await import("@/lib/durable-storage");

    await durableStorage.setItem("ff-panes", "LAYOUT");
    await flushDurableWrites();
    expect(backing.get("ff-panes")).toBe("LAYOUT");
  });

  it("also waits for a write issued while the drain is running", async () => {
    setTauri(true);
    const { durableStorage, flushDurableWrites } =
      await import("@/lib/durable-storage");

    const releaseFirst = gateSave();
    void durableStorage.setItem("ff-panes", "FIRST");
    await settle();

    let drained = false;
    const flush = flushDurableWrites().then(() => {
      drained = true;
    });
    await settle();

    // A second write lands mid-drain — `getItem`'s legacy-adoption path can do
    // this for real. Draining one batch and stopping would walk away from it.
    const releaseSecond = gateSave();
    void durableStorage.setItem("ff-split", "SECOND");
    releaseFirst();
    await settle();
    expect(drained).toBe(false);

    releaseSecond();
    await flush;
    expect(drained).toBe(true);
    expect(backing.get("ff-split")).toBe("SECOND");
  });

  it("gives up rather than blocking the close forever", async () => {
    setTauri(true);
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const { durableStorage, flushDurableWrites } =
      await import("@/lib/durable-storage");

    // A save that never settles. `onCloseRequested` awaits this drain before
    // destroying the window, so an unbounded wait would leave the user with an
    // app they cannot close — strictly worse than losing the write.
    gateSave();
    void durableStorage.setItem("ff-panes", "LAYOUT");
    await settle();

    await flushDurableWrites(10);
    expect(errorSpy).toHaveBeenCalledWith(
      expect.stringContaining("flush timed out"),
    );
    errorSpy.mockRestore();
  });
});

/** Hold `save()` open until the returned function is called, so a test can
 *  observe the interval where a write is issued but not yet durable. */
function gateSave(): () => void {
  let release!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  save.mockImplementation(async () => {
    await gate;
  });
  return release;
}

/** Let every already-queued microtask and macrotask run. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** Make every read of `key` throw (the schema stamp and other keys keep working,
 *  so tests exercise one bad key rather than a dead store). Returns a function
 *  that restores normal reads. */
function failReadsFor(key: string): () => void {
  get.mockImplementation(async (k: string) => {
    if (k === key) throw new Error("disk full");
    return backing.get(k);
  });
  return () => {
    get.mockImplementation(async (k: string) => backing.get(k));
  };
}
