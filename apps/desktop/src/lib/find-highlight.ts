// DOM helpers for the in-thread find bar (#679). Pure DOM, no React — builds
// per-occurrence Ranges within the matching messages and paints them with the
// CSS Custom Highlight API (`CSS.highlights` + `::highlight(...)` in index.css).
//
// The authoritative match set (which messages match) comes from the backend
// `searchInSession`; this module only locates the visible occurrences of the
// query inside those messages' rendered DOM and orders them for next/prev.
// Occurrences inside collapsed sub-blocks aren't in the DOM yet — auto-expanding
// those is a tracked follow-up.

import { isWordChar, tokenizeQuery } from "@/lib/find-tokens";

const ALL = "ff-find";
const ACTIVE = "ff-find-active";

/** True when the CSS Custom Highlight API is available (WKWebView ≥ Safari 17.2
 *  / macOS ≥ 14.2). When false, callers fall back to message-level scrolling. */
export function supportsHighlightApi(): boolean {
  return (
    typeof CSS !== "undefined" &&
    "highlights" in CSS &&
    typeof Highlight !== "undefined"
  );
}

/**
 * Collect one Range per whole-token occurrence of `query` inside the elements in
 * `messageIds`, in document order. Elements are looked up by `data-message-id`
 * within `root` (the pane's content subtree) so highlights never leak across
 * split panes.
 *
 * Token-aware (#748): `messageIds` is the authoritative set from
 * `searchInSession`, an FTS5 *tokenized* query — an AND of whole tokens, any
 * order/distance, case-insensitive, no prefix. This scan mirrors that: it splits
 * the query into tokens (`tokenizeQuery`) and highlights each token only at word
 * boundaries. So a multi-word query like `run turn` highlights both tokens
 * wherever they appear, and a token like `run` is *not* highlighted inside a
 * larger word (`overrun`) — highlights and the "n of m" count stay aligned with
 * the match set. (Deviations from FTS are documented in `find-tokens.ts`.)
 *
 * Skip-aware (#875): any text node whose closest ancestor carries
 * `data-skip-find` is excluded. Reasoning text (the folded Thinking block) carries
 * the attribute so its surface (the 120-char preview line and the expanded body)
 * doesn't contribute to the highlight set — the backend FTS5 index doesn't cover
 * reasoning, and surfacing FE-only matches here would re-introduce the
 * data-vs-DOM divergence the count was centralised to fix.
 */
export function collectOccurrences(
  root: HTMLElement,
  messageIds: Set<string>,
  query: string,
): Range[] {
  const ranges: Range[] = [];
  const tokens = tokenizeQuery(query);
  if (tokens.length === 0) return ranges;

  // Preserve on-screen (document) order across messages for sane next/prev.
  const rows = Array.from(
    root.querySelectorAll<HTMLElement>("[data-message-id]"),
  ).filter((el) => messageIds.has(el.dataset.messageId ?? ""));

  for (const row of rows) {
    const walker = document.createTreeWalker(row, NodeFilter.SHOW_TEXT, {
      acceptNode(node) {
        // Reject text nodes inside any `data-skip-find` ancestor up to the
        // `[data-message-id]` row. Walk up from `node` until we hit `row` (the
        // iteration's root) or a skip marker; reject the latter, accept the former.
        let el: Node | null = node.parentNode;
        while (el && el !== row) {
          if (
            el.nodeType === Node.ELEMENT_NODE &&
            (el as Element).hasAttribute("data-skip-find")
          ) {
            return NodeFilter.FILTER_REJECT;
          }
          el = el.parentNode;
        }
        return NodeFilter.FILTER_ACCEPT;
      },
    });
    let node = walker.nextNode();
    while (node) {
      const text = node.nodeValue ?? "";
      const hay = text.toLowerCase();
      // Gather every token's whole-word hits in this node, then order them by
      // offset so multiple tokens in one node still step in document order.
      const hits: { at: number; len: number }[] = [];
      for (const token of tokens) {
        let at = hay.indexOf(token, 0);
        while (at !== -1) {
          const end = at + token.length;
          // Whole token only: neither neighbour may be a token character.
          if (!isWordChar(hay[at - 1]) && !isWordChar(hay[end])) {
            hits.push({ at, len: token.length });
          }
          at = hay.indexOf(token, at + token.length);
        }
      }
      hits.sort((a, b) => a.at - b.at);
      for (const { at, len } of hits) {
        const range = document.createRange();
        range.setStart(node, at);
        range.setEnd(node, at + len);
        ranges.push(range);
      }
      node = walker.nextNode();
    }
  }
  return ranges;
}

/** Paint all occurrences, marking `activeIndex` (if any) as the active match. */
export function applyHighlights(ranges: Range[], activeIndex: number): void {
  if (!supportsHighlightApi()) return;
  if (ranges.length === 0) {
    clearHighlights();
    return;
  }
  CSS.highlights.set(ALL, new Highlight(...ranges));
  const active = ranges[activeIndex];
  if (active) {
    CSS.highlights.set(ACTIVE, new Highlight(active));
  } else {
    CSS.highlights.delete(ACTIVE);
  }
}

/** Remove all find highlights. Safe to call when the API is unsupported. */
export function clearHighlights(): void {
  if (!supportsHighlightApi()) return;
  CSS.highlights.delete(ALL);
  CSS.highlights.delete(ACTIVE);
}

/**
 * Index of the first range that falls inside the message `messageId` (its start
 * node's closest `[data-message-id]` ancestor), or -1 if none. Lets a global-
 * search hit (#710) activate the clicked message's occurrence rather than the
 * first occurrence in the thread.
 */
export function indexOfMessage(ranges: Range[], messageId: string): number {
  return ranges.findIndex((range) => {
    const start = range.startContainer;
    const el =
      start.nodeType === Node.ELEMENT_NODE
        ? (start as HTMLElement)
        : start.parentElement;
    return (
      el?.closest("[data-message-id]")?.getAttribute("data-message-id") ===
      messageId
    );
  });
}

/** Scroll the element containing `range` into the centre of its scroller. */
export function scrollRangeIntoView(range: Range): void {
  const el =
    range.startContainer.nodeType === Node.ELEMENT_NODE
      ? (range.startContainer as HTMLElement)
      : range.startContainer.parentElement;
  el?.scrollIntoView({ block: "center", behavior: "smooth" });
}
