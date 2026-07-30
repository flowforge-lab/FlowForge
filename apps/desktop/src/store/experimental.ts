// Experimental opt-in flags for the Experimental section (#133, SET.10). All flags
// are FE-only today — each gates a future backend behavior (documented next to the
// switch in experimental-section.tsx). Persisted under `"ff-experimental"` via
// `durableStorage` (#1121); mirrors `store/command-shortcuts.ts`. Default all off.
//
// Hydration is async, and unlike the other flag-ish stores this one IS read
// imperatively at boot (`App.tsx` picks the update channel from
// `flags.localUpdateChannel`), so that read waits on `persist.onFinishHydration`
// rather than racing it — see `whenExperimentalHydrated` below.

import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { durableStorage } from "@/lib/durable-storage";

const STORAGE_KEY = "ff-experimental";

/** Default notebook_runner status poll interval (#871 FE-1). The panel reads this
 *  and re-arms a `setInterval` while the kernel state is live. 5s gives a steady
 *  "still running" signal without hammering the IPC; the lower bound (1s) keeps
 *  a power user from accidentally pegging the loop. */
export const NOTEBOOK_POLL_DEFAULT_MS = 5000;
export const NOTEBOOK_POLL_MIN_MS = 1000;
export const NOTEBOOK_POLL_MAX_MS = 60_000;

/** Clamp into the legal range; out-of-range values fall to the default so an
 *  invalid persisted value can't crash the poller. */
export function clampNotebookPollInterval(ms: number): number {
  if (!Number.isFinite(ms)) return NOTEBOOK_POLL_DEFAULT_MS;
  if (ms < NOTEBOOK_POLL_MIN_MS) return NOTEBOOK_POLL_MIN_MS;
  if (ms > NOTEBOOK_POLL_MAX_MS) return NOTEBOOK_POLL_MAX_MS;
  return ms;
}

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
  // Dev affordance (#567, #749): when ON the updater polls the local
  // `dev-release.sh` feed (localhost:8787) instead of GitHub, enabling the
  // seamless dogfood loop without any env-var setup.
  | "localUpdateChannel"
  // Dev affordance: gates the "Developer" group in the About section
  // (the sidecar smoke-test button). Defaults off so dev-only surfaces never
  // reach end users; the ipc.ts "Dev-only — never shipped onto a user-visible
  // surface" contract holds because nothing renders without this flag.
  | "devTools";

export const FLAG_IDS: readonly FlagId[] = [
  "ownApiKey",
  "spotlight",
  "preventSleep",
  "remoteExecution",
  "backgroundObservers",
  "smartSkillSurfacing",
  "stepTimelineExport",
  "localUpdateChannel",
  "devTools",
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
  devTools: false,
};

export interface ExperimentalState {
  flags: ExperimentalFlags;
  /** Notebook status panel poll interval (#871 FE-1), ms. Default
   *  `NOTEBOOK_POLL_DEFAULT_MS`; setters clamp into the legal range so the
   *  interval can never underflow or stall. */
  notebookPollIntervalMs: number;
  setFlag: (id: FlagId, on: boolean) => void;
  /** Clamped into `[NOTEBOOK_POLL_MIN_MS, NOTEBOOK_POLL_MAX_MS]`. */
  setNotebookPollInterval: (ms: number) => void;
  resetExperimental: () => void;
}

export const useExperimentalStore = create<ExperimentalState>()(
  persist(
    (set) => ({
      flags: { ...EXPERIMENTAL_DEFAULTS },
      notebookPollIntervalMs: NOTEBOOK_POLL_DEFAULT_MS,
      setFlag: (id, on) => set((s) => ({ flags: { ...s.flags, [id]: on } })),
      setNotebookPollInterval: (ms) =>
        set({ notebookPollIntervalMs: clampNotebookPollInterval(ms) }),
      resetExperimental: () =>
        set({
          flags: { ...EXPERIMENTAL_DEFAULTS },
          notebookPollIntervalMs: NOTEBOOK_POLL_DEFAULT_MS,
        }),
    }),
    {
      name: STORAGE_KEY,
      storage: createJSONStorage(() => durableStorage),
      // Defaults first so a blob persisted before a field existed hydrates that
      // field to its default rather than `undefined` — see the same merge on
      // `flags` for the original rationale; the interval is just the next row.
      merge: (persisted, current) => {
        const p = persisted as Partial<ExperimentalState> | undefined;
        return {
          ...current,
          ...p,
          flags: { ...EXPERIMENTAL_DEFAULTS, ...(p?.flags ?? {}) },
          notebookPollIntervalMs: clampNotebookPollInterval(
            p?.notebookPollIntervalMs ?? current.notebookPollIntervalMs,
          ),
        };
      },
    },
  ),
);

/** Resolves once the persisted flags have landed (immediately if they already
 *  have). Imperative boot-time readers must await this: `durableStorage` hydrates
 *  asynchronously, so a bare `getState()` at startup can see the all-off defaults
 *  and act on a flag the user actually turned on — e.g. polling the wrong update
 *  channel (#1121). Components don't need it; they re-render on hydration. */
export function whenExperimentalHydrated(): Promise<void> {
  if (useExperimentalStore.persist.hasHydrated()) return Promise.resolve();
  return new Promise((resolve) => {
    const unsub = useExperimentalStore.persist.onFinishHydration(() => {
      unsub();
      resolve();
    });
  });
}
