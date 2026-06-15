// Open/closed state for the keyboard-shortcuts help overlay (Issue #20). Pure
// frontend, ephemeral (never persisted). Mirrors store/palette.ts so app-shell's
// global handler can toggle it the same way it toggles the palette.

import { create } from "zustand";

interface ShortcutsState {
  open: boolean;
  openShortcuts: () => void;
  closeShortcuts: () => void;
  toggleShortcuts: () => void;
}

export const useShortcutsStore = create<ShortcutsState>((set) => ({
  open: false,
  openShortcuts: () => set({ open: true }),
  closeShortcuts: () => set({ open: false }),
  toggleShortcuts: () => set((s) => ({ open: !s.open })),
}));
