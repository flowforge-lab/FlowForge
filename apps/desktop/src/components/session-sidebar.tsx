import { useRef, useState } from "react";
import { Moon, Pencil, Plus, Search, Sun, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { cn } from "@/lib/utils";
import { useTheme } from "@/lib/theme";
import { useChatStore } from "@/store/chat";
import { filterSessions, resolveLabel } from "@/lib/sessions";
import type { Session } from "@/bindings";

// ── Inline-rename session item ───────────────────────────────────────────────

function SessionItem({
  session,
  index,
  active,
  streaming,
}: {
  session: Session;
  index: number;
  active: boolean;
  streaming: boolean;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  const sessionTitles = useChatStore((s) => s.sessionTitles);
  const selectSession = useChatStore((s) => s.selectSession);
  const setSessionTitle = useChatStore((s) => s.setSessionTitle);

  const currentLabel = resolveLabel(session, sessionTitles[session.id]);

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
    <div
      role="button"
      tabIndex={0}
      onClick={() => !editing && void selectSession(session.id)}
      onKeyDown={(e) => {
        if (!editing && (e.key === "Enter" || e.key === " ")) {
          e.preventDefault();
          void selectSession(session.id);
        }
      }}
      className={cn(
        "group relative flex items-center gap-1.5 rounded-md px-2 py-1.5 text-left text-[13px] transition-colors cursor-pointer select-none",
        active
          ? "bg-sidebar-accent text-sidebar-accent-foreground"
          : "text-muted-foreground hover:bg-sidebar-accent/50 hover:text-foreground",
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

          {/* Right-side slot: streaming dot > pencil (hover) > kbd hint (idle) */}
          <span className="flex shrink-0 items-center">
            {streaming ? (
              <span className="size-1.5 animate-pulse rounded-full bg-primary" />
            ) : (
              <>
                {/* Pencil shown on hover; kbd hint shown when idle */}
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
  );
}

// ── Sidebar ──────────────────────────────────────────────────────────────────

export function SessionSidebar() {
  const sessions = useChatStore((s) => s.sessions);
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const streamingBySession = useChatStore((s) => s.streamingBySession);
  const sessionTitles = useChatStore((s) => s.sessionTitles);
  const newSession = useChatStore((s) => s.newSession);
  const theme = useTheme((s) => s.theme);
  const toggleTheme = useTheme((s) => s.toggleTheme);

  const [filter, setFilter] = useState("");
  const filterRef = useRef<HTMLInputElement>(null);

  const filtered = filterSessions(sessions, filter, sessionTitles);
  // Each session's index in the *full* list — keeps the ⌘1–9 hint accurate when
  // the visible list is filtered (the global shortcut indexes the full list).
  const indexById = new Map(sessions.map((s, i) => [s.id, i]));
  const filtering = filter.trim().length > 0;

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
              theme === "light"
                ? "Switch to dark theme"
                : "Switch to light theme"
            }
          >
            {theme === "light" ? (
              <Moon className="size-4" />
            ) : (
              <Sun className="size-4" />
            )}
          </Button>
          <Button
            variant="ghost"
            size="icon"
            className="size-7 text-muted-foreground hover:text-foreground"
            onClick={() => void newSession()}
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
          {filtered.length === 0
            ? filtering && (
                <p className="px-2 py-6 text-center text-[12px] text-muted-foreground/60">
                  No sessions match “{filter.trim()}”
                </p>
              )
            : filtered.map((session) => (
                <SessionItem
                  key={session.id}
                  session={session}
                  index={indexById.get(session.id) ?? 0}
                  active={session.id === activeSessionId}
                  streaming={Boolean(streamingBySession[session.id])}
                />
              ))}
        </nav>
      </ScrollArea>

      <Separator />
      <div className="px-3 py-2 text-[11px] text-muted-foreground/60">
        ⌘K palette · ⌘N new · ⌘1–9 jump · Esc stop
      </div>
    </aside>
  );
}
