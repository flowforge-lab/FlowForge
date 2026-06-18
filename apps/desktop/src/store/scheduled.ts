// Scheduled-tasks store for the Scheduled section (#132, SET.9). The mock backend
// owns the task list for the session; this store is the UI cache. Mirrors
// `store/profiles.ts` (load + optimistic mutate via IPC).

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type { CreateScheduledTaskInput, ScheduledTask } from "@/lib/scheduled";

interface ScheduledState {
  tasks: ScheduledTask[];
  loading: boolean;
  saving: boolean;
  error: string | null;

  load: () => Promise<void>;
  toggle: (id: string) => Promise<void>;
  create: (input: CreateScheduledTaskInput) => Promise<void>;
  /** Footer reset: resume every paused task (the default running state). */
  resetScheduled: () => Promise<void>;
}

export const useScheduledStore = create<ScheduledState>((set, get) => ({
  tasks: [],
  loading: false,
  saving: false,
  error: null,

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

  resetScheduled: async () => {
    const paused = get().tasks.filter((t) => t.paused);
    for (const t of paused) {
      await get().toggle(t.id);
    }
  },
}));
