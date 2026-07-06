// The real permission matrix (#702, RFC 0019 §3) — loaded from / persisted via IPC.
// Unlike store/control-config.ts (presentation only), editing a cell here drives
// runtime approval on the next tool invocation. Mirrors that store's load-once +
// optimistic-persist shape; each `setCell` edits one cell and reconciles with the
// view the backend returns.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type {
  Mode,
  Safety,
  PermissionCell,
  PermissionMatrixView,
  PermissionOverrideEntry,
} from "@/bindings";

/** Nested lookup built from the flat view: `matrix[mode][safety]` → cell. */
export type MatrixLookup = Record<Mode, Record<Safety, PermissionCell>>;

interface PermissionMatrixState {
  matrix: MatrixLookup | null;
  // Per-tool overrides (RFC 0019 §4) — a tool listed here bypasses the safety
  // matrix and resolves to its own cell across every mode. Kept as the sorted
  // list the backend returns so the Control Panel can group by cell.
  overrides: PermissionOverrideEntry[];
  loading: boolean;
  saving: boolean;
  error: string | null;

  load: () => Promise<void>;
  setCell: (mode: Mode, safety: Safety, cell: PermissionCell) => Promise<void>;
  setOverride: (tool: string, cell: PermissionCell) => Promise<void>;
  removeOverride: (tool: string) => Promise<void>;
}

/** Flatten the wire view into the nested `matrix[mode][safety]` lookup. */
function fromView(view: PermissionMatrixView): MatrixLookup {
  const lookup = {} as MatrixLookup;
  for (const { mode, safety, cell } of view.cells) {
    (lookup[mode] ??= {} as Record<Safety, PermissionCell>)[safety] = cell;
  }
  return lookup;
}

export const usePermissionMatrixStore = create<PermissionMatrixState>(
  (set, get) => ({
    matrix: null,
    overrides: [],
    // Starts loading so the grid doesn't flash "No permissions" before the
    // mount-effect's first load() resolves.
    loading: true,
    saving: false,
    error: null,

    load: async () => {
      set({ loading: true, error: null });
      try {
        const view = await ipc.getPermissionMatrix();
        set({
          matrix: fromView(view),
          overrides: view.overrides,
          loading: false,
        });
      } catch (err) {
        set({
          loading: false,
          error: err instanceof Error ? err.message : String(err),
        });
      }
    },

    setCell: async (mode, safety, cell) => {
      const { matrix } = get();
      if (!matrix || matrix[mode]?.[safety] === cell) return;
      // Optimistic: patch the single cell, then reconcile with the stored view.
      const optimistic: MatrixLookup = {
        ...matrix,
        [mode]: { ...matrix[mode], [safety]: cell },
      };
      set({ matrix: optimistic, saving: true, error: null });
      try {
        const view = await ipc.setPermissionCell(mode, safety, cell);
        set({
          matrix: fromView(view),
          overrides: view.overrides,
          saving: false,
        });
      } catch (err) {
        // Roll back to the pre-edit matrix on failure — but only if our optimistic
        // update is still the current one, so a stale older call can't clobber a
        // newer edit (the UI blocks concurrent edits, but this stays correct if a
        // caller ever bypasses that).
        set((state) => ({
          matrix: state.matrix === optimistic ? matrix : state.matrix,
          saving: false,
          error: err instanceof Error ? err.message : String(err),
        }));
      }
    },

    setOverride: async (tool, cell) => {
      const { overrides } = get();
      set({ saving: true, error: null });
      try {
        const view = await ipc.setToolOverride(tool, cell);
        set({
          matrix: fromView(view),
          overrides: view.overrides,
          saving: false,
        });
      } catch (err) {
        set({
          overrides,
          saving: false,
          error: err instanceof Error ? err.message : String(err),
        });
      }
    },

    removeOverride: async (tool) => {
      const { overrides } = get();
      set({ saving: true, error: null });
      try {
        const view = await ipc.removeToolOverride(tool);
        set({
          matrix: fromView(view),
          overrides: view.overrides,
          saving: false,
        });
      } catch (err) {
        set({
          overrides,
          saving: false,
          error: err instanceof Error ? err.message : String(err),
        });
      }
    },
  }),
);
