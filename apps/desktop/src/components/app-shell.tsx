import { useEffect } from "react";
import { AlertTriangle } from "lucide-react";
import { SessionSidebar } from "@/components/session-sidebar";
import { ChatView } from "@/components/chat-view";
import { InputBar } from "@/components/input-bar";
import { useChatStore } from "@/store/chat";

// Global, keyboard-native shortcuts: ⌘/Ctrl+N new session, ⌘/Ctrl+1..9 jump
// to session, Esc cancels the active turn. (Full ⌘K palette lands in M3.)
function useGlobalShortcuts() {
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      const store = useChatStore.getState();
      const mod = e.metaKey || e.ctrlKey;

      if (mod && e.key.toLowerCase() === "n") {
        e.preventDefault();
        void store.newSession();
        return;
      }
      if (mod && e.key >= "1" && e.key <= "9") {
        const target = store.sessions[Number(e.key) - 1];
        if (target) {
          e.preventDefault();
          void store.selectSession(target.id);
        }
        return;
      }
      if (e.key === "Escape") {
        void store.cancelActiveTurn();
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);
}

export function AppShell() {
  useGlobalShortcuts();
  const bootstrapError = useChatStore((s) => s.bootstrapError);

  return (
    <div className="flex h-full bg-background text-foreground">
      <SessionSidebar />
      <main className="flex min-w-0 flex-1 flex-col">
        {bootstrapError && (
          <div className="flex items-center gap-2 border-b border-destructive/30 bg-destructive/10 px-4 py-2 text-[13px] text-destructive">
            <AlertTriangle className="size-4 shrink-0" />
            <span>
              <strong>Backend unreachable:</strong> {bootstrapError}. Run{" "}
              <code className="rounded bg-destructive/15 px-1 font-mono text-xs">
                VITE_FF_MOCK=1 pnpm dev
              </code>{" "}
              to use the mock backend, or{" "}
              <code className="rounded bg-destructive/15 px-1 font-mono text-xs">
                pnpm tauri dev
              </code>{" "}
              for the real backend.
            </span>
          </div>
        )}
        <ChatView />
        <InputBar />
      </main>
    </div>
  );
}
