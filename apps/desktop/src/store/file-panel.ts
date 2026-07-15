// Workspace file browser state (#872). Backs the `{ kind: "files" }` split-panel
// surface: a lazily-loaded directory tree plus the currently-open file's body.
//
// Only *view* state (which dirs are expanded, which file is selected, the
// markdown raw/rendered toggle) is persisted to localStorage — the directory
// listings and file bodies are transient caches, re-fetched from the backend on
// demand, so large file contents never get serialized on every mutation (the
// mistake `store/split.ts` would make if it carried the payload itself).

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import { useSplitStore } from "@/store/split";
import type { DirEntry, FileContent } from "@/bindings";

const STORAGE_KEY = "ff-file-panel";

/** On-disk shape. `expanded` is an array in localStorage (JSON has no Set); the
 *  live store holds it as a `Set` for O(1) membership tests during render. */
interface Persisted {
  expanded: string[];
  selectedPath: string | null;
  markdownRaw: boolean;
}

/** Live view state, hydrated from {@link Persisted}. */
interface ViewState {
  /** Directory rel-paths (workspace-root-relative, `""` = root) that are open. */
  expanded: Set<string>;
  /** The file rel-path shown in the viewer, or null when none is selected. */
  selectedPath: string | null;
  /** For markdown files: show raw source (true) instead of rendered (false). */
  markdownRaw: boolean;
}

function loadPersisted(): ViewState {
  const fallback: ViewState = {
    expanded: new Set(),
    selectedPath: null,
    markdownRaw: false,
  };
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return fallback;
    const p = JSON.parse(raw) as Partial<Persisted>;
    return {
      expanded: new Set(Array.isArray(p.expanded) ? p.expanded : []),
      selectedPath: p.selectedPath ?? null,
      markdownRaw: Boolean(p.markdownRaw),
    };
  } catch {
    return fallback;
  }
}

function persist(v: ViewState): void {
  try {
    const p: Persisted = {
      expanded: [...v.expanded],
      selectedPath: v.selectedPath,
      markdownRaw: v.markdownRaw,
    };
    localStorage.setItem(STORAGE_KEY, JSON.stringify(p));
  } catch {
    // Quota / private mode — non-fatal; the panel just won't survive reload.
  }
}

interface FilePanelState extends ViewState {
  /** Which session the transient caches below belong to. Changing sessions
   *  clears them so one session's tree never leaks into another. */
  sessionId: string | null;
  /** Cached directory listings by rel-path. Absent = not yet fetched. */
  tree: Record<string, DirEntry[]>;
  /** Per-directory load error, keyed by rel-path (e.g. jail rejection). */
  dirError: Record<string, string>;
  /** The selected file's body, or null when none loaded / still loading. */
  content: FileContent | null;
  contentLoading: boolean;
  contentError: string | null;

  /** Open the Files panel in the split surface (palette / ⌘⇧E entry point). */
  openFiles: () => void;
  /** Point the panel at `sessionId`, loading the root listing and re-hydrating
   *  the persisted expanded dirs + selected file. Clears caches on a switch. */
  syncSession: (sessionId: string) => Promise<void>;
  /** Fetch + cache a directory listing if not already present. */
  ensureDir: (path: string) => Promise<void>;
  /** Toggle a directory open/closed, fetching its listing on first expand. */
  toggleExpand: (path: string) => Promise<void>;
  /** Select a file and load its body into the viewer. */
  selectFile: (path: string) => Promise<void>;
  /** Flip the markdown raw/rendered toggle. */
  setMarkdownRaw: (raw: boolean) => void;
}

export const useFilePanelStore = create<FilePanelState>((set, get) => {
  const save = () => {
    const { expanded, selectedPath, markdownRaw } = get();
    persist({ expanded, selectedPath, markdownRaw });
  };

  return {
    ...loadPersisted(),
    sessionId: null,
    tree: {},
    dirError: {},
    content: null,
    contentLoading: false,
    contentError: null,

    openFiles: () => {
      useSplitStore.getState().openInSplit({ kind: "files" });
    },

    syncSession: async (sessionId) => {
      if (get().sessionId !== sessionId) {
        // New session: drop the previous session's cached listings/content.
        set({ sessionId, tree: {}, dirError: {}, content: null });
      }
      await get().ensureDir("");
      // Re-hydrate persisted expanded dirs so the tree comes back as left.
      for (const dir of get().expanded) {
        if (dir) await get().ensureDir(dir);
      }
      const { selectedPath } = get();
      if (selectedPath) await get().selectFile(selectedPath);
    },

    ensureDir: async (path) => {
      const { sessionId, tree } = get();
      if (!sessionId || tree[path]) return;
      try {
        const entries = await ipc.listDirectory(sessionId, path);
        set((s) => ({
          tree: { ...s.tree, [path]: entries },
          dirError: omit(s.dirError, path),
        }));
      } catch (e) {
        set((s) => ({
          dirError: { ...s.dirError, [path]: errMsg(e) },
        }));
      }
    },

    toggleExpand: async (path) => {
      const isOpen = get().expanded.has(path);
      // Clone into a fresh Set so the change is a new reference — Zustand
      // subscribers only re-render when the identity changes.
      const expanded = new Set(get().expanded);
      if (isOpen) {
        expanded.delete(path);
        set({ expanded });
        save();
        return;
      }
      expanded.add(path);
      set({ expanded });
      save();
      await get().ensureDir(path);
    },

    selectFile: async (path) => {
      const { sessionId } = get();
      if (!sessionId) return;
      set({ selectedPath: path, contentLoading: true, contentError: null });
      save();
      try {
        const content = await ipc.readFile(sessionId, path);
        // Ignore a stale response if the selection changed while we awaited.
        if (get().selectedPath !== path) return;
        set({ content, contentLoading: false });
      } catch (e) {
        if (get().selectedPath !== path) return;
        set({ content: null, contentLoading: false, contentError: errMsg(e) });
      }
    },

    setMarkdownRaw: (raw) => {
      set({ markdownRaw: raw });
      save();
    },
  };
});

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function omit<T>(rec: Record<string, T>, key: string): Record<string, T> {
  if (!(key in rec)) return rec;
  const { [key]: _drop, ...rest } = rec;
  return rest;
}
