// @vitest-environment jsdom
//
// #1121: every persisted FE store must write through `durableStorage`, not the
// WKWebView's localStorage — a localStorage write isn't guaranteed to reach disk
// before the process exits, so a change made late in a session can be lost on
// quit (#1114 diagnosed this for pin order; the cause was never specific to it).
//
// This is the regression guard for the wiring itself: a new store that forgets
// `storage: createJSONStorage(() => durableStorage)`, or one that quietly loses
// it in a refactor, would keep passing its own tests while silently going back
// to losing writes. Here the store plugin is mocked and Tauri is faked present,
// so a store still on localStorage shows up as a missing plugin write.

import { beforeEach, describe, expect, it, vi } from "vitest";

const { set, save, backing } = vi.hoisted(() => {
  const backing = new Map<string, unknown>();
  return {
    set: vi.fn(async (key: string, value: unknown) => {
      backing.set(key, value);
    }),
    save: vi.fn(async () => {}),
    backing,
  };
});

vi.mock("@tauri-apps/plugin-store", () => ({
  Store: {
    load: vi.fn(async () => ({
      set,
      save,
      get: vi.fn(async (key: string) => backing.get(key)),
      delete: vi.fn(async () => true),
    })),
  },
}));

// The stores mirror some setters to the backend; nothing here calls those, but
// the module graph imports `ipc`, and faking Tauri present would otherwise let a
// stray call reach a real `invoke`.
vi.mock("@/lib/ipc", () => ({ ipc: new Proxy({}, { get: () => vi.fn() }) }));

