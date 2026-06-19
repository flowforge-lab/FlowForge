// Per-session working directory (slice 3b, #200; git branch #211). The backend
// owns the value (its in-memory `session_cwd` map, defaulting to the global
// workspace root) and resolves the cwd's git branch; this store is a thin cache
// so the composer's workspace selector can render synchronously and update after
// a change. Not persisted to localStorage — the backend is the source of truth,
// loaded per session on mount.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type { SessionWorkspace } from "@/bindings";

/** Prepend `path` to the recents list, de-duped, most-recent-first. */
function addRecent(recents: string[], path: string): string[] {
  return [path, ...recents.filter((p) => p !== path)];
}

export interface SessionWorkspaceState {
  /** Cached workspace (path + git branch) by session id. Absent = not loaded. */
  bySession: Record<string, SessionWorkspace>;
  /** Distinct workspace paths seen this session, most-recent-first — feeds the
   *  selector dropdown (#210). In-memory only; cross-restart persistence is
   *  deliberately out of scope. */
  recents: string[];

  /** Fetch and cache a session's workspace from the backend. */
  load: (sessionId: string) => Promise<void>;
  /** Set a session's workspace, then reload so the cached entry carries the
   *  backend's canonical path and resolved git branch. Throws (and leaves the
   *  cache unchanged) if the backend rejects the path. */
  set: (sessionId: string, path: string) => Promise<void>;
  /** The cached workspace for a session, or `undefined` if not loaded. */
  get: (sessionId: string) => SessionWorkspace | undefined;
}

export const useSessionWorkspaceStore = create<SessionWorkspaceState>(
  (set, get) => ({
    bySession: {},
    recents: [],

    load: async (sessionId) => {
      const workspace = await ipc.getSessionWorkspace(sessionId);
      set((s) => ({
        bySession: { ...s.bySession, [sessionId]: workspace },
        recents: addRecent(s.recents, workspace.path),
      }));
    },

    set: async (sessionId, path) => {
      await ipc.setSessionWorkspace(sessionId, path);
      await get().load(sessionId);
    },

    get: (sessionId) => get().bySession[sessionId],
  }),
);
