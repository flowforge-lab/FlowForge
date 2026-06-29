// App self-update state (#363, RFC 0014 P1). Holds the last update-check result and
// the in-flight install flag so the Settings → About row and a future background
// indicator can share one source of truth. Not persisted — it's transient runtime
// state re-derived from `checkForUpdates` on each launch / poll.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type { UpdateStatus } from "@/lib/about";

interface UpdateState {
  /** Result of the most recent check; `null` before the first one completes. */
  status: UpdateStatus | null;
  /** True while `installUpdate` is in flight (drives the "Update now" spinner). */
  installing: boolean;
  /** Session-local dismiss of the global update bar (#565). Not persisted — the
   *  bar reappears on the next poll/launch while the update is still available:
   *  `refresh()` clears this on a fresh `available` status. */
  dismissed: boolean;
  /** Check for updates and store the result. Errors are swallowed so the silent
   *  background poll never surfaces noise; the manual path reads `status` itself.
   *  Clears `dismissed` so a still-available update resurfaces the bar. */
  refresh: () => Promise<void>;
  /** Download + install the available update. On real success the backend relaunches
   *  the app, so this never resolves — `installing` stays true until restart. The
   *  `finally` clears it for the mock path (which resolves immediately) and on error. */
  install: () => Promise<void>;
  /** Dismiss the global update bar for this session (#565). */
  dismiss: () => void;
}

/**
 * Whether the background update poll should run (#567, RFC 0014 §12.3). Prod
 * always polls; a dev build polls only when the `localUpdateChannel`
 * experimental flag is on (to pick up a local `dev-release.sh` feed).
 * Extracted as a pure predicate because `import.meta.env.PROD`/`DEV` are Vite
 * compile-time constants that can't be flipped per-test. The feed is selected
 * by the backend `FF_UPDATER_ENDPOINT`: set -> the local `dev-release.sh` feed
 * (the intended pairing); unset -> the default public GitHub feed. So a dev
 * build with this flag on but no local endpoint still reaches the public feed
 * and is inert only while the dev version matches the latest release -- set
 * `FF_UPDATER_ENDPOINT` when enabling this flag.
 */
export function shouldPollUpdate(
  prod: boolean,
  dev: boolean,
  localUpdateChannel: boolean,
): boolean {
  return prod || (dev && localUpdateChannel);
}

export const useUpdateStore = create<UpdateState>((set) => ({
  status: null,
  installing: false,
  dismissed: false,

  refresh: async () => {
    try {
      const status = await ipc.checkForUpdates();
      // Clear dismiss on every fresh poll so a still-available update resurfaces.
      set({ status, dismissed: false });
    } catch {
      // Best-effort: a failed check leaves the previous status untouched.
    }
  },

  install: async () => {
    set({ installing: true });
    try {
      await ipc.installUpdate();
    } finally {
      set({ installing: false });
    }
  },

  dismiss: () => set({ dismissed: true }),
}));
