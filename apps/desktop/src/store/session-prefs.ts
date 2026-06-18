// Per-session view preferences for the sidebar (#169): which sessions are pinned
// (sorted to the top) and which are dismissed (hidden from the list). Both are
// FE-only, reversible, and lossless — dismissing hides a session, it never deletes
// data (Delete is tracked separately in #168). Persisted to localStorage under
// `"ff-session-prefs"`; mirrors `store/command-shortcuts.ts`.

import { create } from "zustand";
import { persist } from "zustand/middleware";

const STORAGE_KEY = "ff-session-prefs";

export interface SessionPrefsState {
  /** Pinned session ids, insertion order (newest pin last). */
  pinned: string[];
  /** Dismissed (view-hidden) session ids. */
  dismissed: string[];

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
    { name: STORAGE_KEY },
  ),
);
