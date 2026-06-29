// App self-update state (#363, RFC 0014 P1). Holds the last update-check result and
// the in-flight install flag so the Settings → About row and a future background
// indicator can share one source of truth. Not persisted — it's transient runtime
// state re-derived from `checkForUpdates` on each launch / poll.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type { UpdateStatus } from "@/lib/about";

/** In-flight self-update download progress (#566). Cumulative bytes downloaded and
 *  the content length; `total` is `null` when the feed omits it (indeterminate bar).
 *  The wire type (`UpdateProgressEvent`) carries `bigint`; we hold `number` here —
 *  byte counts are well within `Number.MAX_SAFE_INTEGER` and the percent math needs
 *  numbers, so the conversion happens at the event boundary (see lib/events.ts). */
export interface UpdateProgress {
  downloaded: number;
  total: number | null;
}

/** Determinate percent (0–100) when `total` is known, else `null` — the caller
 *  renders an indeterminate bar. Pure so it's unit-testable without a component. */
export function progressPercent(p: UpdateProgress | null): number | null {
  return p && p.total ? (p.downloaded / p.total) * 100 : null;
}

interface UpdateState {
  /** Result of the most recent check; `null` before the first one completes. */
  status: UpdateStatus | null;
  /** True while `installUpdate` is in flight (drives the "Update now" spinner). */
  installing: boolean;
  /** Live download progress while installing (#566); `null` before the first chunk,
   *  after the download finishes (terminal event), and when not installing. */
  progress: UpdateProgress | null;
  /** Check for updates and store the result. Errors are swallowed so the silent
   *  background poll never surfaces noise; the manual path reads `status` itself. */
  refresh: () => Promise<void>;
  /** Download + install the available update. On real success the backend relaunches
   *  the app, so this never resolves — `installing` stays true until restart. The
   *  `finally` clears it for the mock path (which resolves immediately) and on error. */
  install: () => Promise<void>;
  /** Set (or clear) the live download progress. Fed by the `update:progress` /
   *  `update:download-finished` listeners wired in lib/events.ts (#566). */
  setProgress: (progress: UpdateProgress | null) => void;
}

export const useUpdateStore = create<UpdateState>((set) => ({
  status: null,
  installing: false,
  progress: null,

  refresh: async () => {
    try {
      set({ status: await ipc.checkForUpdates() });
    } catch {
      // Best-effort: a failed check leaves the previous status untouched.
    }
  },

  install: async () => {
    set({ installing: true, progress: null });
    try {
      await ipc.installUpdate();
    } finally {
      set({ installing: false, progress: null });
    }
  },

  setProgress: (progress) => set({ progress }),
}));
