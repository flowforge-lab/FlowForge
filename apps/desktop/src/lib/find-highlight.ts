// DOM helpers for the in-thread find bar (#679). Pure DOM, no React — builds
// per-occurrence Ranges within the matching messages and paints them with the
// CSS Custom Highlight API (`CSS.highlights` + `::highlight(...)` in index.css).
//
// The authoritative match set (which messages match) comes from the backend
// `searchInSession`; this module only locates the visible occurrences of the
// query inside those messages' rendered DOM and orders them for next/prev.
// Occurrences inside collapsed sub-blocks aren't in the DOM yet — auto-expanding
// those is a tracked follow-up.

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
 * Collect one Range per case-insensitive occurrence of `query` inside the
 * elements in `messageIds`, in document order. Elements are looked up by
 * `data-message-id` within `root` (the pane's content subtree) so highlights
 * never leak across split panes.
 *
 * NOTE — two different match models (PR #739 review): `messageIds` is the
 * authoritative set from `searchInSession`, an FTS5 *tokenized* query (whole/
 * prefix tokens, any order/distance). This scan, by contrast, highlights a
 * *literal case-insensitive substring* of the whole query. So a multi-word
 * query can mark a message as matching (all tokens present) while this finds no
 * literal substring → the message is counted but has nothing to highlight/step
 * to; and an FTS token hit (`run`) won't highlight it inside a larger word
 * (`overrun`). Benign for the common single-word find, and "n of m" counts DOM
 * occurrences so the number stays self-consistent. Reconciling the two (e.g.
 * per-token DOM highlighting) is a tracked follow-up.
 */
export function collectOccurrences(
  root: HTMLElement,
  messageIds: Set<string>,
  query: string,
): Range[] {
  const ranges: Range[] = [];
  if (!query) return ranges;
  const needle = query.toLowerCase();

  // Preserve on-screen (document) order across messages for sane next/prev.
  const rows = Array.from(
    root.querySelectorAll<HTMLElement>("[data-message-id]"),
  ).filter((el) => messageIds.has(el.dataset.messageId ?? ""));

  for (const row of rows) {
    const walker = document.createTreeWalker(row, NodeFilter.SHOW_TEXT);
    let node = walker.nextNode();
    while (node) {
      const text = node.nodeValue ?? "";
      const hay = text.toLowerCase();
      let from = 0;
      let at = hay.indexOf(needle, from);
      while (at !== -1) {
        const range = document.createRange();
        range.setStart(node, at);
        range.setEnd(node, at + needle.length);
        ranges.push(range);
        from = at + needle.length;
        at = hay.indexOf(needle, from);
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
