// Per-session model selection (RFC 0005 §11.2, Phase D; #499). Each pane hosts its
// own session, so model selection is per session — exactly like the workspace
// (#200) and mode (#266). The backend owns resolution (session > phenotype >
// global, inherit-when-None); this store is a thin cache so the per-pane model
// chip renders the resolved label synchronously and updates after a change. Not
// persisted to localStorage — the backend is the source of truth, loaded per
// session on mount.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type { ModelSelection, ResolvedModel } from "@/bindings";
import type { ServedWindow } from "@/lib/served-window";

export interface SessionModelState {
  /** The authoritative resolved `(connection, model)` per session, plus the
   *  attachment capabilities derived from it (§11.3) — drives the chip label and
   *  the composer attach gate. Absent = not loaded yet. */
  resolvedBySession: Record<string, ResolvedModel>;
  /** The raw per-session override, or `null` when the session inherits the
   *  phenotype/global tiers. Absent = not loaded yet; drives the "clear" item. */
  overrideBySession: Record<string, ModelSelection | null>;
  /** Sessions whose resolver call rejected — e.g. the Phase D backend commands
   *  aren't registered yet (this FE merges ahead of its backend half, #499). The
   *  chip hides for these rather than spinning forever or leaking an unhandled
   *  rejection; it reappears once `load` succeeds after the backend lands. */
  unavailableBySession: Record<string, boolean>;
  /** The served context window + source per session (#602). Absent until the
   *  backend forwards it. PENDING CONTRACT: today this is only seeded by the mock /
   *  tests; once Tony lands the `/api/ps` probe and the `ResolvedModel` fields
   *  (`contextWindow` / `trainedContextWindow` / `contextWindowSource`), populate it
   *  from `resolved.*` in `load()` below — no new IPC call, it rides on
   *  `resolveModelSelection`. See lib/served-window.ts. */
  servedWindowBySession: Record<string, ServedWindow | undefined>;

  /** Fetch and cache a session's resolved selection + raw override. Never rejects:
   *  a backend failure marks the session unavailable instead. */
  load: (sessionId: string) => Promise<void>;
  /** Seed/replace the served window for a session (#602). Used by the mock / tests
   *  now; the real populate happens in `load()` once the contract carries it. */
  setServedWindow: (
    sessionId: string,
    window: ServedWindow | undefined,
  ) => void;
  /** Set a session's override, then reload so the cache carries the backend's
   *  canonical resolved pair. Throws (cache unchanged) if the backend rejects it. */
  set: (sessionId: string, selection: ModelSelection) => Promise<void>;
  /** Clear a session's override so it falls back to phenotype/global, then reload. */
  clear: (sessionId: string) => Promise<void>;
}

export const useSessionModelStore = create<SessionModelState>((set, get) => ({
  resolvedBySession: {},
  overrideBySession: {},
  unavailableBySession: {},
  servedWindowBySession: {},

  setServedWindow: (sessionId, window) =>
    set((s) => ({
      servedWindowBySession: {
        ...s.servedWindowBySession,
        [sessionId]: window,
      },
    })),

  load: async (sessionId) => {
    try {
      const [resolved, override] = await Promise.all([
        ipc.resolveModelSelection(sessionId),
        ipc.getSessionModelSelection(sessionId),
      ]);
      set((s) => ({
        resolvedBySession: { ...s.resolvedBySession, [sessionId]: resolved },
        overrideBySession: { ...s.overrideBySession, [sessionId]: override },
        unavailableBySession: { ...s.unavailableBySession, [sessionId]: false },
      }));
    } catch {
      // The backend half isn't present (the Phase D commands aren't registered, so
      // Tauri rejects). Degrade gracefully: mark the session unavailable so the chip
      // hides instead of spinning, and swallow the rejection so it doesn't surface
      // as an unhandled promise rejection in the real app.
      set((s) => ({
        unavailableBySession: { ...s.unavailableBySession, [sessionId]: true },
      }));
    }
  },

  set: async (sessionId, selection) => {
    await ipc.setSessionModelSelection(sessionId, selection);
    await get().load(sessionId);
  },

  clear: async (sessionId) => {
    await ipc.setSessionModelSelection(sessionId, null);
    await get().load(sessionId);
  },
}));
