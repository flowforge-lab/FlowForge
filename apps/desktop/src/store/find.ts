// In-thread find bar (#679) open/target state. Pure frontend, ephemeral (never
// persisted). One find is active at a time, scoped to a session id: the focused
// pane's session mirrors to the chat store's `activeSessionId`, and forks are
// distinct sessions, so a single active target is unambiguous. Mirrors
// store/shortcuts.ts so app-shell's global handler can toggle it the same way.
//
// The query, match list, and active index are DOM-derived and live as component
// state in FindBar; this store only carries whether find is open and which
// session it targets.

import { create } from "zustand";

interface FindState {
  open: boolean;
  sessionId: string | null;
  openFind: (sessionId: string) => void;
  closeFind: () => void;
  toggleFind: (sessionId: string) => void;
}

export const useFindStore = create<FindState>((set) => ({
  open: false,
  sessionId: null,
  openFind: (sessionId) => set({ open: true, sessionId }),
  closeFind: () => set({ open: false }),
  toggleFind: (sessionId) =>
    set((s) =>
      s.open && s.sessionId === sessionId
        ? { open: false }
        : { open: true, sessionId },
    ),
}));
