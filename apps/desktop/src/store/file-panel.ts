// Workspace file browser state (#872, per-pane in #944). Backs the file panel
// that renders as the right portion of each session pane.
//
// The panel is scoped per-session: `openSessions` tracks which sessions have it
// open, and `bySession` holds each session's view + transient caches, so two
// panes showing different sessions browse independently. The chat|files and
// tree|viewer divider widths are single global values shared by every pane.
//
// Only *view* state (open sessions, per-session expanded dirs / selected file /
// markdown toggle, and the two widths) is persisted — the directory listings and
// file bodies are transient caches, re-fetched from the backend on demand, so
// large file contents never get serialized on every mutation.
//
// Stored through `durableStorage` (#1134) — a WKWebView doesn't reliably flush
// localStorage before the process exits, so a panel opened or a tree expanded
// late in a session could otherwise come back closed. The on-disk shape is
// unchanged, so an existing `ff-file-panel` value is adopted as-is; see
// lib/durable-json.ts. That read is async, hence `hasHydrated` below.

import { create } from "zustand";
import { readDurable, writeDurable } from "@/lib/durable-json";
import { ipc } from "@/lib/ipc";
import type { DirEntry, FileContent } from "@/bindings";

const STORAGE_KEY = "ff-file-panel";

/** Divider width bounds (px). The tree column and the whole file panel each get
 *  a drag handle; widths are clamped so neither can be dragged unusable. */
export const MIN_TREE_WIDTH = 140;
export const MAX_TREE_WIDTH = 420;
export const DEFAULT_TREE_WIDTH = 208; // matches the old fixed w-52
export const MIN_PANEL_WIDTH = 280;
export const MAX_PANEL_WIDTH = 720;
export const DEFAULT_PANEL_WIDTH = 420;

export function clampTreeWidth(px: number): number {
  return Math.max(MIN_TREE_WIDTH, Math.min(MAX_TREE_WIDTH, Math.round(px)));
}

export function clampPanelWidth(px: number): number {
  return Math.max(MIN_PANEL_WIDTH, Math.min(MAX_PANEL_WIDTH, Math.round(px)));
}

/** Live per-session view + caches, hydrated from {@link PersistedSession}. */
export interface SessionFileState {
  /** Directory rel-paths (workspace-root-relative, `""` = root) that are open. */
  expanded: Set<string>;
  /** The file rel-path shown in the viewer, or null when none is selected. */
  selectedPath: string | null;
  /** For markdown files: show raw source (true) instead of rendered (false). */
  markdownRaw: boolean;
  /** Cached directory listings by rel-path. Absent = not yet fetched. */
  tree: Record<string, DirEntry[]>;
  /** Per-directory load error, keyed by rel-path (e.g. jail rejection). */
  dirError: Record<string, string>;
  /** The selected file's body, or null when none loaded / still loading. */
  content: FileContent | null;
  contentLoading: boolean;
  contentError: string | null;
}

/** On-disk per-session shape. `expanded` is an array in localStorage (JSON has
 *  no Set); the live store holds it as a `Set` for O(1) membership tests. */
interface PersistedSession {
  expanded: string[];
  selectedPath: string | null;
  markdownRaw: boolean;
}

interface Persisted {
  openSessions: string[];
  panelWidth: number;
  treeWidth: number;
  view: Record<string, PersistedSession>;
}

function emptySlice(): SessionFileState {
  return {
    expanded: new Set(),
    selectedPath: null,
    markdownRaw: false,
    tree: {},
    dirError: {},
    content: null,
    contentLoading: false,
    contentError: null,
  };
}

interface Hydrated {
  openSessions: Set<string>;
  panelWidth: number;
  treeWidth: number;
  bySession: Record<string, SessionFileState>;
}

const FALLBACK: Hydrated = {
  openSessions: new Set(),
  panelWidth: DEFAULT_PANEL_WIDTH,
  treeWidth: DEFAULT_TREE_WIDTH,
  bySession: {},
};

