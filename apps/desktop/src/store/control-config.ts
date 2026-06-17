// Control settings for the Control section (#127). Loaded from / persisted via
// IPC — the backend (mock for now) owns durable storage; this store is the UI
// cache. Mirrors store/search-config.ts. The whole config round-trips, so a
// `setX` applies an optimistic update and persists the full config.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import {
  CONTROL_DEFAULTS,
  policyForMode,
  type ControlConfig,
  type ControlOverrides,
  type DefaultMode,
} from "@/lib/control";

/** The string-list override buckets. */
type OverrideList = keyof ControlOverrides;

interface ControlConfigState {
  config: ControlConfig | null;
  loading: boolean;
  saving: boolean;
  error: string | null;

  load: () => Promise<void>;
  setDefaultMode: (mode: DefaultMode) => Promise<void>;
  addOverride: (list: OverrideList, value: string) => Promise<void>;
  removeOverride: (list: OverrideList, value: string) => Promise<void>;
  setInjectMemory: (inject: boolean) => Promise<void>;
  setUserInstructions: (text: string) => Promise<void>;
  addPromptFile: (path: string) => Promise<void>;
  removePromptFile: (path: string) => Promise<void>;
  resetControl: () => Promise<void>;
}

export const useControlConfigStore = create<ControlConfigState>((set, get) => ({
  config: null,
  loading: false,
  saving: false,
  error: null,

  load: async () => {
    set({ loading: true, error: null });
    try {
      const config = await ipc.getControlConfig();
      set({ config, loading: false });
    } catch (err) {
      set({
        loading: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  },

  setDefaultMode: (mode) =>
    persist(set, get, (c) => ({
      ...c,
      defaultMode: mode,
      // Selecting a column re-derives the per-row policy from that mode.
      permissionPolicy: policyForMode(mode),
    })),

  addOverride: (list, value) => {
    const trimmed = value.trim();
    if (trimmed === "") return Promise.resolve();
    return persist(set, get, (c) =>
      c.overrides[list].includes(trimmed)
        ? c
        : {
            ...c,
            overrides: {
              ...c.overrides,
              [list]: [...c.overrides[list], trimmed],
            },
          },
    );
  },

  removeOverride: (list, value) =>
    persist(set, get, (c) => ({
      ...c,
      overrides: {
        ...c.overrides,
        [list]: c.overrides[list].filter((v) => v !== value),
      },
    })),

  setInjectMemory: (inject) =>
    persist(set, get, (c) => ({ ...c, injectMemory: inject })),

  setUserInstructions: (text) =>
    persist(set, get, (c) => ({ ...c, userInstructions: text })),

  addPromptFile: (path) => {
    const trimmed = path.trim();
    if (trimmed === "") return Promise.resolve();
    return persist(set, get, (c) =>
      c.promptFiles.includes(trimmed)
        ? c
        : { ...c, promptFiles: [...c.promptFiles, trimmed] },
    );
  },

  removePromptFile: (path) =>
    persist(set, get, (c) => ({
      ...c,
      promptFiles: c.promptFiles.filter((p) => p !== path),
    })),

  resetControl: () =>
    persist(set, get, () => structuredClone(CONTROL_DEFAULTS)),
}));

// Apply a pure transform to the current config and persist it via IPC, keeping
// the store optimistic and surfacing errors. No-op if the config hasn't loaded.
type SetFn = (partial: Partial<ControlConfigState>) => void;
type GetFn = () => ControlConfigState;

async function persist(
  set: SetFn,
  get: GetFn,
  update: (config: ControlConfig) => ControlConfig,
): Promise<void> {
  const { config } = get();
  if (!config) return;
  const next = update(config);
  if (next === config) return; // transform decided it was a no-op
  set({ config: next, saving: true, error: null });
  try {
    const stored = await ipc.setControlConfig(next);
    set({ config: stored, saving: false });
  } catch (err) {
    set({
      saving: false,
      error: err instanceof Error ? err.message : String(err),
    });
  }
}
