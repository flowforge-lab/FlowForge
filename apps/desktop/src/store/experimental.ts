// Experimental opt-in flags for the Experimental section (#133, SET.10). All flags
// are FE-only today — each gates a future backend behavior (documented next to the
// switch in experimental-section.tsx). Persisted to localStorage under
// `"ff-experimental"`; mirrors `store/command-shortcuts.ts`. Default all off.

import { create } from "zustand";
import { persist } from "zustand/middleware";

const STORAGE_KEY = "ff-experimental";

/** The opt-in flags. Keep in sync with the rows in experimental-section.tsx. */
export type FlagId =
  | "ownApiKey"
  | "spotlight"
  | "preventSleep"
  | "remoteExecution"
  | "backgroundObservers"
  | "smartSkillSurfacing"
  // FE-only dev affordance (#417): gates the step-timeline download on the
  // StepGroup header. Unlike the others it gates a shipped FE behavior, not a
  // future backend.
  | "stepTimelineExport"
  // FE-only dev affordance (#567, RFC 0014 §12.3, P5c): lets the background
  // update poll run in a dev build so the global update bar picks up a local
  // `dev-release.sh` feed. Pairs with `FF_UPDATER_ENDPOINT`; without a local
  // feed the check returns up-to-date, so it never reaches the public feed.
  | "localUpdateChannel";

export const FLAG_IDS: readonly FlagId[] = [
  "ownApiKey",
  "spotlight",
  "preventSleep",
  "remoteExecution",
  "backgroundObservers",
  "smartSkillSurfacing",
  "stepTimelineExport",
  "localUpdateChannel",
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
  stepTimelineExport: false,
  localUpdateChannel: false,
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