function parsePersisted(raw: unknown): Hydrated {
  const p = (raw ?? {}) as Partial<Persisted>;
  const bySession: Record<string, SessionFileState> = {};
  const view = p.view ?? {};
  for (const [id, v] of Object.entries(view)) {
    bySession[id] = {
      ...emptySlice(),
      expanded: new Set(Array.isArray(v.expanded) ? v.expanded : []),
      selectedPath: v.selectedPath ?? null,
      markdownRaw: Boolean(v.markdownRaw),
    };
  }
  return {
    openSessions: new Set(Array.isArray(p.openSessions) ? p.openSessions : []),
    panelWidth: clampPanelWidth(p.panelWidth ?? DEFAULT_PANEL_WIDTH),
    treeWidth: clampTreeWidth(p.treeWidth ?? DEFAULT_TREE_WIDTH),
    bySession,
  };
}

function persist(h: Hydrated): void {
  const view: Record<string, PersistedSession> = {};
  for (const [id, s] of Object.entries(h.bySession)) {
    view[id] = {
      expanded: [...s.expanded],
      selectedPath: s.selectedPath,
      markdownRaw: s.markdownRaw,
    };
  }
  const p: Persisted = {
    openSessions: [...h.openSessions],
    panelWidth: h.panelWidth,
    treeWidth: h.treeWidth,
    view,
  };
  writeDurable(STORAGE_KEY, p);
}

interface FilePanelState {
  /** Sessions with the file panel open, keyed by session id. */
  openSessions: Set<string>;
  /** Per-session view state + transient caches. */
  bySession: Record<string, SessionFileState>;
  /** Chat|files divider width (px), shared across panes. */
  panelWidth: number;
  /** Tree|viewer divider width (px), shared across panes. */
  treeWidth: number;
  /** False until the (always-async) durable read has landed. `session-pane.tsx`
   *  gates the panel on it: `openSessions.has(id)` decides whether a pane shows
   *  the panel at all, and `syncSession` re-fetches the persisted expanded dirs
   *  on mount — both would otherwise run against empty defaults, flashing the
   *  chat full-width and dropping the restored tree. Runtime-only. */
  hasHydrated: boolean;

  /** Open the Files panel for `sessionId` (palette / ⌘⇧E / header entry point). */
  openFiles: (sessionId: string) => void;
  /** Close the Files panel for `sessionId`. */
  closeFiles: (sessionId: string) => void;
  /** Toggle the Files panel open/closed for `sessionId`. */
  toggleFiles: (sessionId: string) => void;
  /** Load the root listing and re-hydrate the persisted expanded dirs + selected
   *  file for `sessionId`. Called when a pane's file panel mounts. */
  syncSession: (sessionId: string) => Promise<void>;
  /** Fetch + cache a directory listing if not already present. */
  ensureDir: (sessionId: string, path: string) => Promise<void>;
  /** Toggle a directory open/closed, fetching its listing on first expand. */
  toggleExpand: (sessionId: string, path: string) => Promise<void>;
  /** Select a file and load its body into the viewer. */
  selectFile: (sessionId: string, path: string) => Promise<void>;
  /** Flip the markdown raw/rendered toggle. */
  setMarkdownRaw: (sessionId: string, raw: boolean) => void;
  /** Commit the chat|files divider width. */
  setPanelWidth: (px: number) => void;
  /** Commit the tree|viewer divider width. */
  setTreeWidth: (px: number) => void;
  /** Adopt the persisted view state. Fired once on module load; exported on the
   *  store so tests can re-run it after seeding storage. */
  hydrate: () => Promise<void>;
}

