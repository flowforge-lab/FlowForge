// Queue of "some dropped files couldn't be attached" notices (#723), pushed from
// the shared staging path (lib/stage-files.ts) when a drop / paste / pick rejects
// files, and rendered by <AttachRejectToast>. A QUEUE like the session-done queue
// (store/session-done-toast.ts): two quick rejections each get their own card
// rather than clobbering one another. Auto-dismiss timing is caller-owned (the
// component), matching the ui/toast.tsx contract.

import { create } from "zustand";

export interface RejectToast {
  id: string;
  message: string;
}

interface AttachRejectToastState {
  toasts: RejectToast[];
  /** Enqueue a rejection notice. */
  push: (message: string) => void;
  /** Remove one toast by its own id (dismiss / auto-dismiss). */
  dismiss: (id: string) => void;
}

export const useAttachRejectToastStore = create<AttachRejectToastState>(
  (set) => ({
    toasts: [],
    push: (message) =>
      set((s) => ({
        toasts: [...s.toasts, { id: crypto.randomUUID(), message }],
      })),
    dismiss: (id) =>
      set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
  }),
);
