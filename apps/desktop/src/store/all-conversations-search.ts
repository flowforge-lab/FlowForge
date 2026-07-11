// Open/closed state for the full-screen "All Conversations" search modal
// (#876). Pure frontend, ephemeral (never persisted). Mirrors store/shortcuts.ts
// so app-shell's global handler can toggle it, and close it before other
// full-screen overlays open, the same way.

import { create } from "zustand";

interface AllConversationsSearchState {
  open: boolean;
  openSearch: () => void;
  closeSearch: () => void;
  toggleSearch: () => void;
}

export const useAllConversationsSearchStore =
  create<AllConversationsSearchState>((set) => ({
    open: false,
    openSearch: () => set({ open: true }),
    closeSearch: () => set({ open: false }),
    toggleSearch: () => set((s) => ({ open: !s.open })),
  }));
