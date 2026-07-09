// Per-session `notebook_runner` kernel state (#871 FE-1). A standalone Zustand
// slice — like `goal.ts` / `session-workspace.ts` — holding the latest snapshot
// per session. The panel reads it; the panel owns the polling loop, so this store
// is not pushed to by backend events (a later `notebook:updated` push event can
// replace polling — tracked in #871, not implemented here).
//
// Mocked under `VITE_FF_MOCK=1` so the panel runs standalone. The real
// `notebook_status` / `notebook_stop` IPC commands land in a follow-up BE PR.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type { NotebookKernelState } from "@/bindings";

/**
 * A session's snapshot, after the FE has decided whether to display the panel.
 * `null` means "we've heard back from the backend for this session" — useful so
 * the panel can distinguish "never polled yet" from "polled and no kernel".
 * `undefined` means "this session id has never been touched", and is what the
 * absence-from-store uses to skip rendering until the first hydrate resolves.
 */
export type MaybeKernel = NotebookKernelState | null | undefined;

interface NotebookState {
  /** sessionId -> latest kernel snapshot (`null` = "no kernel"; `undefined` =
   *  not yet polled). The `undefined` case is encoded inside `undefined`-as-key
   *  entries the natural way so we don't need a separate "hydrated" boolean. */
  bySession: Record<string, MaybeKernel>;

  /** Hydrate a session's kernel state (#871 FE-1). On mount the panel reads
   *  `bySession[sessionId]` and renders accordingly:
   *   - undefined: render nothing yet (panel self-hides), wait for hydrate
   *   - null:      panel renders the "no kernel" row
   *   - {…}:       panel renders the live/dead row with a Stop button
   *  Best-effort: an IPC failure leaves the entry undefined so the next mount
   *  retries — we never strand the panel on a hard error. */
  hydrate: (sessionId: string) => Promise<void>;
  /** Force a refresh — used by the panel's polling loop while a kernel is live.
   *  A failure (or a session that no longer exists) drops the cached entry so
   *  the panel falls back to the "no kernel" row instead of stalling. */
  refresh: (sessionId: string) => Promise<void>;
  /** Stop the session's kernel. Resolves once the backend has acked; the
   *  caller follows up with `refresh(sessionId)` so the panel re-renders with
   *  the post-stop snapshot. Idempotent. */
  stop: (sessionId: string) => Promise<void>;
  /** Drop a session's cached snapshot (no IPC). Used by the chat store's
   *  delete-session reconciliation path so a vanished session's kernel
   *  doesn't dangle in memory. */
  clear: (sessionId: string) => void;
}

export const useNotebookStore = create<NotebookState>((set, get) => ({
  bySession: {},

  hydrate: async (sessionId) => {
    try {
      const state = await ipc.notebookStatus(sessionId);
      set((s) => ({ bySession: { ...s.bySession, [sessionId]: state } }));
    } catch (err) {
      // Best-effort: leave `bySession` empty so the next mount retries rather
      // than pinning the panel on a stale error state. Logged once so an offline
      // backend is visible during dev.
      console.error("[notebook] hydrate failed:", err);
    }
  },

  refresh: async (sessionId) => {
    try {
      const state = await ipc.notebookStatus(sessionId);
      set((s) => ({ bySession: { ...s.bySession, [sessionId]: state } }));
    } catch {
      // Drop the cached snapshot rather than freeze the panel. A missing session
      // (deleted) or a transient backend error should both fall back to "no
      // kernel" — the polling loop's next tick will re-hydrate if the session
      // is genuinely alive.
      set((s) => {
        if (!(sessionId in s.bySession)) return s;
        const { [sessionId]: _dropped, ...rest } = s.bySession;
        return { bySession: rest };
      });
    }
  },

  stop: async (sessionId) => {
    await ipc.notebookStop(sessionId);
    await get().refresh(sessionId);
  },

  clear: (sessionId) =>
    set((s) => {
      if (!(sessionId in s.bySession)) return s;
      const { [sessionId]: _dropped, ...rest } = s.bySession;
      return { bySession: rest };
    }),
}));
