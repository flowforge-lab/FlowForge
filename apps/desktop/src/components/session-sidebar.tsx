import type { ComponentType, ReactNode } from "react";
import { useEffect, useRef, useState } from "react";
import {
  EyeOff,
  Folder,
  MoreHorizontal,
  Moon,
  PanelLeft,
  Pencil,
  Pin,
  PinOff,
  Plus,
  RotateCcw,
  Search,
  Settings,
  SplitSquareHorizontal,
  SplitSquareVertical,
  Sun,
  Trash2,
  X,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { cn } from "@/lib/utils";
import { resolveEffectiveTheme } from "@/lib/theme";
import { useTheme, usePrefsStore } from "@/store/prefs";
import { useSettingsStore } from "@/store/settings";
import { useChatStore } from "@/store/chat";
import { useSessionPrefsStore } from "@/store/session-prefs";
import { usePanesStore, MAX_PANES } from "@/store/panes";
import {
  ContextMenu,
  ContextMenuTrigger,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSub,
  ContextMenuSubTrigger,
  ContextMenuSubContent,
  ContextMenuSeparator,
} from "@/components/ui/context-menu";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSub,
  DropdownMenuSubTrigger,
  DropdownMenuSubContent,
  DropdownMenuSeparator,
} from "@/components/ui/dropdown-menu";
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogHeader,
  AlertDialogFooter,
  AlertDialogTitle,
  AlertDialogDescription,
  AlertDialogAction,
  AlertDialogCancel,
} from "@/components/ui/alert-dialog";
import {
  arrangeSessions,
  filterSessions,
  resolveLabel,
  selectSessionOverflow,
} from "@/lib/sessions";
import type { SessionListTab } from "@/lib/sessions";
import { SegmentedControl } from "@/components/settings/segmented-control";
import type { Session } from "@/bindings";

// ── Shared menu body ─────────────────────────────────────────────────────────
// The right-click ContextMenu and the ⋯ DropdownMenu render the identical item
// list, so it lives once here and is parameterized by the menu primitive's parts
// (radix ContextMenu/DropdownMenu expose the same item/sub/separator shapes).

interface MenuParts {
  Item: ComponentType<{
    onSelect?: (event: Event) => void;
    disabled?: boolean;
    className?: string;
    children?: ReactNode;
  }>;
  Sub: ComponentType<{ children?: ReactNode }>;
  SubTrigger: ComponentType<{ disabled?: boolean; children?: ReactNode }>;
  SubContent: ComponentType<{ children?: ReactNode }>;
  Separator: ComponentType;
}

const CONTEXT_PARTS: MenuParts = {
  Item: ContextMenuItem,
  Sub: ContextMenuSub,
  SubTrigger: ContextMenuSubTrigger,
  SubContent: ContextMenuSubContent,
  Separator: ContextMenuSeparator,
};

const DROPDOWN_PARTS: MenuParts = {
  Item: DropdownMenuItem,
  Sub: DropdownMenuSub,
  SubTrigger: DropdownMenuSubTrigger,
  SubContent: DropdownMenuSubContent,
  Separator: DropdownMenuSeparator,
};

interface SessionMenuItemsProps {
  parts: MenuParts;
  atCap: boolean;
  pinned: boolean;
  dismissed: boolean;
  onOpen: () => void;
  onOpenSplit: (dir: "vertical" | "horizontal") => void;
  onTogglePin: () => void;
  onDismissToggle: () => void;
  onRename: () => void;
  onDelete: () => void;
}

export function SessionMenuItems({
  parts: P,
  atCap,
  pinned,
  dismissed,
  onOpen,
  onOpenSplit,
  onTogglePin,
  onDismissToggle,
  onRename,
  onDelete,
}: SessionMenuItemsProps) {
  return (
    <>
      <P.Item onSelect={onOpen}>Open</P.Item>
      <P.Sub>
        <P.SubTrigger disabled={atCap}>Open in split</P.SubTrigger>
        <P.SubContent>
          <P.Item onSelect={() => onOpenSplit("vertical")}>
            <SplitSquareHorizontal />
            Right
          </P.Item>
          <P.Item onSelect={() => onOpenSplit("horizontal")}>
            <SplitSquareVertical />
            Down
          </P.Item>
        </P.SubContent>
      </P.Sub>
      <P.Separator />
      <P.Item onSelect={onTogglePin}>
        {pinned ? <PinOff /> : <Pin />}
        {pinned ? "Unpin" : "Pin"}
      </P.Item>
      <P.Item onSelect={onDismissToggle}>
        {dismissed ? <RotateCcw /> : <EyeOff />}
        {dismissed ? "Restore" : "Dismiss"}
      </P.Item>
      <P.Separator />
      <P.Item onSelect={onRename}>
        <Pencil />
        Rename
      </P.Item>
      <P.Separator />
      <P.Item
        onSelect={onDelete}
        className="text-destructive focus:text-destructive"
      >
        <Trash2 />
        Delete
      </P.Item>
    </>
  );
}

