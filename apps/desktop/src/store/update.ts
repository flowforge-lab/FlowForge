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
  /** Check for updates and store the result. Errors are swallowed so the silent
   *  background poll never surfaces noise; the manual path reads `status` itself. */
  refresh: () => Promise<void>;
  /** Download + install the available update. On real success the backend relaunches
   *  the app, so this never resolves — `installing` stays true until restart. The
   *  `finally` clears it for the mock path (which resolves immediately) and on error. */
  install: () => Promise<void>;
}

/**
 * Whether the background update poll should run (#567, RFC 0014 §12.3). Prod
 * always polls; a dev build polls only when the `localUpdateChannel`
 * experimental flag is on (to pick up a local `dev-release.sh` feed).
 * Extracted as a pure predicate because `import.meta.env.PROD`/`DEV` are Vite
 * compile-time constants that can't be flipped per-test. The dev feed is gated
 * by `FF_UPDATER_ENDPOINT`; without it the check returns up-to-date, so a dev
 * process never reaches the public GitHub feed.
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

  refresh: async () => {
    try {
      set({ status: await ipc.checkForUpdates() });
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
}));
