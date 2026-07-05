// Vitest setup (runs once before any test, for every environment).
//
// Node v22+ ships an experimental global `localStorage` that is `undefined`
// unless `--localstorage-file` is passed to the Node process. Vitest's jsdom
// environment creates a real jsdom `window.localStorage`, but its
// `populateGlobal` step can't overwrite Node's non-configurable experimental
// getter on `globalThis`, so `globalThis.localStorage` stays `undefined`.
// Every persisted zustand store (`store/experimental.ts`, `store/model-config.ts`,
// …) calls `localStorage.setItem` on state changes and crashes in tests.
//
// Polyfill an in-memory `localStorage` before any test module imports. Each
// `clear()`/`removeItem()` resets the backing map, matching the semantics tests
// rely on (`experimental.test.ts` calls `localStorage.clear()` in `afterEach`).
function createInMemoryStorage(): Storage {
  const store = new Map<string, string>();
  return {
    getItem: (k) => store.get(k) ?? null,
    setItem: (k, v) => {
      store.set(k, String(v));
    },
    removeItem: (k) => {
      store.delete(k);
    },
    clear: () => {
      store.clear();
    },
    key: (i) => [...store.keys()][i] ?? null,
    get length() {
      return store.size;
    },
  };
}

if (
  typeof globalThis.localStorage === "undefined" ||
  globalThis.localStorage === null
) {
  Object.defineProperty(globalThis, "localStorage", {
    value: createInMemoryStorage(),
    writable: true,
    configurable: true,
  });
}
