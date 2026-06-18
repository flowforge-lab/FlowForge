import type { ComponentType, ReactNode } from "react";
import { useRef, useState } from "react";
import {
  Eye,
  EyeOff,
  MoreHorizontal,
  Moon,
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
import { useTheme } from "@/store/prefs";
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
import { arrangeSessions, filterSessions, resolveLabel } from "@/lib/sessions";
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
  const [showDismissed, setShowDismissed] = useState(false);
  const filterRef = useRef<HTMLInputElement>(null);

  const pinnedSet = new Set(pinnedIds);
  const dismissedSet = new Set(dismissedIds);
  const dismissedCount = sessions.reduce(
    (n, s) => n + (dismissedSet.has(s.id) ? 1 : 0),
    0,
  );

  const filtered = filterSessions(sessions, filter, sessionTitles);
  // Each session's index in the *full* list — keeps the ⌘1–9 hint accurate when
  // the visible list is filtered (the global shortcut indexes the full list).
  const indexById = new Map(sessions.map((s, i) => [s.id, i]));
  const filtering = filter.trim().length > 0;

  // Hide dismissed sessions (unless revealed), then float pinned to the top.
  const visible = arrangeSessions(
    filtered,
    pinnedSet,
    dismissedSet,
    showDismissed,
  );

  return (
    <aside className="flex h-full w-60 shrink-0 flex-col border-r bg-sidebar text-sidebar-foreground">
      <div className="flex h-12 items-center justify-between px-3">
        <span className="text-sm font-semibold tracking-tight">FlowForge</span>
        <div className="flex items-center gap-0.5">
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
          <Button
            variant="ghost"
            size="icon"
            className="size-7 text-muted-foreground hover:text-foreground"
            onClick={openSettings}
            title="Settings"
          >
            <Settings className="size-4" />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            className="size-7 text-muted-foreground hover:text-foreground"
            onClick={newSessionInFocusedPane}
            title="New session (⌘N)"
          >
            <Plus className="size-4" />
          </Button>
        </div>
      </div>

      <Separator />

      {/* Filter box — narrows the list by resolved label, keyboard-first (#19). */}
      <div className="px-2 py-2">
        <div className="flex items-center gap-1.5 rounded-md border bg-background/40 px-2 transition-colors focus-within:border-ring focus-within:ring-2 focus-within:ring-ring/25">
          <Search className="size-3.5 shrink-0 text-muted-foreground/60" />
          <input
            ref={filterRef}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation(); // don't also cancel the active turn
                setFilter("");
                filterRef.current?.blur();
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
              onClick={() => {
                setFilter("");
                filterRef.current?.focus();
              }}
              className="flex size-4 shrink-0 items-center justify-center rounded text-muted-foreground/60 hover:text-foreground"
            >
              <X className="size-3" />
            </button>
          )}
        </div>
      </div>

      <ScrollArea className="flex-1">
        <nav className="flex flex-col gap-px p-1.5">
          {visible.length === 0
            ? filtering && (
                <p className="px-2 py-6 text-center text-[12px] text-muted-foreground/60">
                  No sessions match “{filter.trim()}”
                </p>
              )
            : visible.map((session) => (
                <SessionItem
                  key={session.id}
                  session={session}
                  index={indexById.get(session.id) ?? 0}
                  active={session.id === activeSessionId}
                  streaming={Boolean(streamingBySession[session.id])}
                  pinned={pinnedSet.has(session.id)}
                  dismissed={dismissedSet.has(session.id)}
                />
              ))}
        </nav>
      </ScrollArea>

      {dismissedCount > 0 && (
        <button
          type="button"
          onClick={() => setShowDismissed((v) => !v)}
          className="mx-1.5 mb-1 flex items-center gap-1.5 rounded-md px-2 py-1 text-left text-[11px] text-muted-foreground/70 transition-colors hover:bg-sidebar-accent/50 hover:text-foreground"
        >
          {showDismissed ? (
            <Eye className="size-3 shrink-0" />
          ) : (
            <EyeOff className="size-3 shrink-0" />
          )}
          {showDismissed
            ? "Hide dismissed"
            : `Show dismissed (${dismissedCount})`}
        </button>
      )}

      <Separator />
      <div className="px-3 py-2 text-[11px] text-muted-foreground/60">
        ⌘K palette · ⌘N new · ⌘1–9 jump · Esc stop
      </div>
    </aside>
  );
}
