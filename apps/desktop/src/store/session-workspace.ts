// Per-session working directory (slice 3b, #200). The backend owns the value
// (its in-memory `session_cwd` map, defaulting to the global workspace root);
// this store is a thin cache so the composer's workspace selector can render
// synchronously and update after a change. Not persisted to localStorage —
// the backend is the source of truth, loaded per session on mount.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";

export interface SessionWorkspaceState {
  /** Cached workspace path by session id. Absent = not loaded yet. */
  pathBySession: Record<string, string>;

  /** Fetch and cache a session's workspace from the backend. */
  load: (sessionId: string) => Promise<void>;
  /** Set a session's workspace; caches the canonical path the backend returns.
   *  Throws (and leaves the cache unchanged) if the backend rejects the path. */
  set: (sessionId: string, path: string) => Promise<void>;
  /** The cached path for a session, or `undefined` if not loaded. */
  get: (sessionId: string) => string | undefined;
}

export const useSessionWorkspaceStore = create<SessionWorkspaceState>(
  (set, get) => ({
    pathBySession: {},

    load: async (sessionId) => {
      const path = await ipc.getSessionWorkspace(sessionId);
      set((s) => ({
        pathBySession: { ...s.pathBySession, [sessionId]: path },
      }));
    },

    set: async (sessionId, path) => {
      const canonical = await ipc.setSessionWorkspace(sessionId, path);
      set((s) => ({
        pathBySession: { ...s.pathBySession, [sessionId]: canonical },
      }));
    },

    get: (sessionId) => get().pathBySession[sessionId],
  }),
);
