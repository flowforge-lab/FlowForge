// Tauri-backed durable storage for zustand `persist` (#1110 follow-up, made the
// FE-wide default in #1121).
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

/** Shape version of `prefs.json` itself — the file, not any one store's blob
 *  (those keep their own zustand `version`). Stamped on first successful load
 *  so a future layout change (re-keying, splitting the file) can migrate rather
 *  than guess what it's looking at. Bump + extend `migrateFile` together.
 *
 *  Why this exists when #1134 declined a schema version (#1121 review): that
 *  objection was to writing a `migrate()` against a single known version — a
 *  guess about a shape nobody has seen. It applies to the migration, not to the
 *  stamp, and only the stamp is built here; `migrateFile` deliberately has no
 *  per-version branch yet.
 *
 *  The stamp has to be early to be worth anything. It's written on load, so by
 *  the time a second shape exists every running install is already stamped and
 *  the first real migration can dispatch on a number. Add it *with* that
 *  migration instead and the migration still faces a population of unstamped
 *  files it has to sniff — exactly the guessing #1134 wanted to avoid. So:
 *  cheap now (one key, one write per load), removes the guess later. Don't
 *  delete it as dead weight — it is doing its job precisely by being inert. */
const SCHEMA_VERSION = 1;
const SCHEMA_KEY = "__ff_schema";

function inTauri(): boolean {
  return (
    typeof globalThis.window !== "undefined" &&
    "__TAURI_INTERNALS__" in globalThis.window
  );
}

/** Keys whose read failed twice. zustand hydrates defaults after a failed read,
 *  and the next write would then persist those defaults OVER the still-intact
 *  on-disk value — a transient read error turning into permanent data loss.
 *  Writes are suppressed for such a key until a later read succeeds. */
const degraded = new Set<string>();

/** Whether reads for `name` are currently failing, so writes are being held back
 *  to protect the on-disk value. Exported for tests and for any UI that wants to
 *  surface "your settings aren't being saved" rather than lie by omission. */
export function storageDegraded(name: string): boolean {
  return degraded.has(name);
}

// One store instance (and one on-disk file) shared by every persisted zustand
// store that uses this adapter, keyed by the `name` each passes through
// `getItem`/`setItem` — same sharing model as they'd otherwise get from one
// shared `localStorage`.
let storePromise: ReturnType<typeof loadStore> | null = null;

type PrefsStore = {
  get<T>(key: string): Promise<T | undefined>;
  set(key: string, value: unknown): Promise<void>;
  delete(key: string): Promise<boolean>;
  save(): Promise<void>;
};

async function loadStore(): Promise<PrefsStore> {
  const { Store } = await import("@tauri-apps/plugin-store");
  const store = (await Store.load(STORE_FILE, {
    autoSave: false,
  })) as unknown as PrefsStore;
  // Migrate before the memoized promise resolves, so no `getItem` can observe a
  // half-migrated file.
  await migrateFile(store);
  return store;
}

/** Bring `prefs.json` up to `SCHEMA_VERSION`. A file with no stamp is either a
 *  fresh install or one written before #1121 — both are already the v1 layout
 *  (one JSON-string blob per store key), so they're just stamped. */
async function migrateFile(store: PrefsStore): Promise<void> {
  const from = await store.get<number>(SCHEMA_KEY);
  if (from === SCHEMA_VERSION) return;
  // No shape change has shipped yet, so there is nothing to convert — an
  // unstamped file (fresh install, or written before #1121) is already the v1
  // layout: one JSON-string blob per store key. The per-version switch lands
  // with the first bump; stamping now is what makes that switch possible.
  await store.set(SCHEMA_KEY, SCHEMA_VERSION);
  await store.save();
}

function getStore() {
  storePromise ??= loadStore();
  return storePromise;
}

/** Drop the memoized store so the next call re-`load`s it. A rejected promise
 *  stays rejected forever, so without this a single failed load would make every
 *  later read/write fail too — including the retry meant to recover from it. */
function resetStore(): void {
  storePromise = null;
}

// ---------------------------------------------------------------------------
// In-flight write tracking (#1184)
// ---------------------------------------------------------------------------

/** Writes issued but not yet settled.
 *
 *  Nobody holds these promises at the call sites: `writeDurable` is
 *  fire-and-forget by design, and zustand's `persist` middleware discards
 *  `setItem`'s result too. Awaiting per call would push `await` through every
 *  store action and then every caller, for a write no action is waiting on —
 *  backpressure is the wrong shape. Keeping the handles here instead lets
 *  teardown wait for the bytes once, at the one moment it matters (#1184). */
