// Start-goal dialog visibility (#816). A goal is session-scoped (RFC 0020 §3), so
// the dialog carries the session it will start a goal for. `sessionId === null`
// means closed; a non-null value means open for that session. Ephemeral UI state,
// like `usePaletteStore.open` — never persisted. The palette's "Start goal…"
// command opens it; `start-goal-dialog.tsx` reads it and calls `useGoalStore.start`.
import { create } from "zustand";

interface GoalDialogState {
  /** The session a goal will be started for, or null when the dialog is closed. */
  sessionId: string | null;
  open: (sessionId: string) => void;
  close: () => void;
}

export const useGoalDialogStore = create<GoalDialogState>((set) => ({
  sessionId: null,
  open: (sessionId) => set({ sessionId }),
  close: () => set({ sessionId: null }),
}));
