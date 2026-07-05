import type { ComponentType, ReactNode } from "react";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  Check,
  Download,
  EyeOff,
  Folder,
  MoreHorizontal,
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
  SquareCheck,
  Trash2,
  X,
} from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { EmptyState } from "@/components/ui/empty-state";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { cn } from "@/lib/utils";
import { exportSessionToFile } from "@/lib/export-session";
import type { Format } from "@/bindings/Format";
import { usePrefsStore, clampSidebarWidth } from "@/store/prefs";
import { useSettingsStore } from "@/store/settings";
import { useChatStore } from "@/store/chat";
import { useSessionPrefsStore } from "@/store/session-prefs";
import { useSessionDoneToastStore } from "@/store/session-done-toast";
import { useFindStore } from "@/store/find";
import { usePanesStore, MAX_PANES } from "@/store/panes";
import { ipc } from "@/lib/ipc";
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
  groupContentHits,
  resolveLabel,
  sanitizeSnippet,
  selectSessionOverflow,
  SESSION_REVEAL_BATCH,
} from "@/lib/sessions";
import type { Session } from "@/bindings";
import type { SearchHit } from "@/bindings/SearchHit";

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

// Shared class for the flat ghost icon buttons in the sidebar header and
// collapsed rail (#674): muted by default, foreground on hover.
const RAIL_ICON_BTN = "size-7 text-muted-foreground hover:text-foreground";

// Cross-session content search (#710): debounce keystrokes before hitting the
// FTS backend, and cap how many hits we group into sidebar rows.
const CONTENT_SEARCH_DEBOUNCE_MS = 200;
const CONTENT_HIT_LIMIT = 30;

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
  onExport: (format: Format) => void;
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
  onExport,
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
      <P.Sub>
        <P.SubTrigger>
          <Download />
          Export
        </P.SubTrigger>
        <P.SubContent>
          <P.Item onSelect={() => onExport("markdown")}>Markdown (.md)</P.Item>
          <P.Item onSelect={() => onExport("json")}>JSON (.json)</P.Item>
        </P.SubContent>
      </P.Sub>
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

// ── Content search-hit row (#710) ────────────────────────────────────────────
// A session surfaced by full-text search (not a title match): its label plus the
// backend's `<mark>`-highlighted snippet. Clicking opens the session and jumps to
// the matched message. Intentionally lean — no pin/menu/hover chrome.

export function ContentHitRow({
  session,
  snippet,
  onOpen,
}: {
  session: Session;
  snippet: string;
  onOpen: () => void;
}) {
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onOpen}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen();
        }
      }}
      className="group flex cursor-pointer select-none flex-col gap-0.5 rounded-md px-2 py-1.5 text-left text-muted-foreground transition-colors hover:bg-sidebar-accent/50 hover:text-foreground"
    >
      <span className="min-w-0 truncate text-[13px] text-foreground/90">
        {resolveLabel(session)}
      </span>
      {/* Snippet is pre-highlighted with <mark> by the backend; `ff-hit-snippet`
          styles the marks (see index.css). Sanitized first — the backend does not
          escape the surrounding message text, which can contain raw HTML (#747 C1). */}
      <span
        className="ff-hit-snippet min-w-0 truncate text-[11px] text-muted-foreground/80"
        dangerouslySetInnerHTML={{ __html: sanitizeSnippet(snippet) }}
      />
    </div>
  );
}

// ── Inline-rename session item ───────────────────────────────────────────────

