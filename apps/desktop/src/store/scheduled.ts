// Scheduled-tasks store for the Scheduled section (#132, SET.9). The mock backend
// owns the task list for the session; this store is the UI cache. Mirrors
// `store/profiles.ts` (load + optimistic mutate via IPC).

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type {
  CreateScheduledTaskInput,
  RunRecord,
  ScheduledTask,
} from "@/bindings";

interface ScheduledState {
  tasks: ScheduledTask[];
  loading: boolean;
  saving: boolean;
  error: string | null;
  /** Latest fired-run session id per task id, from `runNow` + `scheduled:fired`.
   *  Backs the ↗ open-session jump; a task absent here has no known session yet. */
  runsByTask: Record<string, string>;
  /** The task id currently mid `runNow` (drives the row's run-now spinner), or null. */
  runningId: string | null;

  load: () => Promise<void>;
  toggle: (id: string) => Promise<void>;
  create: (input: CreateScheduledTaskInput) => Promise<void>;
  /** Delete a user task (built-ins are rejected by the backend). */
  remove: (id: string) => Promise<void>;
  /** Fire a task out of band; caches the run's session for the ↗ jump. The
   *  `scheduled:fired` / `scheduled:changed` events finalize the row state. */
  runNow: (id: string) => Promise<void>;
  /** A `scheduled:fired` event: cache the run's session and optimistically stamp
   *  `lastRun` until the `scheduled:changed` snapshot lands. */
  applyFired: (run: RunRecord) => void;
  /** A `scheduled:changed` snapshot: replace the task list wholesale (server truth),
   *  preserving the locally-cached run sessions. */
  applyChanged: (tasks: ScheduledTask[]) => void;
  /** Save an edit to a user task. The RFC command surface has no `update`, so this
   *  is delete-then-recreate (#541): the row keeps its position but gets a fresh id
   *  and reset run stamps. Rejects if the recreate fails after the delete. */
  edit: (id: string, input: CreateScheduledTaskInput) => Promise<void>;
  /** Footer reset: resume every paused task (the default running state). */
  resetScheduled: () => Promise<void>;
}

export const useScheduledStore = create<ScheduledState>((set, get) => ({
  tasks: [],
  loading: false,
  saving: false,
  error: null,
  runsByTask: {},
  runningId: null,

  load: async () => {
    set({ loading: true, error: null });
    try {
      const tasks = await ipc.listScheduledTasks();
      set({ tasks, loading: false });
    } catch (err) {
      set({
        loading: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  toggle: async (id) => {
    set({ saving: true, error: null });
    try {
      const updated = await ipc.toggleScheduledTask(id);
      set((s) => ({
        tasks: s.tasks.map((t) => (t.id === id ? updated : t)),
        saving: false,
      }));
    } catch (err) {
      set({
        saving: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  create: async (input) => {
    set({ saving: true, error: null });
    try {
      const task = await ipc.createScheduledTask(input);
      set((s) => ({ tasks: [...s.tasks, task], saving: false }));
    } catch (err) {
      set({
        saving: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  remove: async (id) => {
    set({ saving: true, error: null });
    try {
      await ipc.deleteScheduledTask(id);
      set((s) => ({
        tasks: s.tasks.filter((t) => t.id !== id),
        saving: false,
      }));
    } catch (err) {
      set({
        saving: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  runNow: async (id) => {
    set({ runningId: id, error: null });
    try {
      const run = await ipc.runScheduledTaskNow(id);
      set((s) => ({
        runningId: null,
        runsByTask: run.sessionId
          ? { ...s.runsByTask, [id]: run.sessionId }
          : s.runsByTask,
      }));
    } catch (err) {
      set({
        runningId: null,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  applyFired: (run) => {
    set((s) => ({
      runsByTask: run.sessionId
        ? { ...s.runsByTask, [run.taskId]: run.sessionId }
        : s.runsByTask,
      // Optimistic stamp; the `scheduled:changed` snapshot is the source of truth.
      tasks: s.tasks.map((t) =>
        t.id === run.taskId ? { ...t, lastRun: run.firedMs } : t,
      ),
    }));
  },

  applyChanged: (tasks) => {
    set({ tasks });
  },

  edit: async (id, input) => {
    set({ saving: true, error: null });
    try {
      // No `update` command exists, so recreate. Delete first; if create then
      // fails, surface the error (the old row is already gone — the list reload
      // on next open reflects backend truth).
      await ipc.deleteScheduledTask(id);
      const task = await ipc.createScheduledTask(input);
      set((s) => ({
        // Replace in place so the edited task keeps its position in the list.
        tasks: s.tasks.map((t) => (t.id === id ? task : t)),
        saving: false,
      }));
    } catch (err) {
      set({
        saving: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  resetScheduled: async () => {
    const paused = get().tasks.filter((t) => t.paused);
    for (const t of paused) {
      await get().toggle(t.id);
    }
  },
}));