// ── Inline-rename session item ───────────────────────────────────────────────

export function SessionItem({
  session,
  index,
  active,
  streaming,
  pinned,
  dismissed,
}: {
  session: Session;
  index: number;
  active: boolean;
  streaming: boolean;
  pinned: boolean;
  dismissed: boolean;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  const sessionTitles = useChatStore((s) => s.sessionTitles);
  const selectSession = useChatStore((s) => s.selectSession);
  const loadSession = useChatStore((s) => s.loadSession);
  const setSessionTitle = useChatStore((s) => s.setSessionTitle);
  const deleteSession = useChatStore((s) => s.deleteSession);
  const atCap = usePanesStore((s) => s.leafCount() >= MAX_PANES);
  const togglePin = useSessionPrefsStore((s) => s.togglePin);
  const dismiss = useSessionPrefsStore((s) => s.dismiss);
  const restore = useSessionPrefsStore((s) => s.restore);

  // Shared by both menus and the inline pencil.
  const onTogglePin = () => togglePin(session.id);
  const onDismissToggle = () =>
    dismissed ? restore(session.id) : dismiss(session.id);
  const onRename = () => setEditing(true);
  // A menu item closes its menu on select, so defer the confirm to the next tick
  // to avoid the dialog and the closing menu fighting over focus.
  const onDelete = () => setConfirmingDelete(true);

  const currentLabel = resolveLabel(session, sessionTitles[session.id]);

  // Click loads the session into the focused pane (#148) so it appears where the
  // user is looking; falls back to a global switch if panes aren't initialized.
  function open() {
    const focused = usePanesStore.getState().focusedPaneId;
    if (focused) {
      usePanesStore.getState().setPaneSession(focused, session.id);
      void loadSession(session.id);
    } else {
      void selectSession(session.id);
    }
  }

  // "Open in Split" splits the focused pane and drops this session into the new
  // (focused) pane. No-op (and hidden) at the pane cap.
  function openInSplit(dir: "vertical" | "horizontal") {
    const focused = usePanesStore.getState().focusedPaneId;
    if (!focused) return;
    if (dir === "vertical")
      usePanesStore.getState().splitRight(focused, session.id);
    else usePanesStore.getState().splitDown(focused, session.id);
    void loadSession(session.id);
  }

  function startEditing(e: React.MouseEvent) {
    e.stopPropagation(); // don't also trigger session select
    setDraft(currentLabel === "New session" ? "" : currentLabel);
    setEditing(true);
    // Focus the input after state update.
    setTimeout(() => inputRef.current?.select(), 0);
  }

  function commitRename() {
    const trimmed = draft.trim();
    if (trimmed && trimmed !== currentLabel) {
      setSessionTitle(session.id, trimmed);
    }
    setEditing(false);
  }

  function cancelRename() {
    setEditing(false);
  }

  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>
        <div
          role="button"
          tabIndex={0}
          onClick={() => !editing && open()}
          onKeyDown={(e) => {
            if (!editing && (e.key === "Enter" || e.key === " ")) {
              e.preventDefault();
              open();
            }
          }}
          className={cn(
            "group relative flex items-center gap-1.5 rounded-md px-2 py-1.5 text-left text-[13px] transition-colors cursor-pointer select-none",
            active
              ? "bg-sidebar-accent text-sidebar-accent-foreground"
              : "text-muted-foreground hover:bg-sidebar-accent/50 hover:text-foreground",
            dismissed && "opacity-60",
          )}
        >
          {editing ? (
            /* ── Rename input ── */
            <input
              ref={inputRef}
              autoFocus
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onBlur={commitRename}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  commitRename();
                } else if (e.key === "Escape") {
                  e.preventDefault();
                  cancelRename();
                }
                e.stopPropagation();
              }}
              onClick={(e) => e.stopPropagation()}
              placeholder="Session name…"
              className="min-w-0 flex-1 truncate bg-transparent text-[13px] text-foreground outline-none placeholder:text-muted-foreground/50"
            />
          ) : (
            /* ── Normal label ── */
            <>
              <span className="min-w-0 flex-1 truncate">{currentLabel}</span>

              {/* Right slot: pin glyph (when pinned) + streaming dot, or the
                  hover actions (pencil, ⋯) / kbd hint when idle. */}
              <span className="flex shrink-0 items-center gap-0.5">
                {pinned && !streaming && (
                  <Pin
                    className="size-3 shrink-0 text-muted-foreground/70"
                    aria-label="Pinned"
                  />
                )}
                {streaming ? (
                  <span className="size-1.5 animate-pulse rounded-full bg-primary" />
                ) : (
                  <>
                    {/* Pencil + ⋯ shown on hover; kbd hint shown when idle. */}
                    <button
                      title="Rename session"
                      onClick={startEditing}
                      className={cn(
                        "hidden size-5 items-center justify-center rounded group-hover:flex",
                        "text-muted-foreground/70 hover:text-foreground hover:bg-sidebar-accent",
                      )}
                    >
                      <Pencil className="size-3" />
                    </button>
                    <DropdownMenu>
                      <DropdownMenuTrigger asChild>
                        <button
                          title="Session actions"
                          aria-label="Session actions"
                          onClick={(e) => e.stopPropagation()}
                          onKeyDown={(e) => e.stopPropagation()}
                          className={cn(
                            "hidden size-5 items-center justify-center rounded group-hover:flex data-[state=open]:flex",
                            "text-muted-foreground/70 hover:text-foreground hover:bg-sidebar-accent",
                          )}
                        >
                          <MoreHorizontal className="size-3" />
                        </button>
                      </DropdownMenuTrigger>
                      <DropdownMenuContent align="end">
                        <SessionMenuItems
                          parts={DROPDOWN_PARTS}
                          atCap={atCap}
                          pinned={pinned}
                          dismissed={dismissed}
                          onOpen={open}
                          onOpenSplit={openInSplit}
                          onTogglePin={onTogglePin}
                          onDismissToggle={onDismissToggle}
                          onRename={onRename}
                          onDelete={onDelete}
                        />
                      </DropdownMenuContent>
                    </DropdownMenu>
                    {index < 9 && (
                      <kbd className="font-mono text-[10px] text-muted-foreground/50 group-hover:hidden">
                        ⌘{index + 1}
                      </kbd>
                    )}
                  </>
                )}
              </span>
            </>
          )}
        </div>
      </ContextMenuTrigger>
      <ContextMenuContent>
        <SessionMenuItems
          parts={CONTEXT_PARTS}
          atCap={atCap}
          pinned={pinned}
          dismissed={dismissed}
          onOpen={open}
          onOpenSplit={openInSplit}
          onTogglePin={onTogglePin}
          onDismissToggle={onDismissToggle}
          onRename={onRename}
          onDelete={onDelete}
        />
      </ContextMenuContent>

      <AlertDialog open={confirmingDelete} onOpenChange={setConfirmingDelete}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete session?</AlertDialogTitle>
            <AlertDialogDescription>
              This permanently removes &ldquo;{currentLabel}&rdquo; and its
              transcript. This can&rsquo;t be undone. (To hide a session without
              losing it, use Dismiss.)
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction onClick={() => void deleteSession(session.id)}>
              Delete
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </ContextMenu>
  );
}

