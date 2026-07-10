// In-thread find bar (#679/#875). IDE-style Cmd+F for the active thread, with
// the authoritative match set coming from the backend `searchInSession`
// (FTS5 over message text + tool-call args + tool-result bodies, v11). What
// #875 changed: the count + active cursor now come from the *data model*
// (`lib/find-occurrences.ts`), not the DOM — so matches in folded sub-blocks
// are counted, and the DOM is only used to paint + scroll to the active
// occurrence AFTER force-opening its containing collapser (StepGroup /
// ToolStepBlock / long OutputBlock) via `store/find-expansion`.
//
// Strategy: initial-bulk — open every collapser in every matching message
// when find opens, then step over the data-model list. Force-open on demand
// inside `step()` covers the case where the user manually folded a
// containing sub-block mid-search; the bus is idempotent.

import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronUp, Search, X } from "@/components/ui/icon";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ipc } from "@/lib/ipc";
import { useFindStore } from "@/store/find";
import { useFindExpansion } from "@/store/find-expansion";
import { useChatStore } from "@/store/chat";
import {
  applyHighlights,
  clearHighlights,
  collectOccurrences,
  scrollRangeIntoView,
} from "@/lib/find-highlight";
import {
  buildSessionOccurrences,
  uniqueExpandIds,
  type Occurrence,
} from "@/lib/find-occurrences";

const DEBOUNCE_MS = 150;

