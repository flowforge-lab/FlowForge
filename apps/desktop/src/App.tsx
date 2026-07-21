import { useEffect, useState } from "react";
import { AlertTriangle, Loader2 } from "@/components/ui/icon";
import { AppShell } from "@/components/app-shell";
import { startIpcEvents } from "@/lib/events";
import { reportFirstPaint } from "@/lib/boot-trace";
import { initTitleFlash } from "@/lib/title-flash";
import { ipc, type Unlisten } from "@/lib/ipc";
import { initPrefs, usePrefsStore } from "@/store/prefs";
import { useChatStore } from "@/store/chat";
import { useModelConfigStore } from "@/store/model-config";
import {
  shouldPollUpdate,
  activeUpdateChannel,
  useUpdateStore,
} from "@/store/update";
import { useExperimentalStore } from "@/store/experimental";

// How often the production build re-checks for an update in the background.
const UPDATE_POLL_MS = 6 * 60 * 60 * 1000;
// Local dogfood channel (#705): poll every 15s so a dev-release.sh build is
// picked up within seconds. Negligible on loopback (one tiny GET per tick).
const DEV_POLL_MS = 15_000;

// Module-level guard: StrictMode runs effects twice in dev; bootstrapping twice
// would create duplicate sessions.
let booted = false;

// Paint-first boot (#599): the backend defers `AppState::new()` off the
// synchronous pre-window path onto a background hydrate task, emitting
// `app:ready` when the state is managed. Subscribe-then-check closes the race
// where the event fired before the listener attached: `await` the `app:ready`
// / `app:init-error` listener registrations, THEN poll `isAppReady` (a static
// flag, safe to call pre-ready) — the await is what closes the race; a
// concurrent poll could slip between the `isAppReady` read and `listen()`
// resolving, missing the event and hanging on the splash forever. If the flag
// is already true we proceed immediately, otherwise we wait for the event. No
// backend command that reads managed `AppState` may run before this resolves.
//
// Error surface (#599 boot regression): the hydrate task's `JoinHandle` is
// dropped, so a panic in `AppState::new()` or the post-init wiring would be
// silently discarded by Tokio and `app:ready` would never fire — leaving this
// loading state spinning forever. The backend emits `app:init-error` on every
// early-exit path (carrying the reason); we also arm a timeout for the case
// where the task dies without emitting (e.g. killed mid-init), so the user
// never stares at a dead spinner with no recourse.
const APP_READY_TIMEOUT_MS = 30_000;

function useAppReady(): { ready: boolean; error: string | null } {
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let done = false;
    const markReady = () => {
      if (!done) {
        done = true;
        setReady(true);
      }
    };
    const markError = (reason: string) => {
      if (!done) {
        done = true;
        setError(reason);
      }
    };
    let offReady: Unlisten | undefined;
    let offError: Unlisten | undefined;
    let cancelled = false;
    // Subscribe-then-check MUST be sequenced, not concurrent (#599 review): both
    // `listen()` calls are async round-trips — the listener is only active once
    // the promise resolves. If the poll (`isAppReady`) raced ahead of the
    // subscription, `app:ready` could fire in the gap between `isAppReady`
    // reading false and the listener registering → event missed → permanent
    // splash (exactly the race this hook exists to close). Await both
    // subscriptions so neither event can slip through, THEN poll.
    void (async () => {
      let unReady: Unlisten | undefined;
      let unError: Unlisten | undefined;
      try {
        unReady = await ipc.onAppReady(markReady);
      } catch (err) {
        console.error("[useAppReady] onAppReady failed:", err);
      }
      try {
        unError = await ipc.onAppInitError(markError);
      } catch (err) {
        console.error("[useAppReady] onAppInitError failed:", err);
      }
      if (cancelled) {
        unReady?.();
        unError?.();
        return;
      }
      offReady = unReady;
      offError = unError;
      try {
        const r = await ipc.isAppReady();
        if (!cancelled && r) markReady();
      } catch (err) {
        console.error("[useAppReady] isAppReady failed:", err);
      }
    })();
    const timeout = setTimeout(() => {
      markError(
        "FlowForge did not finish starting within 30s. Check the logs and restart.",
      );
    }, APP_READY_TIMEOUT_MS);
    return () => {
      cancelled = true;
      offReady?.();
      offError?.();
      clearTimeout(timeout);
    };
  }, []);
  return { ready, error };
}