export const useFilePanelStore = create<FilePanelState>((set, get) => {
  const save = () => {
    // Before hydration the state is still defaults; writing them would clobber
    // the open panels and expanded trees the user actually left behind.
    if (!get().hasHydrated) return;
    persist(get());
  };

  /** Return a shallow-cloned `bySession` guaranteeing a slice for `id`, plus the
   *  slice itself. Callers mutate the returned map and `set` it. */
  const withSlice = (
    id: string,
  ): {
    bySession: Record<string, SessionFileState>;
    slice: SessionFileState;
  } => {
    const bySession = { ...get().bySession };
    const slice = bySession[id] ?? emptySlice();
    bySession[id] = slice;
    return { bySession, slice };
  };

  /** Merge `patch` into session `id`'s slice and commit it. */
  const patchSlice = (id: string, patch: Partial<SessionFileState>) => {
    const { bySession, slice } = withSlice(id);
    bySession[id] = { ...slice, ...patch };
    set({ bySession });
  };

  return {
    ...FALLBACK,
    hasHydrated: false,

    openFiles: (sessionId) => {
      if (get().openSessions.has(sessionId)) return;
      const openSessions = new Set(get().openSessions);
      openSessions.add(sessionId);
      set({ openSessions });
      save();
    },

    closeFiles: (sessionId) => {
      if (!get().openSessions.has(sessionId)) return;
      const openSessions = new Set(get().openSessions);
      openSessions.delete(sessionId);
      set({ openSessions });
      save();
    },

    toggleFiles: (sessionId) => {
      const open = get().openSessions.has(sessionId);
      if (open) get().closeFiles(sessionId);
      else get().openFiles(sessionId);
    },

    syncSession: async (sessionId) => {
      await get().ensureDir(sessionId, "");
      // Re-hydrate persisted expanded dirs so the tree comes back as left.
      const slice = get().bySession[sessionId];
      for (const dir of slice?.expanded ?? []) {
        if (dir) await get().ensureDir(sessionId, dir);
      }
      const selectedPath = get().bySession[sessionId]?.selectedPath;
      if (selectedPath) await get().selectFile(sessionId, selectedPath);
    },

    ensureDir: async (sessionId, path) => {
      if (get().bySession[sessionId]?.tree[path]) return;
      try {
        const entries = await ipc.listDirectory(sessionId, path);
        const { bySession, slice } = withSlice(sessionId);
        bySession[sessionId] = {
          ...slice,
          tree: { ...slice.tree, [path]: entries },
          dirError: omit(slice.dirError, path),
        };
        set({ bySession });
      } catch (e) {
        const { bySession, slice } = withSlice(sessionId);
        bySession[sessionId] = {
          ...slice,
          dirError: { ...slice.dirError, [path]: errMsg(e) },
        };
        set({ bySession });
      }
    },

    toggleExpand: async (sessionId, path) => {
      const current = get().bySession[sessionId] ?? emptySlice();
      const isOpen = current.expanded.has(path);
      // Clone into a fresh Set so the change is a new reference — Zustand
      // subscribers only re-render when the identity changes.
      const expanded = new Set(current.expanded);
      if (isOpen) {
        expanded.delete(path);
        patchSlice(sessionId, { expanded });
        save();
        return;
      }
      expanded.add(path);
      patchSlice(sessionId, { expanded });
      save();
      await get().ensureDir(sessionId, path);
    },

    selectFile: async (sessionId, path) => {
      patchSlice(sessionId, {
        selectedPath: path,
        contentLoading: true,
        contentError: null,
      });
      save();
      try {
        const content = await ipc.readFile(sessionId, path);
        // Ignore a stale response if the selection changed while we awaited.
        if (get().bySession[sessionId]?.selectedPath !== path) return;
        patchSlice(sessionId, { content, contentLoading: false });
      } catch (e) {
        if (get().bySession[sessionId]?.selectedPath !== path) return;
        patchSlice(sessionId, {
          content: null,
          contentLoading: false,
          contentError: errMsg(e),
        });
      }
    },

    setMarkdownRaw: (sessionId, raw) => {
      patchSlice(sessionId, { markdownRaw: raw });
      save();
    },

    setPanelWidth: (px) => {
      set({ panelWidth: clampPanelWidth(px) });
      save();
    },

    setTreeWidth: (px) => {
      set({ treeWidth: clampTreeWidth(px) });
      save();
    },

    hydrate: async () => {
      const stored = await readDurable(STORAGE_KEY, parsePersisted, FALLBACK);
      // Panels opened while the read was in flight are newer than what's on
      // disk, so they survive the merge rather than being closed under the user.
      // `bySession` slices already in memory win for the same reason (they hold
      // live listing caches the persisted copy doesn't have).
      set((s) => ({
        openSessions: new Set([...stored.openSessions, ...s.openSessions]),
        panelWidth: stored.panelWidth,
        treeWidth: stored.treeWidth,
        bySession: { ...stored.bySession, ...s.bySession },
        hasHydrated: true,
      }));
    },
  };
});

void useFilePanelStore.getState().hydrate();

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function omit<T>(rec: Record<string, T>, key: string): Record<string, T> {
  if (!(key in rec)) return rec;
  const { [key]: _drop, ...rest } = rec;
  return rest;
}
