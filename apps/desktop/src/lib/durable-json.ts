// Read/write helpers for the stores that persist a hand-rolled JSON shape
// through `durableStorage` (#1134) instead of zustand's `persist` middleware
// (#1121): `panes`, `split`, `file-panel`, `palette`.
//
// Why those four don't use the middleware, when the other seven do:
//
// `persist` writes a `{ state, version }` envelope, and on read hands `merge`
// only the `.state` half. These four already have a bare on-disk shape — no
// envelope — written by their own `loadPersisted`/`persist` pair. A value
// already sitting on a user's disk therefore deserializes with
// `version: undefined`, which zustand reads as "no migration needed" and pulls
// `.state` from, getting `undefined` (see `zustand/esm/middleware.mjs:407`);
// `merge` never sees the legacy object at all and the default merge drops it.
// Converting them to the middleware would silently discard every existing
// user's saved layout on the upgrade — the exact data loss this whole
// conversion exists to prevent.
//
// Keeping the on-disk shape byte-identical makes the migration free instead:
// `durableStorage.getItem` already adopts and clears a same-key `localStorage`
// value, and what it adopts parses with each store's existing reader unchanged.

import { durableStorage } from "./durable-storage";

/** Read `key`, hand the parsed JSON to `parse`, and fall back to `fallback` on a
 *  missing, unparseable, or failed read. `parse` owns validation/normalization —
 *  it receives `unknown` because what's on disk was written by an older build. */
export async function readDurable<T>(
  key: string,
  parse: (raw: unknown) => T,
  fallback: T,
): Promise<T> {
  try {
    const raw = await durableStorage.getItem(key);
    if (raw === null) return fallback;
    return parse(JSON.parse(raw));
  } catch {
    // Corrupt blob, or a storage read that failed after its own retry —
    // `durableStorage` has already logged the latter. Start from defaults.
    return fallback;
  }
}

/** Write `value` under `key` without waiting for it. Callers are UI actions with
 *  nothing to do about a failure; `durableStorage` logs its own IO errors.
 *
 *  Not *dropped*, though: `durableStorage.setItem` registers the promise so
 *  window teardown can drain it (#1184). Actions stay synchronous — the wait
 *  happens once, at close, instead of in every caller. */
export function writeDurable(key: string, value: unknown): void {
  try {
    void durableStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Serialization failure only (a cycle in the value) — non-fatal.
  }
}
