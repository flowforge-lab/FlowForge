// App self-update state (#363, RFC 0014 P1). Holds the last update-check result and
// the in-flight install flag so the Settings → About row and a future background
// indicator can share one source of truth. Not persisted — it's transient runtime
// state re-derived from `checkForUpdates` on each launch / poll.

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import { useExperimentalStore } from "@/store/experimental";
import type { UpdateStatus, UpdateChannel } from "@/lib/about";

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
  /** Session-local dismiss of the global update bar (#565). Not persisted — the
   *  bar reappears on the next poll/launch while the update is still available:
   *  `refresh()` clears this on a fresh `available` status. */
  dismissed: boolean;
  /** Check `channel` for updates and store the result. On the `github` channel a
   *  failed check is swallowed (keep prior status) so the silent background poll
   *  never surfaces noise. On the `local` channel a failure means the dev-release
   *  feed is unreachable — we clear any stale status instead of leaving a GitHub
   *  result pinned in the UI (#1033). Clears `dismissed` so a still-available update
   *  resurfaces the bar. */
  refresh: (channel: UpdateChannel) => Promise<void>;
  /** Download + install the available update from `channel`. On real success the
   *  backend relaunches the app, so this never resolves — `installing` stays true
   *  until restart. The `finally` clears it for the mock path and on error. */
  install: (channel: UpdateChannel) => Promise<void>;
  /** Set (or clear) the live download progress. Fed by the `update:progress` /
   *  `update:download-finished` listeners wired in lib/events.ts (#566). */
  setProgress: (progress: UpdateProgress | null) => void;
  /** Dismiss the global update bar for this session (#565). */
  dismiss: () => void;
}

/**
 * Whether the background update poll should run (#567, RFC 0014 §12.3). Prod
 * always polls; a dev build polls only when the `localUpdateChannel`
 * experimental flag is on (to pick up a local `dev-release.sh` feed).
 * Extracted as a pure predicate because `import.meta.env.PROD`/`DEV` are Vite
 * compile-time constants that can't be flipped per-test. When the flag is on,
 * the backend automatically polls localhost:8787 (#749); no env var needed.
 */
export function shouldPollUpdate(
  prod: boolean,
  dev: boolean,
  localUpdateChannel: boolean,
): boolean {
  return prod || (dev && localUpdateChannel);
}

/**
 * The active update channel for this session (#1033). `local` when the
 * `localUpdateChannel` experimental flag is on (dev dogfood feed), else `github`.
 * Single source of truth so the boot poll, the About "Check for updates" row, and
 * the update banner all target the same feed. Reads the experimental store lazily
 * to avoid a static import cycle.
 */
export function activeUpdateChannel(): UpdateChannel {
  return useExperimentalStore.getState().flags.localUpdateChannel
    ? "local"
    : "github";
}

export const useUpdateStore = create<UpdateState>((set) => ({
  status: null,
  installing: false,
  progress: null,
  dismissed: false,

  refresh: async (channel) => {
    try {
      const status = await ipc.checkForUpdates(channel);
      // Clear dismiss on every fresh poll so a still-available update resurfaces.
      set({ status, dismissed: false });
    } catch {
      // On the local dogfood channel a failure means the dev-release feed is
      // unreachable — clear any stale status so a previous GitHub result isn't left
      // pinned in the banner (#1033). On the github channel, stay best-effort and
      // keep the previous status so the silent background poll never flickers.
      if (channel === "local") set({ status: null });
    }
  },

  install: async (channel) => {
    set({ installing: true, progress: null });
    try {
      await ipc.installUpdate(channel);
    } finally {
      set({ installing: false, progress: null });
    }
  },

  setProgress: (progress) => set({ progress }),
  dismiss: () => set({ dismissed: true }),
}));
