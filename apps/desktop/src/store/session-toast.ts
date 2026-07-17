// Queue of session-notification toasts (#703, expanded in #994), pushed from
// `chat.ts` via `lib/notify.ts` when something happens on a session the user is NOT
// viewing: a turn finished (`done`), errored (`error`), stopped without an answer
// (`stopped`), or is blocked awaiting the user's approval/answer (`approval`).
// Rendered by <SessionToasts>. Unlike the single-slot phenotype notice
// (store/pheno-mcp-notice.ts), this is a QUEUE: two sessions signalling close
// together must each get their own card rather than clobbering one another. But a
// single session must not spam the queue with the same kind, so `push` DEDUPES by
// (sessionId, kind) — a second same-kind toast for a session replaces the first.
// Auto-dismiss timing is caller-owned (the component), matching the ui/toast.tsx
// contract.

import { create } from "zustand";

/** Turn-outcome severity a toast announces (#994). Drives the icon/accent/label and
 *  which Settings flag gates it (see lib/notify.ts). */
export type ToastKind = "done" | "error" | "approval" | "stopped";

export interface SessionToast {
  id: string;
  sessionId: string;
  title: string;
  kind: ToastKind;
}

interface SessionToastState {
  toasts: SessionToast[];
  /** Enqueue a toast. Deduped by (sessionId, kind): a same-session/same-kind toast
   *  replaces the existing one (keeps the newest) rather than stacking. */
  push: (toast: Omit<SessionToast, "id">) => void;
  /** Remove one toast by its own id (dismiss / auto-dismiss / after an action). */
  dismiss: (id: string) => void;
  /** Drop all toasts for `sessionId` — used when that session becomes active. */
  dismissBySession: (sessionId: string) => void;
}

export const useSessionToastStore = create<SessionToastState>((set) => ({
  toasts: [],
  push: (toast) =>
    set((s) => ({
      toasts: [
        // Drop any prior toast of the same kind for this session, then append the
        // fresh one at the end so the newest sorts last (stable order for the rest).
        ...s.toasts.filter(
          (t) => !(t.sessionId === toast.sessionId && t.kind === toast.kind),
        ),
        { id: crypto.randomUUID(), ...toast },
      ],
    })),
  dismiss: (id) =>
    set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
  dismissBySession: (sessionId) =>
    set((s) => ({
      toasts: s.toasts.filter((t) => t.sessionId !== sessionId),
    })),
}));