export function FindBar({
  sessionId,
  rootRef,
}: {
  sessionId: string;
  rootRef: React.RefObject<HTMLDivElement | null>;
}) {
  const closeFind = useFindStore((s) => s.closeFind);
  const consumeSeed = useFindStore((s) => s.consumeSeed);
  const setForced = useFindExpansion((s) => s.setForced);
  const forceOpenMany = useFindExpansion((s) => s.forceOpenMany);
  const clearExpansion = useFindExpansion((s) => s.clear);
  const inputRef = useRef<HTMLInputElement>(null);

  // Global search (#710) can open the bar pre-seeded: a query to run and a
  // specific message to jump to. Captured once at mount (before the store seed
  // is consumed) so the first search activates that hit instead of occurrence #1.
  const [query, setQuery] = useState(
    () => useFindStore.getState().seedQuery ?? "",
  );
  const seedMessageIdRef = useRef(useFindStore.getState().seedMessageId);
  const [count, setCount] = useState(0);
  const [active, setActive] = useState(0);
  // Data-model list + DOM ranges kept in refs so `step()` reads fresh values
  // without re-running the search effect. Data-model list is the source of
  // truth for count + cursor (#875); DOM ranges only paint the active set.
  const occurrencesRef = useRef<Occurrence[]>([]);
  const rangesRef = useRef<Range[]>([]);
  const activeRef = useRef(0);

  // Focus the input as soon as the bar opens, and clear the store seed now
  // that this instance has captured it. Also drop any stuck forced-open ids so
  // a re-open starts clean. On unmount, drop the forced-open set so the user's
  // manual collapses and the default-folded state are restored.
  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
    consumeSeed();
    clearExpansion();
    return () => {
      clearExpansion();
      clearHighlights();
    };
  }, [consumeSeed, clearExpansion]);

  // Debounced search: resolve matching messages from the backend, derive the
  // occurrence list from the data model, force-open the containing collapsers,
  // yield two frames so React mounts the newly-visible body, then walk the
  // DOM for paintable ranges. The data-model list drives `m` and the active
  // cursor; the DOM ranges only paint + scroll.
  useEffect(() => {
    const q = query.trim();
    let cancelled = false;
    const timer = window.setTimeout(
      async () => {
        occurrencesRef.current = [];
        rangesRef.current = [];
        activeRef.current = 0;
        if (!q) {
          if (cancelled) return;
          setForced([]);
          setCount(0);
          setActive(0);
          clearHighlights();
          return;
        }
        let hits: Awaited<ReturnType<typeof ipc.searchInSession>> = [];
        try {
          hits = await ipc.searchInSession(sessionId, q);
          if (cancelled) return;
        } catch {
          if (cancelled) return;
        }
        const messages =
          useChatStore.getState().messagesBySession[sessionId] ?? [];
        const liveSteps = useChatStore.getState().toolStepsByMessage;
        const ids = new Set(hits.map((h) => h.messageId));
        const occurrences = buildSessionOccurrences(
          messages,
          liveSteps,
          ids,
          q,
        );
        if (cancelled) return;

        // Force-open every collapser that hides an occurrence, then yield two
        // frames so React mounts the newly-visible body. Without the wait
        // the DOM walker would still see the collapsed tree and fall back to
        // wrapper-level ranges — exactly the "scrolls to top of first
        // response" symptom (#875).
        const expandIds = uniqueExpandIds(occurrences);
        setForced(expandIds);

        const paint = () => {
          if (cancelled) return;
          const root = rootRef.current;
          const ranges = root ? collectOccurrences(root, ids, q) : [];
          // Source-of-truth count: the data-model list (#875). The DOM range
          // count is a sanity check — if it disagrees, paint what we have and
          // log in dev only.
          if (
            occurrences.length > 0 &&
            ranges.length !== occurrences.length &&
            typeof console !== "undefined" &&
            import.meta.env?.DEV
          ) {
            console.warn(
              `[find] DOM ranges (${ranges.length}) != data-model occurrences (${occurrences.length}); possibly missed expandId.`,
            );
          }
          // Seed jump (#710): land on the first occurrence in the seeded
          // messageId, not just the first in the thread.
          const seedId = seedMessageIdRef.current;
          let idx = 0;
          if (seedId) {
            const target = occurrences.findIndex((o) => o.messageId === seedId);
            if (target >= 0) idx = target;
          }
          seedMessageIdRef.current = null;
          occurrencesRef.current = occurrences;
          rangesRef.current = ranges;
          activeRef.current = idx;
          setCount(occurrences.length);
          setActive(idx);
          applyHighlights(ranges, idx);
          if (ranges[idx]) scrollRangeIntoView(ranges[idx]);
        };
        requestAnimationFrame(() => {
          if (cancelled) return;
          requestAnimationFrame(paint);
        });
      },
      q ? DEBOUNCE_MS : 0,
    );
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [query, sessionId, rootRef, setForced]);

  function step(dir: 1 | -1) {
    const occurrences = occurrencesRef.current;
    if (occurrences.length === 0) return;
    const next =
      (activeRef.current + dir + occurrences.length) % occurrences.length;
    const occ = occurrences[next];
    // Re-force-open the next occurrence's collapser if it isn't (might have
    // been manually folded mid-search). Idempotent.
    if (occ?.expandId) forceOpenMany([occ.expandId]);
    activeRef.current = next;
    setActive(next);
    const ranges = rangesRef.current;
    if (ranges[next]) {
      applyHighlights(ranges, next);
      scrollRangeIntoView(ranges[next]);
      return;
    }
    // No DOM range for `next` yet (force-open mid-step): wait two frames, then
    // walk the DOM with the same token rules. Keeps the counter and the
    // highlighted span synchronized even after a manual fold.
    const cancelled = { v: false };
    requestAnimationFrame(() => {
      if (cancelled.v) return;
      requestAnimationFrame(() => {
        if (cancelled.v) return;
        const root = rootRef.current;
        if (!root) return;
        const ids = new Set(occurrences.map((o) => o.messageId));
        const q = query.trim();
        const fresh = collectOccurrences(root, ids, q);
        const clampedNext = Math.min(next, fresh.length - 1);
        rangesRef.current = fresh;
        activeRef.current = clampedNext >= 0 ? clampedNext : 0;
        applyHighlights(fresh, clampedNext);
        if (fresh[clampedNext]) scrollRangeIntoView(fresh[clampedNext]);
      });
    });
    return () => {
      cancelled.v = true;
    };
  }

  function close() {
    clearHighlights();
    clearExpansion();
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
