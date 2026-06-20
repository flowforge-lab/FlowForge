// Read-only cache for the Settings → Memory section (SET.8, #131). Mirrors
// store/mcp.ts's load pattern: fetch the frozen RFC 0006 memory IPC on panel
// mount and hold it for the component, which derives the WHO-less Identity /
// Patterns / Focus cards, JOURNAL, and FILES surfaces via lib/memory-view.ts.
//
// Read-only by design: there are no write commands in the contract (the memory
// model is captured by the agent; the enable toggle + search land with #166).

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type { MemoryFileInfo } from "@/bindings/MemoryFileInfo";
import type { MemoryOverview } from "@/bindings/MemoryOverview";

interface MemoryState {
  files: MemoryFileInfo[];
  overview: MemoryOverview | null;
  /** Body of the curated `MEMORY.md`, parsed into the category cards. */
  curatedBody: string | null;
  /** Daily file body by relPath, used only to derive JOURNAL previews. */
  journalBodies: Record<string, string>;
  query: string;
  loading: boolean;
  error: string | null;
  /** How many silent context-pressure flushes wrote to memory this app session
   *  (#283) — drives the "memory auto-curated" provenance banner. */
  flushCount: number;
  /** Durable facts written by the most recent flush. */
  lastFlushWrites: number;

  /** Fetch list + overview + curated/daily bodies (on panel mount). */
  load: () => Promise<void>;
  setQuery: (q: string) => void;
  /** Footer "Reset to defaults" — clears the search only (no IPC writes). */
  resetSearch: () => void;
  /** Record a `memory:flushed` event (wired in lib/events.ts). The Settings pane
   *  reloads on the bump so freshly-flushed content shows. */
  noteFlush: (writes: number) => void;
}

export const useMemoryStore = create<MemoryState>((set) => ({
  files: [],
  overview: null,
  curatedBody: null,
  journalBodies: {},
  query: "",
  loading: false,
  error: null,
  flushCount: 0,
  lastFlushWrites: 0,

  load: async () => {
    set({ loading: true, error: null });
    try {
      const [files, overview] = await Promise.all([
        ipc.listMemoryFiles(),
        ipc.memoryOverview(),
      ]);

      // Curated body drives the category cards; a missing MEMORY.md is fine.
      const curated = files.find((f) => f.kind === "curated");
      const curatedBody = curated
        ? await ipc.readMemoryFile(curated.relPath).catch(() => null)
        : null;

      // Daily bodies are read only for the one-line JOURNAL previews; a single
      // failed read degrades to an empty preview rather than failing the load.
      const dailies = files.filter((f) => f.kind === "daily");
      const bodies = await Promise.all(
        dailies.map((f) =>
          ipc
            .readMemoryFile(f.relPath)
            .then((body) => [f.relPath, body] as const)
            .catch(() => [f.relPath, ""] as const),
        ),
      );

      set({
        files,
        overview,
        curatedBody,
        journalBodies: Object.fromEntries(bodies),
        loading: false,
      });
    } catch (e) {
      set({
        loading: false,
        error: e instanceof Error ? e.message : String(e),
      });
    }
  },

  setQuery: (query) => set({ query }),
  resetSearch: () => set({ query: "" }),
  noteFlush: (writes) =>
    set((s) => ({ flushCount: s.flushCount + 1, lastFlushWrites: writes })),
}));
