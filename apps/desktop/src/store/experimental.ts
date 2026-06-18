// Experimental opt-in flags for the Experimental section (#133, SET.10). All flags
// are FE-only today — each gates a future backend behavior (documented next to the
// switch in experimental-section.tsx). Persisted to localStorage under
// `"ff-experimental"`; mirrors `store/command-shortcuts.ts`. Default all off.

import { create } from "zustand";
import { persist } from "zustand/middleware";

const STORAGE_KEY = "ff-experimental";

/** The six opt-in flags. Keep in sync with the rows in experimental-section.tsx. */
export type FlagId =
  | "ownApiKey"
  | "spotlight"
  | "preventSleep"
  | "remoteExecution"
  | "backgroundObservers"
  | "smartSkillSurfacing";

export const FLAG_IDS: readonly FlagId[] = [
  "ownApiKey",
  "spotlight",
  "preventSleep",
  "remoteExecution",
  "backgroundObservers",
  "smartSkillSurfacing",
];

export type ExperimentalFlags = Record<FlagId, boolean>;

/** Every flag defaults off. Shared by initial state and `resetExperimental`. */
export const EXPERIMENTAL_DEFAULTS: ExperimentalFlags = {
  ownApiKey: false,
  spotlight: false,
  preventSleep: false,
  remoteExecution: false,
  backgroundObservers: false,
  smartSkillSurfacing: false,
};

export interface ExperimentalState {
  flags: ExperimentalFlags;
  setFlag: (id: FlagId, on: boolean) => void;
  resetExperimental: () => void;
}

export const useExperimentalStore = create<ExperimentalState>()(
  persist(
    (set) => ({
      flags: { ...EXPERIMENTAL_DEFAULTS },
      setFlag: (id, on) => set((s) => ({ flags: { ...s.flags, [id]: on } })),
      resetExperimental: () => set({ flags: { ...EXPERIMENTAL_DEFAULTS } }),
    }),
    {
      name: STORAGE_KEY,
      // Defaults first so a blob persisted before a flag existed hydrates that
      // flag to `false` rather than `undefined`.
      merge: (persisted, current) => {
        const p = persisted as Partial<ExperimentalState> | undefined;
        return {
          ...current,
          ...p,
          flags: { ...EXPERIMENTAL_DEFAULTS, ...(p?.flags ?? {}) },
        };
      },
    },
  ),
);
