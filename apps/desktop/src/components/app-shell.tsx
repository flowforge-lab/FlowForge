import { useEffect } from "react";
import { AlertTriangle } from "@/components/ui/icon";
import { SessionSidebar } from "@/components/session-sidebar";
import { PaneTree } from "@/components/pane-tree";
import { SplitPanel } from "@/components/split-panel";
import { CommandPalette } from "@/components/palette";
import { ShortcutsOverlay } from "@/components/shortcuts-overlay";
import { SettingsPanel } from "@/components/settings-panel";
import { PhenoMcpToast } from "@/components/pheno-mcp-toast";
import { UpdateBar } from "@/components/update-bar";
import { useChatStore } from "@/store/chat";
import { usePrefsStore } from "@/store/prefs";
import { usePanesStore } from "@/store/panes";
import { useSplitStore } from "@/store/split";
import { usePaletteStore } from "@/store/palette";
import { useSettingsStore } from "@/store/settings";
import { useShortcutsStore } from "@/store/shortcuts";
import { useSessionModeStore } from "@/store/session-mode";
import { modeForHotkey } from "@/lib/mode";

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

      const panes = usePanesStore.getState();

      if (mod && e.key.toLowerCase() === "n") {
        e.preventDefault();
        // New session lands in the focused pane (keeps the layout; the pane just
        // swaps to a fresh session). Falls back to a plain new session if panes
        // aren't initialized yet.
        void store.newSession().then(() => {
          const id = useChatStore.getState().activeSessionId;
          const focused = usePanesStore.getState().focusedPaneId;
          if (id && focused)
            usePanesStore.getState().setPaneSession(focused, id);
        });
        return;
      }
      // Cycle the focused pane's agent mode (#266): Plan → Act → Auto. The active
      // session mirrors the focused pane, so cycling it targets where the user is.
      if (mod && e.key === ".") {
        const sid = store.activeSessionId;
        if (sid) {
          e.preventDefault();
          useSessionModeStore
            .getState()
            .cycleMode(sid, usePrefsStore.getState().defaultMode);
        }
        return;
      }
      // Direct mode hotkeys (#267): ⌘P → Plan, ⌘T → Act, ⌘O → Auto on the focused
      // pane's session. (In a browser these would print / new-tab / open; in the
      // Tauri app they're free, and preventDefault keeps the mock exercisable.)
      const directMode =
        mod && !e.shiftKey && !e.altKey ? modeForHotkey(e.key) : undefined;
      if (directMode) {
        const sid = store.activeSessionId;
        if (sid) {
          e.preventDefault();
          useSessionModeStore.getState().setMode(sid, directMode);
        }
        return;
      }
      if (mod && e.key >= "1" && e.key <= "9") {
        const target = store.sessions[Number(e.key) - 1];
        if (target) {
          e.preventDefault();
          // Jump loads the session into the focused pane (not a global switch),
          // so the chosen session shows where the user is looking.
          if (panes.focusedPaneId) {
            panes.setPaneSession(panes.focusedPaneId, target.id);
            void store.loadSession(target.id);
          } else {
            void store.selectSession(target.id);
          }
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

// Build the initial pane layout once sessions are loaded (after bootstrap), or
// rebuild it from persisted state — dropping any panes whose session is gone.
function usePaneInit() {
  const sessions = useChatStore((s) => s.sessions);
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const root = usePanesStore((s) => s.root);
  useEffect(() => {
    if (!root && sessions.length > 0) {
      usePanesStore.getState().init(
        sessions.map((x) => x.id),
        activeSessionId,
      );
    }
  }, [root, sessions, activeSessionId]);
}

export function AppShell() {
  useGlobalShortcuts();
  usePaneInit();
  const bootstrapError = useChatStore((s) => s.bootstrapError);

  return (
    <div className="flex h-full flex-col bg-background text-foreground">
      <UpdateBar />
      <div className="flex min-h-0 flex-1">
        <SessionSidebar />
        <main className="flex min-w-0 flex-1 flex-col">
          <div className="flex min-h-0 min-w-0 flex-1">
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
              <PaneTree />
            </div>
            <SplitPanel />
          </div>
        </main>
      </div>
      <CommandPalette />
      <ShortcutsOverlay />
      <SettingsPanel />
      <PhenoMcpToast />
    </div>
  );
}
