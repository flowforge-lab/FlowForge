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
