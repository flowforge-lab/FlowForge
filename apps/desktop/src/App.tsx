import { useEffect } from "react";
import { AppShell } from "@/components/app-shell";
import { startIpcEvents } from "@/lib/events";
import { initPrefs } from "@/store/prefs";
import { useChatStore } from "@/store/chat";
import { useModelConfigStore } from "@/store/model-config";
import { useUpdateStore } from "@/store/update";

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
      void useChatStore.getState().bootstrap();
      // Load the provider registry so the composer knows the active model's
      // vision capability app-wide (FE-4, #342) — not just after Settings opens.
      void useModelConfigStore.getState().load();
      // Background update check, prod only — mock/dev never polls (#363). Initial
      // check on launch, then every few hours; cleared on teardown.
      if (import.meta.env.PROD) {
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
