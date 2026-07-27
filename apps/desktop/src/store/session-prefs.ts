// Per-session view preferences for the sidebar (#169): which sessions are pinned
// (sorted to the top) and which are dismissed (hidden from the list). Both are
// FE-only, reversible, and lossless — dismissing hides a session, it never deletes
// data (Delete is tracked separately in #168).
//
// Persisted via `durableStorage` (#1110 follow-up), not plain `localStorage`
// directly: a WKWebView's localStorage write isn't guaranteed to have flushed
// to disk by the time the app process exits, and this state needs to survive
// a real quit. See `lib/durable-storage.ts` for the confirmed repro.

import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { durableStorage } from "@/lib/durable-storage";

const STORAGE_KEY = "ff-session-prefs";

export interface SessionPrefsState {
  /** Pinned session ids, insertion order (newest pin last). */
  pinned: string[];
  /** Dismissed (view-hidden) session ids. */
  dismissed: string[];
  /** False until `durableStorage`'s (always-async) read has landed. The
   *  sidebar must not render session rows before this flips — painting with
   *  the default `pinned: []` first is the exact symptom #1110 fixed. */
  hasHydrated: boolean;

  togglePin: (id: string) => void;
  /** Hide a session from the list (reversible). Also clears any pin. */
  dismiss: (id: string) => void;
  /** Un-hide a previously dismissed session. */
  restore: (id: string) => void;
  /** Drop all prefs for a session that no longer exists (e.g. after delete #168),
   *  so no orphaned pin/dismiss entry lingers. */
  purge: (id: string) => void;
  isPinned: (id: string) => boolean;
  isDismissed: (id: string) => boolean;
}

export const useSessionPrefsStore = create<SessionPrefsState>()(
  persist(
    (set, get) => ({
      pinned: [],
      dismissed: [],
      hasHydrated: false,

      togglePin: (id) =>
        set((s) =>
          s.pinned.includes(id)
            ? { pinned: s.pinned.filter((x) => x !== id) }
            : { pinned: [...s.pinned, id] },
        ),

      dismiss: (id) =>
        set((s) => ({
          dismissed: s.dismissed.includes(id)
            ? s.dismissed
            : [...s.dismissed, id],
          // A hidden session shouldn't keep occupying the pinned group.
          pinned: s.pinned.filter((x) => x !== id),
        })),

      restore: (id) =>
        set((s) => ({ dismissed: s.dismissed.filter((x) => x !== id) })),

      purge: (id) =>
        set((s) => ({
          pinned: s.pinned.filter((x) => x !== id),
          dismissed: s.dismissed.filter((x) => x !== id),
        })),

      isPinned: (id) => get().pinned.includes(id),
      isDismissed: (id) => get().dismissed.includes(id),
    }),
    {
      name: STORAGE_KEY,
      storage: createJSONStorage(() => durableStorage),
      // Runtime-only signal, not a real preference — always starts `false` in
      // memory each launch regardless of what a previous session persisted.
      partialize: ({ hasHydrated: _drop, ...rest }) => rest,
      // `durableStorage` is always async (unlike the plain-localStorage default
      // this replaced), so `pinned`/`dismissed` are NOT populated by the time
      // `create()` returns — this fires once the read actually lands, whether
      // it resolved to a real value, `null` (fresh install), or failed (logged
      // in `durable-storage.ts`, falls back to defaults rather than hanging).
      onRehydrateStorage: () => () => {
        useSessionPrefsStore.setState({ hasHydrated: true });
      },
    },
  ),
);
