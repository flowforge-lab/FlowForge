import { create } from "zustand";
import type { Goal } from "@/bindings/Goal";
import { ipc } from "@/lib/ipc";

// Goal mode (#717, RFC 0020). A standalone slice — like `session-workspace` /
// `update` — holding the current goal per session. The panel reads it; the
// `goal:updated` event (wired in `lib/events.ts`) is the source of truth for
// set / pause / resume / steer / iteration boundaries. Controls call the matching
// IPC and let that event patch state back (same pattern as `checkoutBranch`),
// except `abort`, whose command returns void, so it drops the entry locally.
interface GoalState {
  /** sessionId -> its current goal. Absent when the session has no goal. */
  bySession: Record<string, Goal>;

  /** Upsert from a `goal:updated` event. */
  applyGoalUpdated: (goal: Goal) => void;
  /** Begin (or replace) a session's goal and start the loop. Resolves once the
   *  backend returns the new goal, which is upserted immediately so the panel
   *  shows even before the first `goal:updated` boundary event. */
  start: (
    sessionId: string,
    objective: string,
    maxIterations?: number,
  ) => Promise<void>;
  /** Fetch the current snapshot on panel mount, closing the race where a goal
   *  exists before the event listener attached. Best-effort. */
  hydrate: (sessionId: string) => Promise<void>;
  /** Pause the active goal (event re-renders it as `paused`). */
  pause: (sessionId: string) => Promise<void>;
  /** Resume a paused goal (event re-renders it as `active`). */
  resume: (sessionId: string) => Promise<void>;
  /** Abort + delete the goal; drops the local entry so the panel unmounts. */
  abort: (sessionId: string) => Promise<void>;
  /** Drop a session's goal locally (no IPC). */
  clear: (sessionId: string) => void;
}

export const useGoalStore = create<GoalState>((set) => ({
  bySession: {},

  applyGoalUpdated: (goal) =>
    set((s) => ({
      bySession: { ...s.bySession, [goal.sessionId]: goal },
    })),

  start: async (sessionId, objective, maxIterations) => {
    const goal = await ipc.goalSet(sessionId, objective, maxIterations);
    set((s) => ({ bySession: { ...s.bySession, [sessionId]: goal } }));
  },

  hydrate: async (sessionId) => {
    try {
      const goal = await ipc.goalStatus(sessionId);
      set((s) => {
        if (!goal) {
          if (!(sessionId in s.bySession)) return s;
          const { [sessionId]: _dropped, ...rest } = s.bySession;
          return { bySession: rest };
        }
        return { bySession: { ...s.bySession, [sessionId]: goal } };
      });
    } catch {
      // Best-effort — a missing goal is not an error state for the panel.
    }
  },

  pause: async (sessionId) => {
    await ipc.goalPause(sessionId);
  },

  resume: async (sessionId) => {
    await ipc.goalResume(sessionId);
  },

  abort: async (sessionId) => {
    await ipc.goalClear(sessionId);
    set((s) => {
      const { [sessionId]: _dropped, ...rest } = s.bySession;
      return { bySession: rest };
    });
  },

  clear: (sessionId) =>
    set((s) => {
      const { [sessionId]: _dropped, ...rest } = s.bySession;
      return { bySession: rest };
    }),
}));
