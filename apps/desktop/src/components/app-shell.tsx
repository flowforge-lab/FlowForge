import { useEffect } from "react";
import { AlertTriangle } from "lucide-react";
import { SessionSidebar } from "@/components/session-sidebar";
import { ChatView } from "@/components/chat-view";
import { InputBar } from "@/components/input-bar";
import { SplitPanel } from "@/components/split-panel";
import { CommandPalette } from "@/components/palette";
import { ShortcutsOverlay } from "@/components/shortcuts-overlay";
import { SettingsPanel } from "@/components/settings-panel";
import { useChatStore } from "@/store/chat";
import { useSplitStore } from "@/store/split";
import { usePaletteStore } from "@/store/palette";
import { useSettingsStore } from "@/store/settings";
import { useShortcutsStore } from "@/store/shortcuts";

// True when focus is in a text-entry element, so a bare "?" types instead of
// opening the shortcuts overlay.
function isTextEntry(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el) return false;
  return (
    el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable
  );
}

// Global, keyboard-native shortcuts: ⌘/Ctrl+K command palette, ? / ⌘/Ctrl+/
// shortcuts help, ⌘/Ctrl+N new session, ⌘/Ctrl+1..9 jump to session, Esc
// cancels the active turn.
function useGlobalShortcuts() {
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      const palette = usePaletteStore.getState();
      const shortcuts = useShortcutsStore.getState();
      const settings = useSettingsStore.getState();
      const mod = e.metaKey || e.ctrlKey;

      // ⌘/Ctrl+K is home: toggle the palette from anywhere. Close overlays
      // first so they never stack (all are z-50).
      if (mod && e.key.toLowerCase() === "k") {
        e.preventDefault();
        shortcuts.closeShortcuts();
        settings.closeSettings();
        palette.togglePalette();
        return;
      }
      // While the palette owns the keyboard, don't fire shell shortcuts behind
      // it — it handles its own arrows/Enter/Esc.
      if (palette.open) return;

      // Shortcuts help: ⌘/Ctrl+/ anywhere, or "?" when not typing in a field.
      if (mod && e.key === "/") {
        e.preventDefault();
        shortcuts.toggleShortcuts();
        return;
      }
      if (!mod && e.key === "?" && !isTextEntry(e.target)) {
        e.preventDefault();
        shortcuts.openShortcuts();
        return;
      }
      // While the help overlay is open it owns the keyboard; Esc closes it and
      // shell shortcuts stand down behind it.
      if (shortcuts.open) {
        if (e.key === "Escape") {
          e.preventDefault();
          shortcuts.closeShortcuts();
        }
        return;
      }
      // Settings panel owns the keyboard the same way — Esc must not fall
      // through to split-close / cancel-turn (PR #78 review).
      if (settings.open) {
        if (e.key === "Escape") {
          e.preventDefault();
          settings.closeSettings();
        }
        return;
      }

      const store = useChatStore.getState();

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
        // Esc closes the split panel first; only if it's already closed does
        // Esc fall through to cancelling the active turn.
        const split = useSplitStore.getState();
        if (split.open) {
          split.closeSplit();
          return;
        }
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
      <main className="flex min-w-0 flex-1">
        {/* Chat column. When the split is closed it stays full-width — no
            visual change from before the panel existed. */}
        <div className="flex min-w-0 flex-1 flex-col">
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
        </div>
        <SplitPanel />
      </main>
      <CommandPalette />
      <ShortcutsOverlay />
      <SettingsPanel />
    </div>
  );
}
