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
  /** Full fire history per task id (newest first), lazily loaded when a row's
   *  history panel is opened (#544). Absent until `loadRuns` runs for that task. */
  historyByTask: Record<string, RunRecord[]>;
  /** Task ids whose history is currently being fetched. Per-task (not a single
   *  slot) so two concurrently-open panels have independent spinners and one's
   *  completion can't clear another's. */
  loadingRunsIds: Set<string>;
  /** The global pause-all kill-switch (RFC 0017 §8.3). When engaged the backend
   *  fires nothing — including manual `runNow`. Tracked in-session: the command
   *  surface exposes a setter but no getter, so this defaults to off on load. */
  pausedAll: boolean;
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
  /** Load a task's fire history (newest first) into `historyByTask` for its
   *  run-history panel. Re-runnable to refresh. */
  loadRuns: (id: string) => Promise<void>;
  /** Engage/release the global pause-all kill-switch. Optimistic, reverting on
   *  failure; the resolved state is authoritative. */
  setPausedAll: (paused: boolean) => Promise<void>;
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
  historyByTask: {},
  loadingRunsIds: new Set(),
  pausedAll: false,
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

  loadRuns: async (id) => {
    // A fetch for this task is already in flight — skip so a quick collapse/
    // re-expand can't leave two `loadRuns(id)` racing to overwrite each other.
    if (get().loadingRunsIds.has(id)) return;
    set((s) => ({
      loadingRunsIds: new Set(s.loadingRunsIds).add(id),
      error: null,
    }));
    try {
      const runs = await ipc.listScheduledRuns(id);
      set((s) => {
        const loadingRunsIds = new Set(s.loadingRunsIds);
        loadingRunsIds.delete(id);
        return {
          historyByTask: { ...s.historyByTask, [id]: runs },
          loadingRunsIds,
        };
      });
    } catch (err) {
      set((s) => {
        const loadingRunsIds = new Set(s.loadingRunsIds);
        loadingRunsIds.delete(id);
        return {
          loadingRunsIds,
          error: err instanceof Error ? err.message : String(err),
        };
      });
    }
  },

  setPausedAll: async (paused) => {
    // Optimistic flip so the switch responds immediately; revert on failure.
    const previous = get().pausedAll;
    set({ pausedAll: paused, error: null });
    try {
      const next = await ipc.setScheduledPausedAll(paused);
      set({ pausedAll: next });
    } catch (err) {
      set({
        pausedAll: previous,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  applyFired: (run) => {
    set((s) => ({
      runsByTask: run.sessionId
        ? { ...s.runsByTask, [run.taskId]: run.sessionId }
        : s.runsByTask,
      // Prepend the fresh run to an already-open history panel so it live-updates;
      // a panel that hasn't been opened stays absent and loads on demand.
      historyByTask:
        run.taskId in s.historyByTask
          ? {
              ...s.historyByTask,
              [run.taskId]: [run, ...s.historyByTask[run.taskId]],
            }
          : s.historyByTask,
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
