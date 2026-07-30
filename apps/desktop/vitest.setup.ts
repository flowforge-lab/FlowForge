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

// jsdom has no `ResizeObserver`. Components that observe layout (e.g. ChatView's
// post-layout autoscroll, #1025) construct one on mount and would crash every
// jsdom render without it. Provide a no-op default; tests that need to drive the
// callback (chat-view.autoscroll.test.tsx) overwrite this with a capturing stub.
if (typeof globalThis.ResizeObserver === "undefined") {
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
}

// jsdom has no layout, so every element reports `offsetWidth`/`offsetHeight: 0`
// — and those two properties in particular are how `@tanstack/react-virtual`
// sizes its viewport (not `getBoundingClientRect`, and `initialRect` doesn't
// rescue it: the zero measurement lands immediately and wins).
//
// A zero-height viewport windows the transcript down to *no rows*, so with
// virtualization the only render path (#1143) every suite that mounts ChatView
// would assert against an empty transcript. The dangerous half of that isn't the
// failures — it's the suites that would keep passing while testing nothing.
// Give jsdom a nominal desktop viewport globally.
//
// It's `HTMLElement.prototype`, so it applies to every element rather than just
// the scroller; nothing in this app branches on those two properties except the
// virtualizer. A test needing a different size can redefine them for its own
// duration (both are `configurable`).
//
// Second, less obvious effect: virtual-core's `measureElement` falls back to
// `element.offsetHeight` when a ResizeObserver entry has no `borderBoxSize`
// (always, in jsdom), so every *row* also measures 1000px here — well above the
// 140px `ROW_ESTIMATE_PX`. That is realistic rather than accidental: real rows
// exceed the estimate too, which is what makes `getTotalSize()` under-state a
// session and is the bug `chat-view.jump-to-latest.test.tsx` reproduces.
// Guarded on `HTMLElement` because this file also runs for node-environment
// suites, where it doesn't exist.
if (typeof HTMLElement !== "undefined") {
  for (const [prop, value] of [
    ["offsetWidth", 800],
    ["offsetHeight", 1000],
  ] as const) {
    Object.defineProperty(HTMLElement.prototype, prop, {
      configurable: true,
      get: () => value,
    });
  }
}
