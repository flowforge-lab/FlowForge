// Cache for the Settings → Memory section (SET.8, #131). Mirrors store/mcp.ts's
// load pattern: fetch the RFC 0006 memory IPC on panel mount and hold it for the
// component, which derives the WHO-less Identity / Patterns / Focus cards,
// JOURNAL, and FILES surfaces via lib/memory-view.ts.
//
// Almost read-only: the sole write is `writeStratum` (#868), a whole-stratum
// replace on the three curated `MEMORY.md` sections. Everything else the pane
// shows is captured by the agent (the enable toggle + search land with #166).

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type { MemoryCategoryId } from "@/lib/memory-view";
import type { MemoryChunkStat } from "@/bindings/MemoryChunkStat";
import type { MemoryFileInfo } from "@/bindings/MemoryFileInfo";
import type { MemoryOverview } from "@/bindings/MemoryOverview";

export interface MemoryState {
  files: MemoryFileInfo[];
  overview: MemoryOverview | null;
  /** Body of the curated `MEMORY.md`, parsed into the category cards. */
  curatedBody: string | null;
  /** Daily file body by relPath, used only to derive JOURNAL previews. */
  journalBodies: Record<string, string>;
  /** Per-chunk salience stats for the Salience surface (M6.2, #293). */
  chunks: MemoryChunkStat[];
  /** chunkKeys with an in-flight reset/pin mutation, for per-row busy state. */
  chunkBusy: Record<string, boolean>;
  /** True while a curated-stratum write is in flight (disables Save). */
  writeBusy: boolean;
  query: string;
  loading: boolean;
  error: string | null;
  /** How many silent context-pressure flushes wrote to memory this app session
   *  (#283) — drives the "memory auto-curated" provenance banner. */
  flushCount: number;
  /** Durable facts written by the most recent flush. */
  lastFlushWrites: number;

  /** Fetch list + overview + curated/daily bodies + chunk stats (on panel mount). */
  load: () => Promise<void>;
  setQuery: (q: string) => void;
  /** Footer "Reset to defaults" — clears the search only (no IPC writes). */
  resetSearch: () => void;
  /** Reset (wake) a chunk: weight back to 1.0; re-pulls the authoritative stats. */
  resetChunk: (chunkKey: string) => Promise<void>;
  /** Pin/unpin a chunk: pinned holds weight at 1.0 and is never dormant. */
  setPinned: (chunkKey: string, pinned: boolean) => Promise<void>;
  /** Replace one curated stratum's body, then re-pull the snapshot (#868).
   *  Resolves `true` on success; `false` leaves `error` set so the editor can
   *  keep the user's buffer instead of discarding it. */
  writeStratum: (stratum: MemoryCategoryId, text: string) => Promise<boolean>;
  /** Record a `memory:flushed` event (wired in lib/events.ts). The Settings pane
   *  reloads on the bump so freshly-flushed content shows. */
  noteFlush: (writes: number) => void;
}

/** Remove a key from the busy map without mutating the original. */
function clearBusy(
  busy: Record<string, boolean>,
  chunkKey: string,
): Record<string, boolean> {
  const next = { ...busy };
  delete next[chunkKey];
  return next;
}

export const useMemoryStore = create<MemoryState>((set, get) => ({
  files: [],
  overview: null,
  curatedBody: null,
  journalBodies: {},
  chunks: [],
  chunkBusy: {},
  writeBusy: false,
  query: "",
  loading: false,
  error: null,
  flushCount: 0,
  lastFlushWrites: 0,

  load: async () => {
    set({ loading: true, error: null });
    try {
      const [files, overview, chunks] = await Promise.all([
        ipc.listMemoryFiles(),
        ipc.memoryOverview(),
        ipc.listMemoryChunks(),
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
        chunks,
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

  // Reset/pin never edit Markdown — they only change `chunk_stats`, so we re-pull
  // the backend-authoritative snapshot rather than patching `weight`/`dormant`
  // locally (the FE never re-derives those). Mirrors store/mcp.ts's IPC-then-
  // reconcile pattern; per-row `chunkBusy` disables the controls while in flight.
  // The `chunkBusy` check is also an internal re-entrancy guard, so a double-fire
  // can't race even if a caller bypasses the UI's `disabled={busy}`.
  resetChunk: async (chunkKey) => {
    if (get().chunkBusy[chunkKey]) return;
    set((s) => ({
      chunkBusy: { ...s.chunkBusy, [chunkKey]: true },
      error: null,
    }));
    try {
      await ipc.resetMemoryChunk(chunkKey);
      const chunks = await ipc.listMemoryChunks();
      set((s) => ({ chunks, chunkBusy: clearBusy(s.chunkBusy, chunkKey) }));
    } catch (e) {
      set((s) => ({
        error: e instanceof Error ? e.message : String(e),
        chunkBusy: clearBusy(s.chunkBusy, chunkKey),
      }));
    }
  },

  setPinned: async (chunkKey, pinned) => {
    if (get().chunkBusy[chunkKey]) return;
    set((s) => ({
      chunkBusy: { ...s.chunkBusy, [chunkKey]: true },
      error: null,
    }));
    try {
      await ipc.setMemoryChunkPinned(chunkKey, pinned);
      const chunks = await ipc.listMemoryChunks();
      set((s) => ({ chunks, chunkBusy: clearBusy(s.chunkBusy, chunkKey) }));
    } catch (e) {
      set((s) => ({
        error: e instanceof Error ? e.message : String(e),
        chunkBusy: clearBusy(s.chunkBusy, chunkKey),
      }));
    }
  },

  // The write seam is deliberately narrow (#868/#969): the backend owns the
  // atomic rewrite of MEMORY.md, so we hand it the new section body and then
  // re-`load()` rather than patching `curatedBody` locally — the reload also
  // picks up the reindexed chunk stats the command produces as a side effect.
  // `writeBusy` doubles as a re-entrancy guard for a double-fired Save.
  writeStratum: async (stratum, text) => {
    if (get().writeBusy) return false;
    set({ writeBusy: true, error: null });
    try {
      await ipc.writeCuratedMemory(stratum, text);
      await get().load();
      set({ writeBusy: false });
      return true;
    } catch (e) {
      set({
        writeBusy: false,
        error: e instanceof Error ? e.message : String(e),
      });
      return false;
    }
  },

  noteFlush: (writes) =>
    set((s) => ({ flushCount: s.flushCount + 1, lastFlushWrites: writes })),
}));