export function SessionItem({
  session,
  index,
  active,
  streaming,
  finished,
  pinned,
  dismissed,
  selectMode = false,
  selected = false,
  onToggleSelect,
}: {
  session: Session;
  index: number;
  active: boolean;
  streaming: boolean;
  /** A turn just finished on this (background) session (#703): show the transient
   *  "done" checkmark. Mutually exclusive with `streaming`. */
  finished: boolean;
  pinned: boolean;
  dismissed: boolean;
  /** Multi-select mode (#643): show a checkbox and toggle selection on row click
   *  instead of opening the session. */
  selectMode?: boolean;
  selected?: boolean;
  onToggleSelect?: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

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

  const currentLabel = resolveLabel(session);

  // Export to Markdown/JSON (#278): the backend serializes, the helper writes the
  // user-chosen file, and we surface the outcome as a transient toast.
  async function onExport(format: Format) {
    try {
      const result = await exportSessionToFile(
        session.id,
        currentLabel === "New session" ? null : currentLabel,
        format,
      );
      if (result.status === "saved") {
        setToast(`Exported to ${result.path}`);
      }
    } catch (err) {
      setToast(
        `Export failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  }

  // Auto-dismiss the toast (mirrors the about-section confirmation pattern).
  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => setToast(null), 3000);
    return () => clearTimeout(t);
  }, [toast]);

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
          onClick={() => {
            if (editing) return;
            if (selectMode) onToggleSelect?.();
            else open();
          }}
          onKeyDown={(e) => {
            if (!editing && (e.key === "Enter" || e.key === " ")) {
              e.preventDefault();
              if (selectMode) onToggleSelect?.();
              else open();
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
          {selectMode && !editing && (
            <input
              type="checkbox"
              checked={selected}
              onChange={() => onToggleSelect?.()}
              // Stop the click from also reaching the row handler (which would
              // toggle a second time and cancel this one out).
              onClick={(e) => e.stopPropagation()}
              aria-label={`Select ${currentLabel}`}
              className="size-3.5 shrink-0 accent-primary"
            />
          )}
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
                {pinned && !streaming && !finished && (
                  <Pin
                    className="size-3 shrink-0 text-muted-foreground/70"
                    aria-label="Pinned"
                  />
                )}
                {/* Right-slot priority (#703): streaming spinner > done checkmark
                    > idle (pin/hover actions). */}
                {streaming ? (
                  <Spinner size="sm" className="text-primary" />
                ) : finished ? (
                  <Check
                    className="ff-fade-in size-3.5 shrink-0 text-emerald-500"
                    aria-label="Finished"
                  />
                ) : selectMode ? null : (
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
                          onExport={onExport}
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
          onExport={onExport}
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

      {toast ? (
        <div
          role="status"
          className="fixed bottom-4 left-4 z-50 max-w-xs rounded-md border border-border bg-popover px-3 py-2 text-[12px] text-popover-foreground shadow-md"
        >
          {toast}
        </div>
      ) : null}
    </ContextMenu>
  );
}

// ── Sidebar ──────────────────────────────────────────────────────────────────

export function SessionSidebar() {
  const allSessions = useChatStore((s) => s.sessions);
  const draftSessionIds = useChatStore((s) => s.draftSessionIds);
  // Hide not-yet-persisted drafts (#671 item 2): a fresh `＋` session shows in its
  // pane but stays out of the list until its first message persists it.
  const sessions = useMemo(
    () => allSessions.filter((s) => !draftSessionIds.has(s.id)),
    [allSessions, draftSessionIds],
  );
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const streamingBySession = useChatStore((s) => s.streamingBySession);
  const recentlyFinishedBySession = useChatStore(
    (s) => s.recentlyFinishedBySession,
  );
  const clearSessionFinished = useChatStore((s) => s.clearSessionFinished);
  const dismissDoneToast = useSessionDoneToastStore((s) => s.dismissBySession);
  const newSession = useChatStore((s) => s.newSession);

  // Focus clears a session's activity signals (#703): the moment a session
  // becomes active — via a row click, the toast's "View", or any other path —
  // drop its transient "done" checkmark and any standing completion toast, since
  // the user is now looking at it. Centralized here so it fires regardless of
  // which action set `activeSessionId`.
  useEffect(() => {
    if (!activeSessionId) return;
    clearSessionFinished(activeSessionId);
    dismissDoneToast(activeSessionId);
  }, [activeSessionId, clearSessionFinished, dismissDoneToast]);

  const sidebarCollapsed = usePrefsStore((s) => s.sidebarCollapsed);
  const setSidebarCollapsed = usePrefsStore((s) => s.setSidebarCollapsed);
  const sidebarWidth = usePrefsStore((s) => s.sidebarWidth);
  const setSidebarWidth = usePrefsStore((s) => s.setSidebarWidth);
  const asideRef = useRef<HTMLElement>(null);

  // Drag-to-resize the sidebar↔chat boundary (#204). The sidebar is flush to the
  // window's left edge, so its width is just the cursor's distance from that edge.
  // Mirrors split-panel.tsx: width is applied imperatively during the drag (no React
  // render / no localStorage write per mousemove) and committed to prefs once on
  // mouseup. The width transition is suspended mid-drag so it tracks the cursor.
  function startResize(e: React.MouseEvent) {
    e.preventDefault();
    const aside = asideRef.current;
    let latest = sidebarWidth;
    if (aside) aside.style.transition = "none";
    const onMove = (ev: MouseEvent) => {
      latest = clampSidebarWidth(ev.clientX);
      if (aside) aside.style.width = `${latest}px`;
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      document.body.style.userSelect = "";
      document.body.style.cursor = "";
      if (aside) aside.style.transition = "";
      setSidebarWidth(latest);
    };
    document.body.style.userSelect = "none";
    document.body.style.cursor = "col-resize";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }

  // The + button swaps the focused pane to a fresh session in place (#671 item 1),
  // leaving every other pane untouched — no new split, so panes that already fill
  // the width never get squeezed into a clipped extra column. With no panes open
  // yet, newSession() still sets the active session, which the single-pane view
  // renders. (The explicit split-pane actions still create splits.)
  function newSessionInFocusedPane() {
    void newSession().then(() => {
      const id = useChatStore.getState().activeSessionId;
      const f = usePanesStore.getState().focusedPaneId;
      if (id && f) usePanesStore.getState().setPaneSession(f, id);
    });
  }
  const openSettings = useSettingsStore((s) => s.openSettings);

  // Open a content-search hit (#710): switch to its session (pane-aware, mirroring
  // the row `open()`), then open the find bar seeded with the query + the hit's
  // messageId so the thread scrolls to and highlights the matched message.
  function openContentHit(hit: SearchHit) {
    const panes = usePanesStore.getState();
    const chat = useChatStore.getState();
    if (panes.focusedPaneId) {
      panes.setPaneSession(panes.focusedPaneId, hit.sessionId);
      void chat.loadSession(hit.sessionId);
    } else {
      void chat.selectSession(hit.sessionId);
    }
    useFindStore.getState().openFind(hit.sessionId, {
      query: filter.trim(),
      messageId: hit.messageId,
    });
  }

  const pinnedIds = useSessionPrefsStore((s) => s.pinned);
  const dismissedIds = useSessionPrefsStore((s) => s.dismissed);
  const dismiss = useSessionPrefsStore((s) => s.dismiss);
  const restore = useSessionPrefsStore((s) => s.restore);
  const deleteSession = useChatStore((s) => s.deleteSession);

  const [filter, setFilter] = useState("");
  const [showFilter, setShowFilter] = useState(false);
  // How many rows the list reveals; grows by SESSION_REVEAL_BATCH per "Show
  // more" click. One endless, incremental reveal replaced the All/Dismissed tabs
  // + fixed cap (#667).
  const [revealCount, setRevealCount] = useState(SESSION_REVEAL_BATCH);
  const filterRef = useRef<HTMLInputElement>(null);

  // Multi-select mode (#643): bulk dismiss/delete across the visible list. State is
  // sidebar-local; bulk handlers reuse the existing per-session store actions.
  const [selectMode, setSelectMode] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirmingBulkDelete, setConfirmingBulkDelete] = useState(false);

  const exitSelect = () => {
    setSelectMode(false);
    setSelected(new Set());
  };
  const toggleSelect = (id: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

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

  // Editing the filter resets the reveal to the first batch so a fresh search
  // starts at the top of the matches (batching applies after filtering). It also
  // drops any selection: a selection is scoped to the view it was made in, and
  // carrying it across a filter change would let a bulk Delete remove now
  // off-screen sessions (#643).
  const changeFilter = (next: string) => {
    setFilter(next);
    setRevealCount(SESSION_REVEAL_BATCH);
    setSelected(new Set());
  };

  const filtered = filterSessions(sessions, filter);
  // Each session's index in the *full* list — keeps the ⌘1–9 hint accurate when
  // the visible list is filtered (the global shortcut indexes the full list).
  const indexById = new Map(sessions.map((s, i) => [s.id, i]));
  const filtering = filter.trim().length > 0;

  // Cross-session full-text search (#710): the same filter box also runs
  // `searchMessages` so a session is findable by what was said in it, not just
  // its title. Debounced, stale-guarded; results render as a separate content
  // group below the title matches.
  const [contentHits, setContentHits] = useState<SearchHit[]>([]);
  // The trimmed query `contentHits` currently reflects — lets us tell "search in
  // flight" from "search returned nothing" so the empty state doesn't flash
  // during the debounce (#747 C2).
  const [searchedFor, setSearchedFor] = useState("");
  useEffect(() => {
    const q = filter.trim();
    let cancelled = false;
    // All state writes stay inside the (async) timer so the effect body never
    // sets state synchronously; an emptied query clears with no delay.
    const timer = window.setTimeout(
      async () => {
        if (!q) {
          setContentHits([]);
          setSearchedFor("");
          return;
        }
        try {
          const hits = await ipc.searchMessages(q, CONTENT_HIT_LIMIT);
          if (!cancelled) {
            setContentHits(hits);
            setSearchedFor(q);
          }
        } catch {
          if (!cancelled) {
            setContentHits([]);
            setSearchedFor(q);
          }
        }
      },
      q ? CONTENT_SEARCH_DEBOUNCE_MS : 0,
    );
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [filter]);
  // A content search is still resolving for the current query — hold the empty
  // state until it settles.
  const contentSearchPending = filtering && filter.trim() !== searchedFor;

  // One row per session, BM25 order, excluding sessions already shown as title
  // matches (and any hit whose session isn't currently listed).
  const sessionsById = useMemo(
    () => new Map(sessions.map((s) => [s.id, s])),
    [sessions],
  );
  const contentRows = filtering
    ? groupContentHits(
        contentHits,
        new Set(filtered.map((s) => s.id)),
        sessionsById,
      )
    : [];

  // Ordered list: pinned → other live → dismissed (dismissed sink to the bottom),
  // then revealed incrementally (#667). The active session is always kept visible.
  const arranged = arrangeSessions(filtered, pinnedSet, dismissedSet);
  const { visible, hasMore } = selectSessionOverflow(
    arranged,
    activeSessionId,
    revealCount,
  );

  // Select mode selects over exactly the revealed rows (B1): Select-all covers
  // what's on screen, and "Show more" still reveals + makes more selectable.
  const rows = visible;
  const rowIds = rows.map((s) => s.id);
  const allSelected =
    rowIds.length > 0 && rowIds.every((id) => selected.has(id));
  const toggleSelectAll = () =>
    setSelected(allSelected ? new Set() : new Set(rowIds));
  // Label the bulk hide/unhide action for the selection: all-dismissed → Restore,
  // otherwise Dismiss (a mixed selection dismisses live rows and restores
  // dismissed ones — see bulkDismiss, A1).
  const selectedAllDismissed =
    selected.size > 0 && [...selected].every((id) => dismissedSet.has(id));

  // Bulk actions run per row over the selection (A1): a dismissed row is
  // restored, a live row is dismissed — so a mixed selection each goes the right
  // way with no mode to pick.
  const bulkDismiss = () => {
    if (selected.size === 0) return;
    for (const id of selected) {
      if (dismissedSet.has(id)) restore(id);
      else dismiss(id);
    }
    exitSelect();
  };
  const bulkDelete = async () => {
    // Sequential: deleteSession snapshots the store each call and re-selects the
    // active session, so parallel deletes would race on that snapshot.
    for (const id of selected) await deleteSession(id);
    setConfirmingBulkDelete(false);
    exitSelect();
  };

  return (
    <aside
      ref={asideRef}
      // Collapsed → a ~48px icon rail (w-12); expanded → persisted px width via
      // inline style (inline wins, so the width class is dropped when expanded).
      style={sidebarCollapsed ? undefined : { width: sidebarWidth }}
      className={cn(
        "relative flex h-full shrink-0 flex-col border-r bg-sidebar text-sidebar-foreground transition-[width,border] duration-200",
        sidebarCollapsed ? "w-12 overflow-hidden" : "overflow-hidden",
      )}
    >
      {sidebarCollapsed ? (
        /* Collapsed rail (#670): a thin icon column — panel-toggle / search / new
           session pinned to the top, settings gear pinned to the bottom. It owns
           the "expand" affordance that used to live in the main pane's header. */
        <div className="flex h-full flex-col items-center gap-1 py-2">
          <Button
            variant="ghost"
            size="icon"
            className={RAIL_ICON_BTN}
            onClick={() => setSidebarCollapsed(false)}
            title="Expand sidebar"
            aria-label="Expand sidebar"
          >
            <PanelLeft className="size-4" />
          </Button>
          {/* Search from the rail (#710): expand the sidebar and focus the filter,
              which now also runs full-text search across sessions. */}
          <Button
            variant="ghost"
            size="icon"
            className={RAIL_ICON_BTN}
            title="Search sessions"
            aria-label="Search sessions"
            onClick={() => {
              setSidebarCollapsed(false);
              revealFilter();
            }}
          >
            <Search className="size-4" />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            className={RAIL_ICON_BTN}
            onClick={newSessionInFocusedPane}
            title="New session"
            aria-label="New session"
          >
            <Plus className="size-4" />
          </Button>
          <div className="flex-1" />
          <Button
            variant="ghost"
            size="icon"
            className={RAIL_ICON_BTN}
            onClick={openSettings}
            title="Settings"
            aria-label="Settings"
          >
            <Settings className="size-4" />
          </Button>
        </div>
      ) : (
        <>
          {/* Drag handle on the right edge (#204). */}
          <div
            onMouseDown={startResize}
            title="Drag to resize"
            aria-hidden
            className="absolute right-0 top-0 z-20 h-full w-1.5 cursor-col-resize hover:bg-primary/30"
          />
          <div className="flex h-12 items-center justify-between gap-1 px-2">
            <Button
              variant="ghost"
              size="icon"
              className={RAIL_ICON_BTN}
              onClick={() => setSidebarCollapsed(true)}
              title="Collapse sidebar"
            >
              <PanelLeft className="size-4" />
            </Button>
            <div className="flex shrink-0 items-center gap-0.5">
              {/* Select toggle — plain ghost when idle, accent-filled only while
              selecting so the accent signals the active mode (#670). */}
              <Button
                variant="ghost"
                size="icon"
                className={cn(
                  "size-7",
                  selectMode
                    ? "bg-emerald-600 text-white hover:bg-emerald-700 hover:text-white dark:hover:bg-emerald-700"
                    : "text-muted-foreground hover:text-foreground",
                )}
                onClick={() =>
                  selectMode ? exitSelect() : setSelectMode(true)
                }
                title="Select sessions"
                aria-label="Select sessions"
                aria-pressed={selectMode}
              >
                <SquareCheck className="size-4" />
              </Button>
              {/* New session — a plain ghost icon button like the other header
              controls (#670); the accent lives on the select toggle while active. */}
              <Button
                variant="ghost"
                size="icon"
                className={RAIL_ICON_BTN}
                onClick={newSessionInFocusedPane}
                title="New session"
                aria-label="New session"
              >
                <Plus className="size-4" />
              </Button>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button
                    variant="ghost"
                    size="icon"
                    className={RAIL_ICON_BTN}
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
            </div>
          </div>

          <Separator />

          {/* Bulk action bar (#643) — revealed beneath the header while selecting. */}
          {selectMode && (
            <div className="flex items-center gap-2 px-2 pb-2">
              <input
                type="checkbox"
                checked={allSelected}
                onChange={toggleSelectAll}
                aria-label="Select all sessions"
                className="size-3.5 shrink-0 accent-primary"
              />
              <span className="text-[12px] text-muted-foreground">
                {selected.size} selected
              </span>
              <div className="ml-auto flex items-center gap-0.5">
                <Button
                  variant="ghost"
                  size="xs"
                  disabled={selected.size === 0}
                  onClick={bulkDismiss}
                  className="text-muted-foreground hover:text-foreground"
                >
                  {selectedAllDismissed ? (
                    <>
                      <RotateCcw className="size-3" />
                      Restore
                    </>
                  ) : (
                    <>
                      <EyeOff className="size-3" />
                      Dismiss
                    </>
                  )}
                </Button>
                <Button
                  variant="ghost"
                  size="xs"
                  disabled={selected.size === 0}
                  onClick={() => setConfirmingBulkDelete(true)}
                  className="text-destructive hover:text-destructive"
                >
                  <Trash2 className="size-3" />
                  Delete
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  className="size-6 text-muted-foreground hover:text-foreground"
                  onClick={exitSelect}
                  title="Exit select mode"
                  aria-label="Exit select mode"
                >
                  <X className="size-3.5" />
                </Button>
              </div>
            </div>
          )}

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
              {rows.map((session) => (
                <SessionItem
                  key={session.id}
                  session={session}
                  index={indexById.get(session.id) ?? 0}
                  active={session.id === activeSessionId}
                  streaming={Boolean(streamingBySession[session.id])}
                  finished={Boolean(recentlyFinishedBySession[session.id])}
                  pinned={pinnedSet.has(session.id)}
                  dismissed={dismissedSet.has(session.id)}
                  selectMode={selectMode}
                  selected={selected.has(session.id)}
                  onToggleSelect={() => toggleSelect(session.id)}
                />
              ))}
              {/* One endless, incremental reveal (#667): +25 rows per click, walking
              non-dismissed then dismissed. "Show less" re-compacts to the first
              batch once expanded. */}
              {hasMore && (
                <button
                  type="button"
                  onClick={() =>
                    setRevealCount((n) => n + SESSION_REVEAL_BATCH)
                  }
                  className="mx-0.5 rounded-md px-2 py-1.5 text-left text-[12px] text-muted-foreground/70 transition-colors hover:bg-sidebar-accent/50 hover:text-foreground"
                >
                  Show more
                </button>
              )}
              {!hasMore && revealCount > SESSION_REVEAL_BATCH && (
                <button
                  type="button"
                  onClick={() => setRevealCount(SESSION_REVEAL_BATCH)}
                  className="mx-0.5 rounded-md px-2 py-1.5 text-left text-[12px] text-muted-foreground/70 transition-colors hover:bg-sidebar-accent/50 hover:text-foreground"
                >
                  Show less
                </button>
              )}

              {/* Content matches (#710): sessions found by full-text search, not
                  by title. Separate, labeled group in BM25 order. Hidden in select
                  mode (these rows aren't selectable). */}
              {filtering && !selectMode && contentRows.length > 0 && (
                <>
                  <p className="mx-0.5 mt-2 mb-0.5 px-2 text-[10px] font-medium uppercase tracking-wide text-muted-foreground/50">
                    In messages
                  </p>
                  {contentRows.map(({ session, hit }) => (
                    <ContentHitRow
                      key={hit.messageId}
                      session={session}
                      snippet={hit.snippet}
                      onOpen={() => openContentHit(hit)}
                    />
                  ))}
                </>
              )}

              {filtering &&
                !contentSearchPending &&
                rows.length === 0 &&
                contentRows.length === 0 && (
                  <EmptyState title={`No sessions match “${filter.trim()}”`} />
                )}
            </nav>
          </ScrollArea>

          <AlertDialog
            open={confirmingBulkDelete}
            onOpenChange={setConfirmingBulkDelete}
          >
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>
                  Delete {selected.size}{" "}
                  {selected.size === 1 ? "session" : "sessions"}?
                </AlertDialogTitle>
                <AlertDialogDescription>
                  This permanently removes the selected{" "}
                  {selected.size === 1 ? "session" : "sessions"} and{" "}
                  {selected.size === 1 ? "its" : "their"} transcript
                  {selected.size === 1 ? "" : "s"}. This can&rsquo;t be undone.
                  (To hide sessions without losing them, use Dismiss.)
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Cancel</AlertDialogCancel>
                <AlertDialogAction onClick={() => void bulkDelete()}>
                  Delete
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>

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
        </>
      )}
    </aside>
  );
}
