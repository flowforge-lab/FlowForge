// In-thread find bar (#679) open/target state. Pure frontend, ephemeral (never
// persisted). One find is active at a time, scoped to a session id: the focused
// pane's session mirrors to the chat store's `activeSessionId`, and forks are
// distinct sessions, so a single active target is unambiguous. Mirrors
// store/shortcuts.ts so app-shell's global handler can toggle it the same way.
//
// The query, match list, and active index are DOM-derived and live as component
// state in FindBar; this store only carries whether find is open and which
// session it targets.
//
// Global search (#710) can open the bar with a *seed*: a query to pre-fill and a
// specific messageId to jump to. FindBar reads the seed once on open and calls
// `consumeSeed` so a later manual search isn't re-seeded.

import { create } from "zustand";

/** Optional pre-fill when opening the bar from a global-search hit (#710). */
export interface FindSeed {
  query?: string;
  messageId?: string;
}

interface FindState {
  open: boolean;
  sessionId: string | null;
  /** Query to pre-fill on open (#710); null for a plain Cmd+F. */
  seedQuery: string | null;
  /** Message to jump to on open (#710); null to default to the first match. */
  seedMessageId: string | null;
  openFind: (sessionId: string, seed?: FindSeed) => void;
  closeFind: () => void;
  toggleFind: (sessionId: string) => void;
  /** Clear the seed after FindBar has applied it. */
  consumeSeed: () => void;
}

export const useFindStore = create<FindState>((set) => ({
  open: false,
  sessionId: null,
  seedQuery: null,
  seedMessageId: null,
  openFind: (sessionId, seed) =>
    set({
      open: true,
      sessionId,
      seedQuery: seed?.query ?? null,
      seedMessageId: seed?.messageId ?? null,
    }),
  closeFind: () => set({ open: false, seedQuery: null, seedMessageId: null }),
  toggleFind: (sessionId) =>
    set((s) =>
      s.open && s.sessionId === sessionId
        ? { open: false, seedQuery: null, seedMessageId: null }
        : { open: true, sessionId, seedQuery: null, seedMessageId: null },
    ),
  consumeSeed: () => set({ seedQuery: null, seedMessageId: null }),
}));