const inFlight = new Set<Promise<void>>();

/** Hard cap on a drain. A hung `save()` must never be able to keep the window
 *  open: `onCloseRequested` awaits this before destroying, so an unbounded wait
 *  here would turn a storage stall into an app the user cannot close. Losing the
 *  write is the lesser failure, and it is the one we already have today. */
const FLUSH_TIMEOUT_MS = 2000;

function track(op: Promise<void>): Promise<void> {
  inFlight.add(op);
  void op.finally(() => inFlight.delete(op));
  return op;
}

/** Resolve once every write issued so far has settled — the drain teardown
 *  awaits (see `durable-flush.ts`).
 *
 *  Loops rather than awaiting one batch: `getItem`'s legacy-localStorage
 *  adoption writes too, so a settling drain can enqueue more work, and stopping
 *  after one pass would walk away from it.
 *
 *  Never rejects. `setItem`/`removeItem` swallow and log their own IO errors, so
 *  the tracked promises don't reject either; a timeout is logged and treated as
 *  done. */
export async function flushDurableWrites(
  timeoutMs: number = FLUSH_TIMEOUT_MS,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (inFlight.size > 0) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) {
      console.error(
        `[durableStorage] flush timed out with ${inFlight.size} write(s) still in flight`,
      );
      return;
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    const expired = new Promise<void>((resolve) => {
      timer = setTimeout(resolve, remaining);
    });
    await Promise.race([Promise.allSettled([...inFlight]), expired]);
    clearTimeout(timer);
  }
}

export const durableStorage: StateStorage = {
  async getItem(name) {
    if (!inTauri()) return localStorage.getItem(name);

    // A swallowed failure here is indistinguishable from "nothing persisted
    // yet" (zustand falls back to defaults either way) — every pin silently
    // disappearing with no diagnostic is exactly the failure mode this whole
    // module exists to fix. Log so it's debuggable instead, and mark the key
    // degraded so the defaults zustand is about to hydrate can't be written
    // back over the value that's still on disk.
    let value: string | undefined;
    try {
      value = await readOnce(name);
    } catch (first) {
      try {
        // One retry: a lock contention or a transient fs error shouldn't cost
        // the user their settings for the rest of the session. Reload the store
        // first — the failure may have been the load itself, and that promise is
        // memoized.
        resetStore();
        value = await readOnce(name);
      } catch (err) {
        console.error(
          `[durableStorage] getItem("${name}") failed twice, falling back to defaults; ` +
            `writes for this key are suppressed until a read succeeds`,
          err,
          first,
        );
        degraded.add(name);
        return null;
      }
    }
    degraded.delete(name);
    if (value !== undefined) return value;

    // One-time migration: a value written before this switch still lives in
    // localStorage under the same key. Adopt it into the durable store so an
    // in-flight user doesn't lose their current prefs on this particular
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

  // setItem/removeItem hand their work to the private functions below and
  // register the promise, so a write is drainable at teardown even though no
  // call site holds it (#1184).
  setItem(name, value) {
    return track(writeItem(name, value));
  },

  removeItem(name) {
    return track(deleteItem(name));
  },
};

async function writeItem(name: string, value: string): Promise<void> {
  if (!inTauri()) {
    localStorage.setItem(name, value);
    return;
  }
  if (degraded.has(name)) {
    console.error(
      `[durableStorage] setItem("${name}") suppressed: this key's read failed, ` +
        `so writing now would overwrite the on-disk value with defaults`,
    );
    return;
  }
  try {
    const store = await getStore();
    await store.set(name, value);
    await store.save();
  } catch (err) {
    console.error(`[durableStorage] setItem("${name}") failed`, err);
    resetStore();
  }
}

async function deleteItem(name: string): Promise<void> {
  if (!inTauri()) {
    localStorage.removeItem(name);
    return;
  }
  if (degraded.has(name)) {
    console.error(
      `[durableStorage] removeItem("${name}") suppressed: this key's read failed, ` +
        `so the on-disk value is left intact`,
    );
    return;
  }
  try {
    const store = await getStore();
    await store.delete(name);
    await store.save();
  } catch (err) {
    console.error(`[durableStorage] removeItem("${name}") failed`, err);
    resetStore();
  }
}

async function readOnce(name: string): Promise<string | undefined> {
  const store = await getStore();
  return store.get<string>(name);
}
