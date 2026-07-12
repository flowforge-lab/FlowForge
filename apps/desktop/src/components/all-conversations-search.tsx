// Full-screen "All Conversations" search modal (#876 surface B): opened from
// the collapsed sidebar rail's search button (the sidebar itself stays
// collapsed — this replaces the old "expand and focus the filter" behavior).
// Structural clone of palette.tsx's CommandPalette/PaletteBody split (thin
// closed-state wrapper + fresh-mount body, hand-rolled backdrop-then-panel,
// footer hint row) — the closest prior art in this codebase, since no shared
// Modal primitive exists here.

import { useEffect, useMemo, useRef, useState } from "react";
import { CornerDownLeft } from "@/components/ui/icon";
import { useChatStore } from "@/store/chat";
import { useAllConversationsSearchStore } from "@/store/all-conversations-search";
import { useContentSearch } from "@/hooks/use-content-search";
import { openContentHit } from "@/lib/open-content-hit";
import { groupContentHits } from "@/lib/sessions";
import { SearchHitList } from "@/components/search-hit-list";

// Thin wrapper so the body mounts fresh each open: query/selection reset for
// free (no effect), and the modal costs nothing while closed.
export function AllConversationsSearchModal() {
  const open = useAllConversationsSearchStore((s) => s.open);
  if (!open) return null;
  return <AllConversationsSearchBody />;
}

function AllConversationsSearchBody() {
  const closeSearch = useAllConversationsSearchStore((s) => s.closeSearch);
  const sessions = useChatStore((s) => s.sessions);

  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const { hits, pending } = useContentSearch(query);

  const sessionsById = useMemo(
    () => new Map(sessions.map((s) => [s.id, s])),
    [sessions],
  );
  // Cross-session content search only (#710 seam) — there's no separate
  // title-matched list here to exclude, unlike the sidebar's dropdown.
  const rows = useMemo(
    () => groupContentHits(hits, new Set(), sessionsById),
    [hits, sessionsById],
  );

  // Clamp at read time rather than storing — results can shrink under a stale
  // `selected` (typing, or sessions changing while open) without a reset effect.
  const activeIndex = rows.length ? Math.min(selected, rows.length - 1) : 0;

  // Focus the input on mount — the wrapper mounts us anew each open.
  useEffect(() => {
    const id = requestAnimationFrame(() => inputRef.current?.focus());
    return () => cancelAnimationFrame(id);
  }, []);

  function openRow(index: number): void {
    const row = rows[index];
    if (!row) return;
    closeSearch();
    openContentHit(row.hit, query.trim());
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLInputElement>): void {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelected(rows.length ? (activeIndex + 1) % rows.length : 0);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelected(
        rows.length ? (activeIndex - 1 + rows.length) % rows.length : 0,
      );
    } else if (e.key === "Enter") {
      e.preventDefault();
      openRow(activeIndex);
    } else if (e.key === "Escape") {
      e.preventDefault();
      closeSearch();
    }
  }

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="All conversations search"
      className="fixed inset-0 z-50 flex items-start justify-center"
    >
      {/* Click-outside closes. Separate element so a click on the panel (a
          sibling painted above) never reaches it. */}
      <div
        className="absolute inset-0 bg-background/60 backdrop-blur-sm"
        onMouseDown={closeSearch}
      />

      <div className="relative mt-[10vh] flex max-h-[76vh] w-[92%] max-w-2xl flex-col overflow-hidden rounded-xl border bg-card shadow-2xl">
        {/* Search input */}
        <div className="flex items-center gap-2.5 border-b px-4">
          <span className="shrink-0 text-[13px] font-medium text-foreground/90">
            All Conversations
          </span>
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setSelected(0); // typing always re-highlights the top result
            }}
            onKeyDown={onKeyDown}
            placeholder="Search all conversations…"
            aria-label="Search all conversations"
            spellCheck={false}
            autoComplete="off"
            className="h-12 min-w-0 flex-1 bg-transparent text-[14px] text-foreground outline-none placeholder:text-muted-foreground/50"
          />
          <kbd className="shrink-0 font-mono text-[11px] text-muted-foreground/60">
            esc
          </kbd>
        </div>

        <SearchHitList
          rows={rows}
          activeIndex={activeIndex}
          onHover={setSelected}
          onSelect={(row) => {
            const index = rows.indexOf(row);
            openRow(index === -1 ? activeIndex : index);
          }}
          listRef={listRef}
          variant="modal"
          pending={pending}
          emptyLabel={
            query.trim()
              ? `No conversations match “${query.trim()}”`
              : "Type to search across every conversation"
          }
        />

        {/* Footer key hints */}
        <div className="flex shrink-0 items-center gap-3 border-t px-3.5 py-2 text-[11px] text-muted-foreground/60">
          <span className="flex items-center gap-1">
            <kbd className="font-mono">↑</kbd>
            <kbd className="font-mono">↓</kbd>
            navigate
          </span>
          <span className="flex items-center gap-1">
            <CornerDownLeft className="size-3" />
            open
          </span>
          <span className="flex items-center gap-1">
            <kbd className="font-mono">⌘F</kbd>
            all conversations
          </span>
        </div>
      </div>
    </div>
  );
}
