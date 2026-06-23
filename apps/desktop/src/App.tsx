import { useEffect } from "react";
import { AppShell } from "@/components/app-shell";
import { startIpcEvents } from "@/lib/events";
import { initPrefs } from "@/store/prefs";
import { useChatStore } from "@/store/chat";
import { useModelConfigStore } from "@/store/model-config";

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
    }
  }, []);

  return <AppShell />;
}

export default App;