beforeEach(() => {
  backing.clear();
  set.mockClear();
  save.mockClear();
  localStorage.clear();
  vi.resetModules();
  (globalThis.window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ =
    {};
});

/** Wait for the persist write (`setItem` → plugin `set` → `save`) to settle. */
const flush = () => new Promise((r) => setTimeout(r, 0));

describe("persisted stores write through durableStorage (#1121)", () => {
  it.each([
    {
      key: "ff-prefs",
      module: "@/store/prefs",
      hook: "usePrefsStore",
      change: { fontScale: 123 },
    },
    {
      key: "ff-model",
      module: "@/store/model-config",
      hook: "useModelConfigStore",
      change: { summarizationThreshold: 45678 },
    },
    {
      key: "ff-session-mode",
      module: "@/store/session-mode",
      hook: "useSessionModeStore",
      change: { modeBySession: { s1: "plan" } },
    },
    {
      key: "ff-edited-messages",
      module: "@/store/edited-messages",
      hook: "useEditedMessagesStore",
      change: { editedIds: ["m1"] },
    },
    {
      key: "ff-command-shortcuts",
      module: "@/store/command-shortcuts",
      hook: "useCommandShortcutsStore",
      change: { shortcuts: [{ id: "1", name: "ship", message: "go" }] },
    },
    {
      key: "ff-experimental",
      module: "@/store/experimental",
      hook: "useExperimentalStore",
      change: { notebookPollIntervalMs: 7000 },
    },
    {
      key: "ff-session-prefs",
      module: "@/store/session-prefs",
      hook: "useSessionPrefsStore",
      change: { pinned: ["s1"] },
    },
  ])("$key persists via the Tauri store, not localStorage", async (store) => {
    const mod = (await import(store.module)) as Record<
      string,
      { setState: (patch: unknown) => void }
    >;
    mod[store.hook].setState(store.change);
    await flush();

    expect(set.mock.calls.some(([k]) => k === store.key)).toBe(true);
    expect(save).toHaveBeenCalled();
    // The whole point: nothing lands in the WebView's own storage, which is the
    // copy that can be lost on quit.
    expect(localStorage.getItem(store.key)).toBeNull();
  });
});

// The four stores from #1134 keep their own hand-rolled read/write instead of
// the `persist` middleware — deliberately, because `persist` would drop their
// existing on-disk value (see lib/durable-json.ts for the full reason). So they
// can't be driven by `setState`; each needs the action that commits a write.
// Same guarantee asserted either way: the write reaches the Tauri store, and
// nothing is left in localStorage to be lost on quit.
interface HydratingStore {
  getState: () => Record<string, (...a: unknown[]) => unknown> & {
    hasHydrated?: boolean;
  };
}

/** Wait for a store's module-load `hydrate()` to land. Polls rather than using a
 *  fixed timer: the chain includes a dynamic `import` of the store plugin, so a
 *  single macrotask isn't a guarantee on a loaded CI box. Deliberately observes
 *  the auto-fire on module load instead of calling `hydrate()` here, so dropping
 *  that call would fail these tests. */
async function waitForHydration(store: HydratingStore): Promise<void> {
  for (let i = 0; i < 100; i++) {
    if (store.getState().hasHydrated === true) return;
    await flush();
  }
  throw new Error("store never hydrated");
}

describe("hand-rolled persisted stores write through durableStorage (#1134)", () => {
  it.each([
    {
      key: "ff-palette",
      module: "@/store/palette",
      hook: "usePaletteStore",
      act: (s: Record<string, (...a: unknown[]) => unknown>) =>
        s.pushRecent("new-session"),
    },
    {
      key: "ff-split",
      module: "@/store/split",
      hook: "useSplitStore",
      act: (s: Record<string, (...a: unknown[]) => unknown>) =>
        s.openInSplit({ kind: "text", text: "hello" }),
    },
    {
      key: "ff-file-panel",
      module: "@/store/file-panel",
      hook: "useFilePanelStore",
      act: (s: Record<string, (...a: unknown[]) => unknown>) =>
        s.openFiles("s1"),
    },
  ])("$key persists via the Tauri store, not localStorage", async (store) => {
    const mod = (await import(store.module)) as Record<string, HydratingStore>;
    // Writes are suppressed until hydration lands, precisely so defaults can't
    // be persisted over real saved state — so wait for it before acting.
    await waitForHydration(mod[store.hook]);
    await store.act(mod[store.hook].getState());
    await flush();

    expect(set.mock.calls.some(([k]) => k === store.key)).toBe(true);
    expect(save).toHaveBeenCalled();
    expect(localStorage.getItem(store.key)).toBeNull();
  });

  // The migration that matters: a value written by a build that used plain
  // localStorage is adopted on first read, not dropped. This is the case
  // `persist` cannot handle for these stores — its envelope check would read
  // `.state` off a bare object and get undefined.
  it.each([
    {
      key: "ff-palette",
      module: "@/store/palette",
      hook: "usePaletteStore",
      legacy: { recent: ["toggle-split", "new-session"] },
      expect: (s: Record<string, unknown>) =>
        expect(s.recent).toEqual(["toggle-split", "new-session"]),
    },
    {
      key: "ff-split",
      module: "@/store/split",
      hook: "useSplitStore",
      legacy: { open: true, width: 700, wrap: false, content: null },
      expect: (s: Record<string, unknown>) => {
        expect(s.open).toBe(true);
        expect(s.width).toBe(700);
        expect(s.wrap).toBe(false);
      },
    },
    {
      key: "ff-file-panel",
      module: "@/store/file-panel",
      hook: "useFilePanelStore",
      legacy: {
        openSessions: ["s1"],
        panelWidth: 500,
        treeWidth: 300,
        view: { s1: { expanded: ["src"], selectedPath: "a.ts" } },
      },
      expect: (s: Record<string, unknown>) => {
        expect([...(s.openSessions as Set<string>)]).toEqual(["s1"]);
        expect(s.panelWidth).toBe(500);
        expect(s.treeWidth).toBe(300);
        const by = s.bySession as Record<
          string,
          { expanded: Set<string>; selectedPath: string | null }
        >;
        expect([...by.s1.expanded]).toEqual(["src"]);
        expect(by.s1.selectedPath).toBe("a.ts");
      },
    },
  ])("$key adopts a legacy localStorage value", async (store) => {
    localStorage.setItem(store.key, JSON.stringify(store.legacy));

    const mod = (await import(store.module)) as Record<string, HydratingStore>;
    await waitForHydration(mod[store.hook]);

    store.expect(mod[store.hook].getState() as Record<string, unknown>);
    // Adopted into the durable store and cleared from the volatile one, so a
    // later regression can't silently resurrect stale state.
    expect(backing.get(store.key)).toBe(JSON.stringify(store.legacy));
    expect(localStorage.getItem(store.key)).toBeNull();
  });

  // `panes` is separate on both counts: its read happens inside `init` (which
  // reconciles the persisted tree against live sessions rather than restoring it
  // blindly), and `init` also pulls each restored pane's transcript through the
  // chat store — stubbed out here, since the `ipc` mock above has no messages to
  // return and this file is about storage, not bootstrap.
  async function loadPanesWithStubbedChat() {
    const { useChatStore } = await import("@/store/chat");
    useChatStore.setState({
      loadSession: async () => {},
      setActiveSession: () => {},
    });
    return (await import("@/store/panes")).usePanesStore;
  }

  it("ff-panes persists via the Tauri store, not localStorage", async () => {
    const usePanesStore = await loadPanesWithStubbedChat();
    await usePanesStore.getState().init(["s1"], "s1");

    expect(set.mock.calls.some(([k]) => k === "ff-panes")).toBe(true);
    expect(save).toHaveBeenCalled();
    expect(localStorage.getItem("ff-panes")).toBeNull();
  });

  it("ff-panes adopts a legacy localStorage layout through init", async () => {
    const legacy = {
      root: {
        type: "split",
        id: "sp",
        dir: "vertical",
        children: [
          { type: "leaf", id: "l1", sessionId: "s1" },
          { type: "leaf", id: "l2", sessionId: "s2" },
        ],
        ratios: [0.5, 0.5],
      },
      focusedPaneId: "l2",
    };
    localStorage.setItem("ff-panes", JSON.stringify(legacy));

    const usePanesStore = await loadPanesWithStubbedChat();
    await usePanesStore.getState().init(["s1", "s2"], "s1");

    expect(usePanesStore.getState().leafCount()).toBe(2);
    expect(usePanesStore.getState().focusedPaneId).toBe("l2");
    expect(localStorage.getItem("ff-panes")).toBeNull();
  });
});
