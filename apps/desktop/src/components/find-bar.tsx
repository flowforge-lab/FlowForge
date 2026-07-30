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
  clearHighlights,
  collectOccurrences,
  paintHighlights,
  scrollRangeIntoView,
} from "@/lib/find-highlight";
import { useTranscriptScroll } from "@/store/transcript-scroll";
import {
  buildSessionOccurrences,
  uniqueExpandIds,
  type Occurrence,
} from "@/lib/find-occurrences";

const DEBOUNCE_MS = 150;
// How many frames to wait for a revealed row to mount. The virtualizer may need
// more than one pass for a far jump (it re-measures and corrects), so this polls
// rather than assuming a fixed number of frames — but it is bounded, because a
// stale occurrence whose message is genuinely gone must not hang the step.
const REVEAL_FRAME_BUDGET = 20;

const nextFrame = () =>
  new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));

/** How many earlier occurrences share `occurrences[i]`'s message. Identifies the
 *  active match within its own row, which is what makes it addressable when the
 *  painted set covers only the mounted subset. */
function ordinalWithinMessage(occurrences: Occurrence[], i: number): number {
  let n = 0;
  for (let k = 0; k < i; k++) {
    if (occurrences[k].messageId === occurrences[i].messageId) n++;
  }
  return n;
}

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

  /** Wait for `messageId`'s row to be in the DOM, up to a frame budget. Returns
   *  null if it never appears — a windowed list that can't reach the row, or an
   *  occurrence whose message is gone. The caller still paints what it can. */
  async function waitForRow(
    messageId: string,
    isCancelled?: () => boolean,
  ): Promise<HTMLElement | null> {
    const selector = `[data-message-id="${messageId}"]`;
    for (let i = 0; i < REVEAL_FRAME_BUDGET; i++) {
      const el = rootRef.current?.querySelector<HTMLElement>(selector);
      if (el) return el;
      if (isCancelled?.()) return null;
      await nextFrame();
    }
    return rootRef.current?.querySelector<HTMLElement>(selector) ?? null;
  }

  // Reveal and paint occurrence `idx` — the single path used by the initial
  // search, the #710 seed jump, and every Enter/Shift+Enter step.
  //
  // Two things make this more than "walk the DOM":
  //
  // 1. **The row may not be mounted.** With the transcript windowed (#1143) only
  //    the visible rows exist, so a hit 3000 rows up has no node to range over
  //    until the list is asked to mount it. `reveal` does that; without it the
  //    counter reads "1 of 500" off the data model while Enter can only reach the
  //    handful of hits on screen. On the non-virtual path `reveal` returns false
  //    and nothing is needed — every row is already there.
  // 2. **The active range can't be an index into the painted set.** The painted
  //    set covers mounted rows only, so it is a *subset* of `occurrences` whose
  //    indices don't line up. The active range is instead resolved by identity:
  //    this occurrence's message, and its ordinal within that message.
  async function activate(idx: number, isCancelled?: () => boolean) {
    const occurrences = occurrencesRef.current;
    const occ = occurrences[idx];
    if (!occ) {
      rangesRef.current = [];
      clearHighlights();
      return;
    }
    // Re-force-open the containing collapser if the user folded it mid-search.
    // Idempotent.
    if (occ.expandId) forceOpenMany([occ.expandId]);
    useTranscriptScroll.getState().reveal(sessionId, occ.messageId);

    const row = await waitForRow(occ.messageId, isCancelled);
    if (isCancelled?.()) return;
    const root = rootRef.current;
    if (!root) return;
    const q = query.trim();
    if (!q) return;
    // One more frame once the row exists, so a force-opened body inside it has
    // committed before its text is walked (#875's "scrolls to the top of the
    // first response" symptom was exactly this walk running too early).
    if (row) await nextFrame();
    if (isCancelled?.()) return;

    const all = collectOccurrences(
      root,
      new Set(occurrences.map((o) => o.messageId)),
      q,
    );
    const inMessage = collectOccurrences(root, new Set([occ.messageId]), q);
    const activeRange = inMessage[ordinalWithinMessage(occurrences, idx)];
    rangesRef.current = all;
    paintHighlights(all, activeRange);
    if (activeRange) scrollRangeIntoView(activeRange);
  }

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
        activeRef.current = idx;
        setCount(occurrences.length);
        setActive(idx);
        await activate(idx, () => cancelled);
      },
      q ? DEBOUNCE_MS : 0,
    );
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
    // `activate` is re-created every render (it closes over `query` and the
    // refs); listing it would re-run the search — and re-hit the backend — on
    // every keystroke's render rather than once per debounced query. The refs it
    // reads are always current by construction.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query, sessionId, rootRef, setForced]);

  function step(dir: 1 | -1) {
    const occurrences = occurrencesRef.current;
    if (occurrences.length === 0) return;
    const next =
      (activeRef.current + dir + occurrences.length) % occurrences.length;
    activeRef.current = next;
    setActive(next);
    void activate(next);
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
      <span
        data-testid="find-counter"
        className="w-16 shrink-0 select-none text-right text-[11px] tabular-nums text-muted-foreground"
      >
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
