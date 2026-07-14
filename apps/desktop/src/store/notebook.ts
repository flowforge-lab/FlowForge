// Per-session `notebook_runner` kernel state (#871 FE-1). A standalone Zustand
// slice — like `goal.ts` / `session-workspace.ts` — holding the latest snapshot
// per session. The panel reads it; the panel owns the polling loop, so this store
// is not pushed to by backend events (a later `notebook:updated` push event can
// replace polling — tracked in #871, not implemented here).
//
// Mocked under `VITE_FF_MOCK=1` so the panel runs standalone. The real
// `notebook_status` / `notebook_stop` IPC commands are backed by
// `KernelSupervisor` (`crates/ff-tools/src/notebook/mod.rs`).

import { create } from "zustand";
import { ipc } from "@/lib/ipc";
import type { NotebookKernelState } from "@/bindings/NotebookKernelState";

// `KernelInfo` and the `kernels?` field on `NotebookKernelState` are part of the
// generated ts-rs binding now (#924, backing the multi-kernel switcher). Re-
// export `KernelInfo` so the panel imports it from one place.
export type { KernelInfo } from "@/bindings/KernelInfo";

/**
 * A session's snapshot, after the FE has decided whether to display the panel.
 * `null` means "we've heard back from the backend for this session" — useful so
 * the panel can distinguish "never polled yet" from "polled and no kernel".
 * `undefined` means "this session id has never been touched", and is what the
 * absence-from-store uses to skip rendering until the first hydrate resolves.
 */
export type MaybeKernel = NotebookKernelState | null | undefined;

interface NotebookState {
  /** sessionId -> latest kernel snapshot (`null` = "no kernel"; `undefined` =
   *  not yet polled). Absence of a key from the record is what "not yet
   *  polled" means — no separate "hydrated" boolean needed. */
  bySession: Record<string, MaybeKernel>;

  /** Set once `notebookStatus` rejects — meaning the `notebook_status` /
   *  `notebook_stop` Tauri commands aren't registered on this build (an older
   *  build predating the BE commands; see the contract note in `lib/ipc.ts`).
   *  Once tripped, `hydrate`/`refresh`/`stop` no-op instead of re-invoking a
   *  command that will never resolve — otherwise every session-pane
   *  mount would `console.error` against a real (non-mock) build. There's no
   *  legitimate way to "become available" again within one running app, so
   *  this only ever flips false -> true (tests reset it explicitly). */
  ipcUnavailable: boolean;

  /** Hydrate a session's kernel state (#871 FE-1). On mount the panel reads
   *  `bySession[sessionId]` and renders accordingly:
   *   - undefined: render nothing yet (panel self-hides), wait for hydrate
   *   - null:      panel renders the "no kernel" row
   *   - {…}:       panel renders the live/dead row with a Stop button
   *  Best-effort: an IPC failure leaves the entry undefined so the next mount
   *  retries — we never strand the panel on a hard error. The first failure
   *  also trips `ipcUnavailable` (see above). */
  hydrate: (sessionId: string) => Promise<void>;
  /** Force a refresh — used by the panel's polling loop while a kernel is live.
   *  A failure (or a session that no longer exists) drops the cached entry so
   *  the panel falls back to the "no kernel" row instead of stalling. */
  refresh: (sessionId: string) => Promise<void>;
  /** Stop the session's kernel. Resolves once the backend has acked; the
   *  caller follows up with `refresh(sessionId)` so the panel re-renders with
   *  the post-stop snapshot. Idempotent. `kernelId` stops a single kernel once
   *  a session holds more than one (Phase 3); omitted, it stops them all. */
  stop: (sessionId: string, kernelId?: string) => Promise<void>;
  /** Restart the session's kernel (#871 FE-2): discard the running subprocess
   *  and its in-kernel state, spawn a fresh one. The command returns the
   *  post-restart snapshot, which we write straight into `bySession` (no
   *  separate `refresh` round-trip). No-ops when the command surface is
   *  unavailable (same breaker as `stop`). */
  restart: (sessionId: string, kernelId?: string) => Promise<void>;
  /** Drop a session's cached snapshot (no IPC). Used by the chat store's
   *  delete-session reconciliation path so a vanished session's kernel
   *  doesn't dangle in memory. */
  clear: (sessionId: string) => void;
}

export const useNotebookStore = create<NotebookState>((set, get) => ({
  bySession: {},
  ipcUnavailable: false,

  hydrate: async (sessionId) => {
    if (get().ipcUnavailable) return;
    try {
      const state = await ipc.notebookStatus(sessionId);
      set((s) => ({ bySession: { ...s.bySession, [sessionId]: state } }));
    } catch (err) {
      // This is the sole gatekeeper for whether the backend command surface
      // exists at all: every session pane calls `hydrate` once on mount, so a
      // rejection here — before any kernel has ever been confirmed live —
      // means `notebook_status` isn't registered on this build, not a
      // per-session issue. Trip the breaker so every other mount short-
      // circuits instead of re-invoking a command that will never resolve.
      // `debug`, not `error`: an expected, documented, temporary condition
      // (see `ipcUnavailable`'s doc comment), not something to act on.
      set({ ipcUnavailable: true });
      console.debug(
        "[notebook] notebook_status unavailable — disabling the panel for this app run:",
        err,
      );
    }
  },

  refresh: async (sessionId) => {
    if (get().ipcUnavailable) return;
    try {
      const state = await ipc.notebookStatus(sessionId);
      set((s) => ({ bySession: { ...s.bySession, [sessionId]: state } }));
    } catch {
      // Unlike `hydrate`, a `refresh` failure doesn't trip the breaker: by
      // the time polling is running, `hydrate` already proved the command
      // exists, so a rejection here is far more likely "this session's
      // kernel/session is gone" than "the command doesn't exist" — scoped to
      // this one session, not a reason to disable every other pane's panel.
      // Drop the cached snapshot rather than freeze the panel; the polling
      // loop's next tick re-hydrates if the session is genuinely still alive.
      set((s) => {
        if (!(sessionId in s.bySession)) return s;
        const { [sessionId]: _dropped, ...rest } = s.bySession;
        return { bySession: rest };
      });
    }
  },

  stop: async (sessionId, kernelId) => {
    if (get().ipcUnavailable) return;
    await ipc.notebookStop(sessionId, kernelId);
    await get().refresh(sessionId);
  },

  restart: async (sessionId, kernelId) => {
    if (get().ipcUnavailable) return;
    const state = await ipc.notebookRestart(sessionId, kernelId);
    set((s) => ({ bySession: { ...s.bySession, [sessionId]: state } }));
  },

  clear: (sessionId) =>
    set((s) => {
      if (!(sessionId in s.bySession)) return s;
      const { [sessionId]: _dropped, ...rest } = s.bySession;
      return { bySession: rest };
    }),
}));
