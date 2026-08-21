// Which sessions have the transcript message navigator popup open (#1290).
//
// A store rather than component state because the popup has two entry points
// that don't share a subtree: the pill itself (inside `ChatView`) and the
// ⌘⇧O shortcut, which lives in the one global keydown listener in
// `app-shell.tsx`. That listener also has to *close* it — Escape must be
// swallowed by the navigator before it reaches `closeSplit()` /
// `cancelActiveTurn()`, the same "the open overlay owns the keyboard" rule the
// palette and the settings dialog already follow in that file.
//
// Keyed per session, like the reveal bus and the file panel, because of split
// panes (#148): ⌘⇧O opens the focused pane's navigator, and pane B's popup
// must not appear alongside it.
//
// Deliberately *not* persisted: a popup that survives a restart is a bug, and
// staying transient keeps the `durableStorage` / `hasHydrated` dance out of a
// piece of state whose whole lifetime is one gesture.

import { create } from "zustand";

interface MessageNavigatorState {
  /** Sessions whose navigator popup is open. */
  openSessions: Set<string>;
  openNavigator: (sessionId: string) => void;
  closeNavigator: (sessionId: string) => void;
  toggleNavigator: (sessionId: string) => void;
}

export const useMessageNavigator = create<MessageNavigatorState>(
  (set, get) => ({
    openSessions: new Set(),

    openNavigator: (sessionId) => {
      if (get().openSessions.has(sessionId)) return;
      // Fresh Set: zustand only notifies on identity change.
      const openSessions = new Set(get().openSessions);
      openSessions.add(sessionId);
      set({ openSessions });
    },

    closeNavigator: (sessionId) => {
      if (!get().openSessions.has(sessionId)) return;
      const openSessions = new Set(get().openSessions);
      openSessions.delete(sessionId);
      set({ openSessions });
    },

    toggleNavigator: (sessionId) => {
      const open = get().openSessions.has(sessionId);
      if (open) get().closeNavigator(sessionId);
      else get().openNavigator(sessionId);
    },
  }),
);
