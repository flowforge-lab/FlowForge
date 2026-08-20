import { Plus, X } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { useSessionWorkspaceStore } from "@/store/session-workspace";
import { useTerminalStore, type TerminalTab } from "@/store/terminal";
import { TerminalView } from "./terminal-view";

// The terminal drawer (#1284): the bottom section of a session pane, holding a
// tab strip and one shell per tab, each rooted at *this pane's* session working
// directory.
//
// Scoped to `sessionId`, like the Files panel beside it, so two panes showing
// different sessions get independent drawers with independent shells. All tabs
// stay mounted — see terminal-view.tsx for why a hidden tab must not unmount.

export function TerminalDrawer({ sessionId }: { sessionId: string }) {
  const slice = useTerminalStore((s) => s.bySession[sessionId]);
  const addTab = useTerminalStore((s) => s.addTab);
  const selectTab = useTerminalStore((s) => s.selectTab);
  const closeTab = useTerminalStore((s) => s.closeTab);
  const closeDrawer = useTerminalStore((s) => s.closeDrawer);
  // The folder the shells are rooted at, for the tab label — the same workspace
  // the composer's selector shows, already cached by the pane.
  const workspace = useSessionWorkspaceStore((s) => s.bySession[sessionId]);
  const folder = workspace?.path.split("/").filter(Boolean).pop();

  const tabs = slice?.tabs ?? [];
  const activeTabId = slice?.activeTabId ?? null;

  /** What a tab is called. Every tab in a pane is rooted at the same directory,
   *  so the folder alone leaves two shells looking identical (#1287 review) --
   *  the shell number is what tells them apart, and it is what the close button
   *  and its tooltip name too. */
  const tabLabel = (tab: TerminalTab) =>
    `${folder ?? "shell"} ${tab.shellNumber}`;

  return (
    <div className="flex h-full w-full min-w-0 flex-col overflow-hidden border-t bg-card">
      <div className="flex h-8 shrink-0 items-center gap-1 border-b px-1.5">
        <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
          {tabs.map((tab) => {
            const active = tab.tabId === activeTabId;
            return (
              <div
                key={tab.tabId}
                className={cn(
                  "group flex shrink-0 items-center gap-1 rounded px-2 py-0.5 text-xs transition-colors",
                  active
                    ? "bg-foreground/10 text-foreground"
                    : "text-muted-foreground hover:bg-foreground/5",
                )}
              >
                <button
                  type="button"
                  aria-pressed={active}
                  title={
                    workspace
                      ? `shell ${tab.shellNumber} — ${workspace.path}`
                      : tabLabel(tab)
                  }
                  onClick={() => selectTab(sessionId, tab.tabId)}
                  className="max-w-40 truncate"
                >
                  {/* The exited marker keeps a dead tab honest: its output is
                      still readable, but nothing more will arrive. */}
                  {tabLabel(tab)}
                  {tab.exited && " (exited)"}
                </button>
                <button
                  type="button"
                  title="Close terminal"
                  aria-label={`Close ${tabLabel(tab)}`}
                  onClick={() => closeTab(sessionId, tab.tabId)}
                  className="flex size-4 items-center justify-center rounded text-muted-foreground opacity-0 transition-opacity hover:bg-foreground/10 hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100"
                >
                  <X className="size-3" />
                </button>
              </div>
            );
          })}
          <button
            type="button"
            title="New terminal"
            aria-label="New terminal"
            onClick={() => addTab(sessionId)}
            className="flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground transition-colors hover:bg-foreground/10 hover:text-foreground"
          >
            <Plus className="size-3.5" />
          </button>
        </div>
        <button
          type="button"
          title="Close terminal panel (⌘J)"
          aria-label="Close terminal panel"
          onClick={() => closeDrawer(sessionId)}
          className="flex size-6 shrink-0 items-center justify-center rounded text-muted-foreground transition-colors hover:bg-foreground/10 hover:text-foreground"
        >
          <X className="size-3.5" />
        </button>
      </div>

      {/* `px-2 pt-1` and no gap below: xterm measures its host, so any padding
          it cannot see would clip the last row. */}
      <div className="min-h-0 flex-1 px-2 pt-1">
        {tabs.map((tab) => (
          <TerminalView
            key={tab.tabId}
            sessionId={sessionId}
            tabId={tab.tabId}
            terminalId={tab.terminalId}
            visible={tab.tabId === activeTabId}
          />
        ))}
      </div>
    </div>
  );
}
