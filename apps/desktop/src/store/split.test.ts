// @vitest-environment jsdom
//
// Persisted split-panel state, and specifically what happens when the blob on
// disk predates the code reading it.
//
// The crash this pins: a profile that still holds `{ kind: "files" }` — the
// workspace file browser variant, moved out of `SplitContent` by #944 — opened
// the panel, fell through split-panel.tsx's exhaustive `switch` to its default
// branch, and returned the object as a React child. React throws on that, and
// because the panel renders above the pane tree the error boundary replaced the
// entire window with "FlowForge hit a UI error". `open` is persisted too, so it
// recurred on every launch until storage was cleared by hand.
//
// Seeded through `localStorage` rather than the mocked plugin store because
// that is the path a real affected profile takes: `durableStorage` adopts a
// same-key legacy value on first read (#1134), so this exercises adoption and
// validation together — the combination an upgrading user actually hits.
//
// The other half of the guard — the `switch` fallback itself — is pinned in
// `components/split-panel.test.tsx`. Testing the parser is not testing the
// renderer; both gates need their own mutant.

import { beforeEach, describe, expect, it, vi } from "vitest";

const { set, save, get, backing } = vi.hoisted(() => {
  const backing = new Map<string, unknown>();
  return {
    set: vi.fn(async (key: string, value: unknown) => {
      backing.set(key, value);
    }),
    save: vi.fn(async () => {}),
    get: vi.fn(async (key: string) => backing.get(key)),
    backing,
  };
});

vi.mock("@tauri-apps/plugin-store", () => ({
  Store: {
    load: vi.fn(async () => ({
      set,
      save,
      get,
      delete: vi.fn(async () => true),
    })),
  },
}));

vi.mock("@/lib/ipc", () => ({ ipc: new Proxy({}, { get: () => vi.fn() }) }));

const STORAGE_KEY = "ff-split";

interface SplitStoreShape {
  hasHydrated: boolean;
  open: boolean;
  width: number;
  wrap: boolean;
  content: unknown;
}

const flush = () => new Promise((r) => setTimeout(r, 0));

/** Seed storage, load a fresh module, and wait for its module-load `hydrate()`
 *  — the same path a launch takes. */
async function hydrateWith(persisted: unknown): Promise<SplitStoreShape> {
  vi.resetModules();
  backing.clear();
  localStorage.clear();
  localStorage.setItem(STORAGE_KEY, JSON.stringify(persisted));
  const { useSplitStore } = await import("@/store/split");
  for (let i = 0; i < 100; i++) {
    if (useSplitStore.getState().hasHydrated) {
      return useSplitStore.getState() as unknown as SplitStoreShape;
    }
    await flush();
  }
  throw new Error("split store never hydrated");
}

beforeEach(() => {
  backing.clear();
  set.mockClear();
  save.mockClear();
  get.mockReset();
  get.mockImplementation(async (key: string) => backing.get(key));
  localStorage.clear();
  vi.resetModules();
  (globalThis.window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ =
    {};
});

describe("split panel persistence (#944 fallout)", () => {
  it("drops a persisted content kind this build cannot render", async () => {
    // Exactly the pre-#944 blob found on a dogfood machine.
    const s = await hydrateWith({
      open: true,
      width: 480,
      wrap: true,
      content: { kind: "files" },
    });

    // Nulled, so the panel shows its empty state instead of handing an unknown
    // object to the renderer.
    expect(s.content).toBeNull();
    // The rest of the panel's state is still honoured — the user's open panel
    // and width shouldn't be reset just because its payload went stale.
    expect(s.open).toBe(true);
    expect(s.width).toBe(480);
  });

  it("drops a well-known kind whose payload is malformed", async () => {
    // `code` with no text.
    expect(
      (await hydrateWith({ open: true, content: { kind: "code", lang: "ts" } }))
        .content,
    ).toBeNull();
    // `code` with no lang.
    expect(
      (await hydrateWith({ open: true, content: { kind: "code", text: "x" } }))
        .content,
    ).toBeNull();
    // Not an object at all.
    expect(
      (await hydrateWith({ open: true, content: "not an object" })).content,
    ).toBeNull();
  });

  it("still restores content it can render", async () => {
    const s = await hydrateWith({
      open: true,
      width: 600,
      wrap: false,
      content: { kind: "code", lang: "rust", text: "fn main() {}" },
    });

    expect(s.content).toEqual({
      kind: "code",
      lang: "rust",
      text: "fn main() {}",
    });
    expect(s.wrap).toBe(false);
    expect(s.width).toBe(600);
  });

  it("restores text content", async () => {
    const s = await hydrateWith({
      open: true,
      content: { kind: "text", text: "output" },
    });

    expect(s.content).toEqual({ kind: "text", text: "output" });
  });
});
