// Tauri-backed durable storage for zustand `persist` (#1110 follow-up).
//
// zustand's persist middleware defaults to `window.localStorage`. Confirmed
// live: pin two sessions, quit `pnpm tauri dev`, relaunch — running
// `localStorage.getItem("ff-session-prefs")` in devtools came back
// `{"pinned":[],"dismissed":[]}`. The in-JS write (`localStorage.setItem`) is
// synchronous, but a WKWebView's write-back of that value to its own on-disk
// store is not guaranteed to have completed before the process exits, so a
// quit shortly after a write can lose it.
//
// `@tauri-apps/plugin-store` persists through the Tauri backend instead — a
// JSON file in the app's data dir, written via Rust `fs`, outside the WebView
// storage engine entirely. Its own autosave is debounced too (same class of
// race), so it's disabled here in favor of an explicit `save()` after every
// write, so a write is durable by the time the call resolves.
//
// Falls back to `localStorage` outside Tauri (browser / `pnpm dev:mock` /
// tests) — `@tauri-apps/plugin-store` needs the Tauri IPC bridge, which only
// exists in the real app.

import type { StateStorage } from "zustand/middleware";

const STORE_FILE = "prefs.json";

function inTauri(): boolean {
  return (
    typeof globalThis.window !== "undefined" &&
    "__TAURI_INTERNALS__" in globalThis.window
  );
}

// One store instance (and one on-disk file) shared by every persisted zustand
// store that uses this adapter, keyed by the `name` each passes through
// `getItem`/`setItem` — same sharing model as they'd otherwise get from one
// shared `localStorage`.
let storePromise: ReturnType<typeof loadStore> | null = null;

async function loadStore() {
  const { Store } = await import("@tauri-apps/plugin-store");
  return Store.load(STORE_FILE, { autoSave: false });
}

function getStore() {
  storePromise ??= loadStore();
  return storePromise;
}

export const durableStorage: StateStorage = {
  async getItem(name) {
    if (!inTauri()) return localStorage.getItem(name);

    // A swallowed failure here is indistinguishable from "nothing persisted
    // yet" (zustand falls back to defaults either way) — every pin silently
    // disappearing with no diagnostic is exactly the failure mode this whole
    // module exists to fix. Log so it's debuggable instead.
    let value: string | undefined;
    try {
      const store = await getStore();
      value = await store.get<string>(name);
    } catch (err) {
      console.error(
        `[durableStorage] getItem("${name}") failed, falling back to defaults`,
        err,
      );
      return null;
    }
    if (value !== undefined) return value;

    // One-time migration: a value written before this switch still lives in
    // localStorage under the same key. Adopt it into the durable store so an
    // in-flight user doesn't lose their current pins on this particular
    // upgrade, then remove it — leaving it behind would let a future
    // regression in the durable path silently resurrect stale prefs instead
    // of failing visibly (empty, logged).
    const legacy = localStorage.getItem(name);
    if (legacy !== null) {
      try {
        const store = await getStore();
        await store.set(name, legacy);
        await store.save();
        localStorage.removeItem(name);
      } catch (err) {
        console.error(
          `[durableStorage] migrating legacy localStorage("${name}") failed`,
          err,
        );
      }
    }
    return legacy;
  },

  async setItem(name, value) {
    if (!inTauri()) {
      localStorage.setItem(name, value);
      return;
    }
    try {
      const store = await getStore();
      await store.set(name, value);
      await store.save();
    } catch (err) {
      console.error(`[durableStorage] setItem("${name}") failed`, err);
    }
  },

  async removeItem(name) {
    if (!inTauri()) {
      localStorage.removeItem(name);
      return;
    }
    try {
      const store = await getStore();
      await store.delete(name);
      await store.save();
    } catch (err) {
      console.error(`[durableStorage] removeItem("${name}") failed`, err);
    }
  },
};
