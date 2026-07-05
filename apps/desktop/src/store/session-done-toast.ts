// Queue of "a background session finished" toasts (#703), pushed from
// `chat.finishTurn` when a turn completes on a session the user is NOT viewing,
// and rendered by <SessionDoneToast>. Unlike the single-slot phenotype notice
// (store/pheno-mcp-notice.ts), this is a QUEUE: two sessions finishing close
// together must each get their own card rather than clobbering one another.
// Auto-dismiss timing is caller-owned (the component), matching the ui/toast.tsx
// contract.

import { create } from "zustand";

export interface DoneToast {
  id: string;
  sessionId: string;
  title: string;
}

interface SessionDoneToastState {
  toasts: DoneToast[];
  /** Enqueue a completion toast for `sessionId`. */
  push: (sessionId: string, title: string) => void;
  /** Remove one toast by its own id (dismiss / auto-dismiss / after "View"). */
  dismiss: (id: string) => void;
  /** Drop any toasts for `sessionId` — used when that session becomes active. */
  dismissBySession: (sessionId: string) => void;
}

export const useSessionDoneToastStore = create<SessionDoneToastState>(
  (set) => ({
    toasts: [],
    push: (sessionId, title) =>
      set((s) => ({
        toasts: [...s.toasts, { id: crypto.randomUUID(), sessionId, title }],
      })),
    dismiss: (id) =>
      set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
    dismissBySession: (sessionId) =>
      set((s) => ({
        toasts: s.toasts.filter((t) => t.sessionId !== sessionId),
      })),
  }),
);
