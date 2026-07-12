// Shared result-row renderer for both #876 search surfaces: the sidebar's
// compact dropdown (surface A, anchored below the filter input) and the
// full-screen "All Conversations" modal (surface B). Owns the assistant-
// assisted search placeholder row, each content-hit's title/date/snippet, the
// empty state, and the active-row auto-scroll effect (mirrors palette.tsx's
// `data-index` + `scrollIntoView` pattern). Keyboard handling (Arrow/Enter)
// stays with each caller's own `<input>`, exactly like palette.tsx.

import { useEffect } from "react";
import { Sparkles } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { resolveLabel, sanitizeSnippet, formatHitDate } from "@/lib/sessions";
import type { ContentHitRow } from "@/lib/sessions";

export interface SearchHitListProps {
  rows: ContentHitRow[];
  /** Clamped by the caller at render time (palette.tsx pattern) — never stored
   *  out of bounds when `rows` shrinks under a stale index. */
  activeIndex: number;
  onHover: (index: number) => void;
  onSelect: (row: ContentHitRow) => void;
  listRef: React.RefObject<HTMLDivElement | null>;
  /** Controls row density/snippet clamping — compact for the dropdown, roomier
   *  for the full-screen modal. */
  variant: "dropdown" | "modal";
  /** A search is still resolving for the current query — suppresses the empty
   *  state so it doesn't flash mid-debounce (#747 C2). */
  pending: boolean;
  emptyLabel: string;
}

export function SearchHitList({
  rows,
  activeIndex,
  onHover,
  onSelect,
  listRef,
  variant,
  pending,
  emptyLabel,
}: SearchHitListProps) {
  // Keep the highlighted row visible during arrow navigation (palette.tsx).
  useEffect(() => {
    listRef.current
      ?.querySelector(`[data-index="${activeIndex}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [activeIndex, listRef]);

  const compact = variant === "dropdown";

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* Assistant-assisted search (#876): placeholder entry row, deliberately
          non-interactive — the click behavior (agent-driven search) is a
          separate follow-up. Excluded from arrow-key roving focus below, so
          it can never end up "selected" by Enter. */}
      <div
        aria-disabled="true"
        title="Coming soon"
        className={cn(
          "flex shrink-0 cursor-default items-center gap-2 border-b text-muted-foreground/60 select-none",
          compact ? "px-2.5 py-2 text-[12px]" : "px-4 py-3 text-[13px]",
        )}
      >
        <Sparkles className="size-3.5 shrink-0" />
        Search with the agent
      </div>

      {rows.length === 0 && !pending && (
        <div
          className={cn(
            "text-center text-muted-foreground/70",
            compact ? "px-3 py-4 text-[12px]" : "px-3 py-8 text-[13px]",
          )}
        >
          {emptyLabel}
        </div>
      )}

      {rows.length > 0 && (
        <div
          ref={listRef}
          role="listbox"
          className="min-h-0 flex-1 overflow-y-auto p-1.5"
        >
          {rows.map(({ session, hit }, i) => {
            const active = i === activeIndex;
            return (
              <div
                key={hit.messageId}
                data-index={i}
                role="option"
                aria-selected={active}
                onMouseMove={() => onHover(i)}
                onClick={() => onSelect({ session, hit })}
                className={cn(
                  "flex cursor-pointer select-none flex-col gap-0.5 rounded-md text-left transition-colors",
                  compact ? "px-2 py-1.5" : "px-3 py-2.5",
                  active
                    ? "bg-accent text-accent-foreground"
                    : "text-muted-foreground hover:bg-sidebar-accent/50 hover:text-foreground",
                )}
              >
                <div className="flex min-w-0 items-baseline justify-between gap-2">
                  <span
                    className={cn(
                      "min-w-0 truncate font-medium",
                      compact ? "text-[13px]" : "text-[14px]",
                      active ? "" : "text-foreground/90",
                    )}
                  >
                    {resolveLabel(session)}
                  </span>
                  <span className="shrink-0 text-[10px] text-muted-foreground/60">
                    {formatHitDate(hit.createdAt)}
                  </span>
                </div>
                {/* Snippet is pre-highlighted with <mark> by the backend;
                    `ff-hit-snippet` styles the marks (see index.css). Sanitized
                    first — the backend does not escape the surrounding message
                    text, which can contain raw HTML (#747 C1). */}
                <span
                  className={cn(
                    "ff-hit-snippet min-w-0 text-muted-foreground/80",
                    compact
                      ? "truncate text-[11px]"
                      : "line-clamp-2 text-[12px]",
                  )}
                  dangerouslySetInnerHTML={{
                    __html: sanitizeSnippet(hit.snippet),
                  }}
                />
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
