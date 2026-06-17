// Memory browser state for Settings → Memory (SET.8). Types are provisional —
// see `lib/memory.ts`. Loads via mock IPC today; real `ff-memory` wiring lands later.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type { MemorySnapshot } from "@/lib/memory";

interface MemoryState {
  snapshot: MemorySnapshot | null;
  query: string;
  loading: boolean;
  error: string | null;

  load: () => Promise<void>;
  setQuery: (query: string) => void;
  /** Footer reset — clears the search query (does not mutate mock data). */
  resetMemory: () => void;
}

export const useMemoryStore = create<MemoryState>((set) => ({
  snapshot: null,
  query: "",
  loading: false,
  error: null,

  load: async () => {
    set({ loading: true, error: null });
    try {
      const snapshot = await ipc.getMemory();
      set({ snapshot, loading: false });
    } catch (err) {
      set({
        loading: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  setQuery: (query) => set({ query }),

  resetMemory: () => set({ query: "" }),
}));
