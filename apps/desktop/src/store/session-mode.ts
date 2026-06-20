// Per-session agent mode (#266, RFC 0011). Mode is resolved per session — and so
// per split pane (#148), since each pane hosts its own session — exactly like the
// workspace (#200). This store holds only *explicit* overrides; a session with no
// override inherits `usePrefsStore.defaultMode` (resolved at the call site, so the
// two stores stay decoupled). Persisted to localStorage under `"ff-session-mode"`.

import { create } from "zustand";
import { persist } from "zustand/middleware";
import type { Mode } from "@/bindings";

const STORAGE_KEY = "ff-session-mode";

/** Cycle order shown on the pill and stepped by click / shortcut: Plan → Act → Auto. */
export const MODE_ORDER: readonly Mode[] = ["plan", "act", "auto"];

/** The next mode in the cycle, wrapping Auto → Plan. */
export function nextMode(mode: Mode): Mode {
  const i = MODE_ORDER.indexOf(mode);
  return MODE_ORDER[(i + 1) % MODE_ORDER.length];
}

export interface SessionModeState {
  /** Explicit per-session overrides; absent → inherit `defaultMode`. */
  modeBySession: Record<string, Mode>;
  /** Resolve a session's mode, falling back to `fallback` (the default-mode pref). */
  resolve: (sessionId: string, fallback: Mode) => Mode;
  setMode: (sessionId: string, mode: Mode) => void;
  /** Advance one session's mode to the next in the cycle, seeding from `fallback`
   *  when it has no explicit override yet. */
  cycleMode: (sessionId: string, fallback: Mode) => void;
}

export const useSessionModeStore = create<SessionModeState>()(
  persist(
    (set, get) => ({
      modeBySession: {},
      resolve: (sessionId, fallback) =>
        get().modeBySession[sessionId] ?? fallback,
      setMode: (sessionId, mode) =>
        set((s) => ({
          modeBySession: { ...s.modeBySession, [sessionId]: mode },
        })),
      cycleMode: (sessionId, fallback) =>
        set((s) => {
          const current = s.modeBySession[sessionId] ?? fallback;
          return {
            modeBySession: {
              ...s.modeBySession,
              [sessionId]: nextMode(current),
            },
          };
        }),
    }),
    // `version` establishes a migration baseline now, so a future shape change can
    // migrate rather than silently drop overrides (#287 review).
    { name: STORAGE_KEY, version: 0 },
  ),
);