// ── Sidebar ──────────────────────────────────────────────────────────────────

export function SessionSidebar() {
  const sessions = useChatStore((s) => s.sessions);
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const streamingBySession = useChatStore((s) => s.streamingBySession);
  const sessionTitles = useChatStore((s) => s.sessionTitles);
  const newSession = useChatStore((s) => s.newSession);

  const sidebarCollapsed = usePrefsStore((s) => s.sidebarCollapsed);
  const setSidebarCollapsed = usePrefsStore((s) => s.setSidebarCollapsed);

  // New session lands in the focused pane (#148) so the layout is preserved —
  // matches app-shell's Cmd+N and the palette. Falls back to a plain new session
  // when panes aren't initialized yet.
  function newSessionInFocusedPane() {
    void newSession().then(() => {
      const id = useChatStore.getState().activeSessionId;
      const focused = usePanesStore.getState().focusedPaneId;
      if (id && focused) usePanesStore.getState().setPaneSession(focused, id);
    });
  }
  const theme = useTheme((s) => s.theme);
  const toggleTheme = useTheme((s) => s.toggleTheme);
  const openSettings = useSettingsStore((s) => s.openSettings);
  const effectiveTheme = resolveEffectiveTheme(theme);

  const pinnedIds = useSessionPrefsStore((s) => s.pinned);
  const dismissedIds = useSessionPrefsStore((s) => s.dismissed);

  const [filter, setFilter] = useState("");
  const [listTab, setListTab] = useState<SessionListTab>("all");
  const [showFilter, setShowFilter] = useState(false);
  const [overflowExpanded, setOverflowExpanded] = useState(false);
  const filterRef = useRef<HTMLInputElement>(null);

  function revealFilter() {
    setShowFilter(true);
  }

  // Focus the filter after it renders (idiomatic — no setState here, so the
  // set-state-in-effect rule doesn't apply).
  useEffect(() => {
    if (showFilter) filterRef.current?.focus();
  }, [showFilter]);

  function hideFilter() {
    setShowFilter(false);
    setFilter("");
    filterRef.current?.blur();
  }

  const pinnedSet = new Set(pinnedIds);
  const dismissedSet = new Set(dismissedIds);
  const dismissedCount = sessions.reduce(
    (n, s) => n + (dismissedSet.has(s.id) ? 1 : 0),
    0,
  );

  // Restoring the last dismissed session disables the Dismissed tab; fall back to
  // All so a disabled tab is never left selected (stranding "No dismissed sessions").
  const effectiveTab: SessionListTab =
    listTab === "dismissed" && dismissedCount === 0 ? "all" : listTab;

  // Switching tab or editing the filter resets the overflow expansion so a stale
  // "expanded" state can't carry across lists. Done in the handlers (not an effect)
  // to keep clear of the set-state-in-effect rule.
  const switchTab = (next: SessionListTab) => {
    setListTab(next);
    setOverflowExpanded(false);
  };
  const changeFilter = (next: string) => {
    setFilter(next);
    setOverflowExpanded(false);
  };

  const filtered = filterSessions(sessions, filter, sessionTitles);
  // Each session's index in the *full* list — keeps the ⌘1–9 hint accurate when
  // the visible list is filtered (the global shortcut indexes the full list).
  const indexById = new Map(sessions.map((s, i) => [s.id, i]));
  const filtering = filter.trim().length > 0;

  const arranged = arrangeSessions(
    filtered,
    pinnedSet,
    dismissedSet,
    effectiveTab,
  );
  const { visible, hiddenCount } = selectSessionOverflow(
    arranged,
    pinnedSet,
    activeSessionId,
    overflowExpanded,
  );
  // Whether overflow exists at all (independent of the current expansion), so the
  // toggle can offer "show less" once expanded.
  const hasOverflow =
    selectSessionOverflow(arranged, pinnedSet, activeSessionId, false)
      .hiddenCount > 0;

  return (
    <aside
      className={cn(
        "flex h-full shrink-0 flex-col border-r bg-sidebar text-sidebar-foreground transition-[width,border] duration-200",
        sidebarCollapsed
          ? "w-0 overflow-hidden border-r-0"
          : "w-60 overflow-hidden",
      )}
      aria-hidden={sidebarCollapsed}
      // `aria-hidden` alone leaves descendants focusable; `inert` also pulls the
      // collapsed sidebar's buttons/input out of the tab order (axe aria-hidden-focus).
      inert={sidebarCollapsed || undefined}
    >
      <div className="flex h-12 items-center justify-between px-3">
        <span className="text-sm font-semibold tracking-tight">FlowForge</span>
        <div className="flex items-center gap-0.5">
          <Button
            variant="ghost"
            size="icon"
            className="size-7 text-muted-foreground hover:text-foreground"
            onClick={() => setSidebarCollapsed(true)}
            title="Collapse sidebar"
          >
            <PanelLeft className="size-4" />
          </Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className="size-7 text-muted-foreground hover:text-foreground"
                title="Sidebar options"
                aria-label="Sidebar options"
              >
                <MoreHorizontal className="size-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={revealFilter}>
                <Search />
                Search
              </DropdownMenuItem>
              <DropdownMenuItem disabled title="Coming soon">
                <Folder />
                Folder view
              </DropdownMenuItem>
              <DropdownMenuSub>
                <DropdownMenuSubTrigger disabled title="Coming soon">
                  Filter by source
                </DropdownMenuSubTrigger>
                <DropdownMenuSubContent>
                  <DropdownMenuItem disabled>Coming soon</DropdownMenuItem>
                </DropdownMenuSubContent>
              </DropdownMenuSub>
            </DropdownMenuContent>
          </DropdownMenu>
          <Button
            variant="ghost"
            size="icon"
            className="size-7 text-muted-foreground hover:text-foreground"
            onClick={toggleTheme}
            title={
              effectiveTheme === "light"
                ? "Switch to dark theme"
                : "Switch to light theme"
            }
          >
            {effectiveTheme === "light" ? (
              <Moon className="size-4" />
            ) : (
              <Sun className="size-4" />
            )}
          </Button>
          {/* Primary action — accent-filled so "new session" reads as the
              prominent control (reference design). */}
          <Button
            size="icon"
            className="size-7 bg-emerald-600 text-white hover:bg-emerald-600/90"
            onClick={newSessionInFocusedPane}
            title="New session (⌘N)"
            aria-label="New session"
          >
            <Plus className="size-4" />
          </Button>
        </div>
      </div>

      <Separator />

      <div className="flex justify-end px-2 py-2">
        <SegmentedControl
          label="Session list"
          value={effectiveTab}
          onValueChange={(v) => switchTab(v as SessionListTab)}
          options={[
            { value: "all", label: "All" },
            {
              value: "dismissed",
              label: `Dismissed (${dismissedCount})`,
              disabled: dismissedCount === 0,
            },
          ]}
        />
      </div>

      {showFilter && (
        <div className="px-2 pb-2">
          <div className="flex items-center gap-1.5 rounded-md border bg-background/40 px-2 transition-colors focus-within:border-ring focus-within:ring-2 focus-within:ring-ring/25">
            <Search className="size-3.5 shrink-0 text-muted-foreground/60" />
            <input
              ref={filterRef}
              value={filter}
              onChange={(e) => changeFilter(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  e.preventDefault();
                  e.stopPropagation();
                  hideFilter();
                }
              }}
              placeholder="Filter sessions…"
              aria-label="Filter sessions"
              spellCheck={false}
              className="h-7 min-w-0 flex-1 bg-transparent text-[13px] text-foreground outline-none placeholder:text-muted-foreground/50"
            />
            {filtering && (
              <button
                type="button"
                title="Clear filter (Esc)"
                onClick={() => hideFilter()}
                className="flex size-4 shrink-0 items-center justify-center rounded text-muted-foreground/60 hover:text-foreground"
              >
                <X className="size-3" />
              </button>
            )}
          </div>
        </div>
      )}

      <ScrollArea className="flex-1">
        <nav className="flex flex-col gap-px p-1.5">
          {visible.length === 0 ? (
            filtering && effectiveTab === "all" ? (
              <p className="px-2 py-6 text-center text-[12px] text-muted-foreground/60">
                No sessions match “{filter.trim()}”
              </p>
            ) : effectiveTab === "dismissed" ? (
              <p className="px-2 py-6 text-center text-[12px] text-muted-foreground/60">
                No dismissed sessions
              </p>
            ) : null
          ) : (
            visible.map((session) => (
              <SessionItem
                key={session.id}
                session={session}
                index={indexById.get(session.id) ?? 0}
                active={session.id === activeSessionId}
                streaming={Boolean(streamingBySession[session.id])}
                pinned={pinnedSet.has(session.id)}
                dismissed={dismissedSet.has(session.id)}
              />
            ))
          )}
          {hasOverflow && (
            <button
              type="button"
              onClick={() => setOverflowExpanded((v) => !v)}
              className="mx-0.5 rounded-md px-2 py-1.5 text-left text-[12px] text-muted-foreground/70 transition-colors hover:bg-sidebar-accent/50 hover:text-foreground"
            >
              {overflowExpanded ? "‹ Show less" : `› ${hiddenCount} more`}
            </button>
          )}
        </nav>
      </ScrollArea>

      <Separator />
      <button
        type="button"
        onClick={openSettings}
        title="Settings"
        className="flex items-center gap-2 px-3 py-2.5 text-left text-[12px] text-muted-foreground transition-colors hover:bg-sidebar-accent/50 hover:text-foreground"
      >
        <Settings className="size-4 shrink-0" />
        Settings
      </button>
    </aside>
  );
}
