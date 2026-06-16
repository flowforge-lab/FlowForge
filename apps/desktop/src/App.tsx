import { useEffect } from "react";
import { AppShell } from "@/components/app-shell";
import { startIpcEvents } from "@/lib/events";
import { startWarmupOnFocus } from "@/lib/warmup";
import { initPrefs } from "@/store/prefs";
import { useChatStore } from "@/store/chat";

// Module-level guard: StrictMode runs effects twice in dev; bootstrapping twice
// would create duplicate sessions.
let booted = false;

function App() {
  useEffect(() => {
    initPrefs();
    startIpcEvents();
    startWarmupOnFocus();
    if (!booted) {
      booted = true;
      void useChatStore.getState().bootstrap();
    }
  }, []);

  return <AppShell />;
}

export default App;
