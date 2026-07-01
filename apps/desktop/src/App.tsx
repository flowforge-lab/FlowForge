import { useEffect } from "react";
import { AppShell } from "@/components/app-shell";
import { startIpcEvents } from "@/lib/events";
import { reportFirstPaint } from "@/lib/boot-trace";
import { initPrefs } from "@/store/prefs";
import { useChatStore } from "@/store/chat";
import { useModelConfigStore } from "@/store/model-config";
import { shouldPollUpdate, useUpdateStore } from "@/store/update";
import { useExperimentalStore } from "@/store/experimental";

// How often the production build re-checks for an update in the background.
const UPDATE_POLL_MS = 6 * 60 * 60 * 1000;

// Module-level guard: StrictMode runs effects twice in dev; bootstrapping twice
// would create duplicate sessions.
let booted = false;

function App() {
  useEffect(() => {
    initPrefs();
    startIpcEvents();
    if (!booted) {
      booted = true;
      // #599 item 0: report FE first paint back to the Rust boot trace (dev /
      // FF_BOOT_TRACE only, real webview only). No-op otherwise.
      reportFirstPaint();
      void useChatStore.getState().bootstrap();
      // Load the provider registry so the composer knows the active model's
      // vision capability app-wide (FE-4, #342) — not just after Settings opens.
      void useModelConfigStore.getState().load();
      // Background update check (#363). Prod always polls; in a dev build the
      // poll runs only when the `localUpdateChannel` experimental flag is on
      // (#567). Pair the flag with a local `FF_UPDATER_ENDPOINT`; without it the
      // poll still reaches the default public GitHub feed, so set the endpoint
      // when enabling the flag in a `pnpm tauri dev` process.
      // Initial check on launch, then every few hours; cleared on teardown.
      const localUpdateChannel =
        useExperimentalStore.getState().flags.localUpdateChannel;
      if (
        shouldPollUpdate(
          import.meta.env.PROD,
          import.meta.env.DEV,
          localUpdateChannel,
        )
      ) {
        void useUpdateStore.getState().refresh();
        const id = setInterval(
          () => void useUpdateStore.getState().refresh(),
          UPDATE_POLL_MS,
        );
        return () => clearInterval(id);
      }
    }
  }, []);

  return <AppShell />;
}

export default App;
