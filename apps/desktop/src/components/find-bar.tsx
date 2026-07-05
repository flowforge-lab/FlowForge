// In-thread find bar (#679) — IDE-style Cmd+F for the active thread. Floating,
// top-right of the pane. The authoritative match set (which messages contain the
// query, incl. tool-call args / tool-result bodies) comes from the backend
// `searchInSession`; within those messages we locate every visible occurrence in
// the DOM, paint them with the CSS Custom Highlight API, and step through them
// with a wrapping "n of m" counter. Occurrences inside collapsed sub-blocks are
// not in the DOM yet — auto-expanding those is a tracked follow-up. Where the
// Highlight API is unavailable (old WKWebView) the bar still counts matches and
// scrolls to them; only the inline paint is skipped.

import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronUp, Search, X } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ipc } from "@/lib/ipc";
import { useFindStore } from "@/store/find";
import {
  applyHighlights,
  clearHighlights,
  collectOccurrences,
  scrollRangeIntoView,
} from "@/lib/find-highlight";

const DEBOUNCE_MS = 150;

export function FindBar({
  sessionId,
  rootRef,
}: {
  sessionId: string;
  rootRef: React.RefObject<HTMLDivElement | null>;
}) {
  const closeFind = useFindStore((s) => s.closeFind);
  const inputRef = useRef<HTMLInputElement>(null);

  const [query, setQuery] = useState("");
  const [count, setCount] = useState(0);
  const [active, setActive] = useState(0);
  // DOM Ranges + the current index kept in refs so next/prev read fresh values
  // without stale closures and without re-running the search effect.
  const rangesRef = useRef<Range[]>([]);
  const activeRef = useRef(0);

  // Focus the input as soon as the bar opens.
  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  // Clear highlights when the bar unmounts (closed / session switch).
  useEffect(() => clearHighlights, []);

  // Debounced search: resolve matching messages from the backend, then locate
  // the visible occurrences in this pane's DOM and paint them. All state writes
  // stay inside the (async) timer callback so the effect body never sets state
  // synchronously. An emptied query settles to zero matches with no delay.
  useEffect(() => {
    const q = query.trim();
    let cancelled = false;
    const timer = window.setTimeout(
      async () => {
        let ranges: Range[] = [];
        if (q) {
          try {
            const hits = await ipc.searchInSession(sessionId, q);
            if (cancelled) return;
            const ids = new Set(hits.map((h) => h.messageId));
            const root = rootRef.current;
            if (root) ranges = collectOccurrences(root, ids, q);
          } catch {
            if (cancelled) return;
          }
        }
        rangesRef.current = ranges;
        activeRef.current = 0;
        setCount(ranges.length);
        setActive(0);
        applyHighlights(ranges, 0);
        if (ranges[0]) scrollRangeIntoView(ranges[0]);
      },
      q ? DEBOUNCE_MS : 0,
    );
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [query, sessionId, rootRef]);

  function step(dir: 1 | -1) {
    const ranges = rangesRef.current;
    if (ranges.length === 0) return;
    const next = (activeRef.current + dir + ranges.length) % ranges.length;
    activeRef.current = next;
    setActive(next);
    applyHighlights(ranges, next);
    scrollRangeIntoView(ranges[next]);
  }

  function close() {
    clearHighlights();
    closeFind();
  }

  const trimmed = query.trim();
  const counter =
    trimmed === ""
      ? ""
      : count === 0
        ? "No results"
        : `${active + 1} of ${count}`;

  return (
    <div className="absolute right-3 top-2 z-20 flex items-center gap-1 rounded-lg border bg-popover/95 py-1 pl-2 pr-1 shadow-md backdrop-blur-sm">
      <Search className="size-3.5 shrink-0 text-muted-foreground" />
      <Input
        ref={inputRef}
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            step(e.shiftKey ? -1 : 1);
          } else if (e.key === "Escape") {
            e.preventDefault();
            close();
          }
        }}
        placeholder="Find in thread"
        aria-label="Find in thread"
        className="h-6 w-44 border-0 bg-transparent px-1 text-xs focus-visible:ring-0"
      />
      <span className="w-16 shrink-0 select-none text-right text-[11px] tabular-nums text-muted-foreground">
        {counter}
      </span>
      <Button
        variant="ghost"
        size="icon-xs"
        disabled={count === 0}
        title="Previous match (Shift+Enter)"
        onClick={() => step(-1)}
      >
        <ChevronUp className="size-3.5" />
      </Button>
      <Button
        variant="ghost"
        size="icon-xs"
        disabled={count === 0}
        title="Next match (Enter)"
        onClick={() => step(1)}
      >
        <ChevronDown className="size-3.5" />
      </Button>
      <Button
        variant="ghost"
        size="icon-xs"
        title="Close (Esc)"
        onClick={close}
      >
        <X className="size-3.5" />
      </Button>
    </div>
  );
}