// The loading/empty state painted before the backend's heavy init finishes.
// Kept dependency-free and unstyled-by-theme beyond the background so it paints
// in the smallest possible first frame.
function BootSplash() {
  return (
    <div className="flex h-full w-full items-center justify-center bg-background text-muted-foreground">
      <div className="flex items-center gap-2 text-sm">
        <Loader2 className="size-4 animate-spin" />
        <span>Starting FlowForge…</span>
      </div>
    </div>
  );
}

// Error state for the boot regression (#599): surfaces the reason the hydrate
// task emitted on `app:init-error` (or the timeout fallback) so the user sees
// an actionable message instead of a dead spinner. Mirrors `<BootSplash>`'s
// dependency-free, theme-light approach so it paints cleanly in the same
// minimal first frame.
function BootError({ reason }: { reason: string }) {
  return (
    <div className="flex h-full w-full items-center justify-center bg-background px-6 text-muted-foreground">
      <div className="flex max-w-md flex-col items-center gap-3 text-center">
        <AlertTriangle className="size-6 text-destructive" />
        <div className="text-sm font-medium text-foreground">
          FlowForge failed to start
        </div>
        <div className="text-xs text-muted-foreground">{reason}</div>
      </div>
    </div>
  );
}

function App() {
  const { ready, error } = useAppReady();

  // Safe to run before `app:ready`: prefs are localStorage-only, the event
  // wiring only subscribes (no commands), and the boot-trace first-paint stamp
  // measures exactly this loading frame — the win the reordering buys.
  useEffect(() => {
    initPrefs();
    initTitleFlash();
    startIpcEvents();
    reportFirstPaint();
  }, []);

  // Backend-dependent work: gated on `app:ready` so the invoke handlers (which
  // read `State<'_, Arc<AppState>>`) never run before the state is managed.
  useEffect(() => {
    if (!ready || booted) return;
    booted = true;
    void useChatStore.getState().bootstrap();
    // Load the provider registry so the composer knows the active model's
    // vision capability app-wide (FE-4, #342) — not just after Settings opens.
    void useModelConfigStore.getState().load();
    // Hydrate the global default mode from the backend `mode.json` (#798) so the
    // user's chosen default survives a relaunch. Gated here (not in initPrefs)
    // because it invokes a command that reads the managed AppState.
    void usePrefsStore.getState().hydrateDefaultMode();
    // Background update check (#363). Prod always polls; in a dev build the
    // poll runs only when the `localUpdateChannel` experimental flag is on
    // (#567). The channel is passed explicitly to every check/install (#1033) —
    // the endpoint is never inferred from a backend flag, so the first check
    // can't race the dev-update watcher onto GitHub.
    const localUpdateChannel =
      useExperimentalStore.getState().flags.localUpdateChannel;
    if (
      shouldPollUpdate(
        import.meta.env.PROD,
        import.meta.env.DEV,
        localUpdateChannel,
      )
    ) {
      const channel = activeUpdateChannel();
      // In dogfood mode, auto-install detected updates without user
      // intervention (#705). The refresh+install cycle is fire-and-forget.
      const poll = async () => {
        const store = useUpdateStore.getState();
        await store.refresh(channel);
        const fresh = useUpdateStore.getState().status;
        if (
          localUpdateChannel &&
          !store.installing &&
          fresh?.kind === "available"
        ) {
          // Install exactly the build this poll just saw: the backend refuses
          // anything else, so a feed that moves mid-install re-prompts on the
          // next tick rather than installing an unseen build (#1034).
          void store.install(channel, fresh.version).catch(() => {});
        }
      };

      let feedUnsub: (() => void) | undefined;
      let id: ReturnType<typeof setInterval> | undefined;
      let cancelled = false;
      // Phase 2 (#705): file-system watcher for instant detection. On the local
      // channel, start the backend watcher and wire its event BEFORE the first
      // poll (#1033) — the watcher only observes the feed file; it no longer
      // gates the endpoint, so the ordering can't corrupt the check.
      const startPolling = () => {
        if (cancelled) return;
        void poll();
        const pollMs = localUpdateChannel ? DEV_POLL_MS : UPDATE_POLL_MS;
        id = setInterval(() => void poll(), pollMs);
      };

      if (localUpdateChannel) {
        void ipc
          .startDevUpdateWatcher()
          .then(() => ipc.onLocalFeedChanged(() => void poll()))
          .then((unsub) => {
            if (cancelled) {
              unsub();
              return;
            }
            feedUnsub = unsub;
          })
          .finally(startPolling);
      } else {
        startPolling();
      }

      return () => {
        cancelled = true;
        if (id !== undefined) clearInterval(id);
        feedUnsub?.();
      };
    }
  }, [ready]);

  if (error) return <BootError reason={error} />;
  if (!ready) return <BootSplash />;
  return <AppShell />;
}

export default App;
