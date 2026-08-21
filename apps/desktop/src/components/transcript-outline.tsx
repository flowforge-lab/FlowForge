import { memo, useEffect, useState } from "react";
import { cn } from "@/lib/utils";
import { useTranscriptScroll } from "@/store/transcript-scroll";
import { visibleMarkers } from "@/lib/transcript-outline";
import type { OutlineMarker } from "@/lib/transcript-outline";

/**
 * The transcript scroll outline (#1165, finished in #1283): a strip beside the
 * scrollbar with one marker per turn, showing where you are in a long session
 * and jumping to any of them in one click. Hovering it opens a flyout of the
 * nearby turns, one line each, and a `current/total` counter says where the
 * viewport sits.
 *
 * The jump goes through the reveal bus (`store/transcript-scroll.ts`), not
 * through the virtualizer directly. That bus exists for exactly this problem —
 * bringing a row that isn't mounted into view in a windowed transcript — and
 * going through it buys three things a direct `scrollToIndex` would not:
 * addressing by message *identity*, so a history prepend (#866) can't
 * misaddress the jump; keying by session, so a click in split pane A cannot
 * scroll pane B (#148); and one place where "reveal" means one thing.
 * `find-bar.tsx`'s `activate` is the working precedent.
 *
 * No `waitForRow` here, unlike find: find waits because it then walks the DOM
 * to build paintable ranges. The outline reads nothing back, so waiting would
 * be cargo cult. A `reveal` that returns false (an id dropped by an edit or
 * truncate since the markers were built) is a no-op, which is the right
 * behaviour for a mis-click on a stale marker.
 *
 * #1182 listed hover preview cards as out of scope so the flag could be judged
 * on the navigation alone; #1283 reverses that — a 2px mark with no label is
 * not enough to aim with, and the snippets are what make the strip usable
 * rather than decorative. Still out of scope, and still for the same reasons:
 * drag-scrubbing the viewport thumb (its own interaction), roving tabindex over
 * the markers (200 buttons must not flood the tab order — the keyboard story is
 * the palette and the find bar, both of which already exist), per-message
 * rather than per-turn resolution, and any persisted state.
 *
 * Shipping default-on (#1283) does not change the tab-order call, which was
 * asked on review and is worth answering here rather than twice in a thread.
 * Nothing is reachable *only* through this strip: every jump it offers is a
 * jump the palette and the find bar already make by keyboard, and every snippet
 * it shows is the transcript's own text. So it stays a pointer-driven shortcut
 * to an existing route, and both the marks and the flyout rows keep
 * `tabIndex={-1}` — a default-on strip that put 40+ buttons ahead of the
 * composer in the tab order would make the keyboard story worse, not better.
 * What would change this: giving the outline something of its own (filtering,
 * bookmarks), at which point it needs a real roving-tabindex treatment.
 */

/** Turns listed in the flyout, centred on the hovered marker. Enough to give
 *  the hovered turn context on both sides, few enough that the flyout never
 *  becomes a second transcript. */
const FLYOUT_ROWS = 7;
/** Row height and padding, in px — used to place the flyout against the strip.
 *  Kept in sync with the row's `h-5` and the container's `py-1` + header. */
const FLYOUT_ROW_PX = 20;
const FLYOUT_PAD_PX = 10;

/** How long the position counter lingers after the viewport stops moving. */
const COUNTER_LINGER_MS = 1000;

export const TranscriptOutline = memo(function TranscriptOutline({
  sessionId,
  markers,
  total,
  firstIndex,
  lastIndex,
}: {
  sessionId: string;
  markers: readonly OutlineMarker[];
  /** `groups.length` — the denominator for the viewport thumb and the counter. */
  total: number;
  /** First and last group index currently windowed, for the thumb. Passed as
   *  numbers rather than the virtual items array, whose identity churns every
   *  render and would defeat this component's memo on every streamed token. */
  firstIndex: number;
  lastIndex: number;
}) {
  // The strip's own height decides how many marks fit as distinct, clickable
  // targets (`OUTLINE_MARKER_PITCH_PX`). Measured rather than assumed: the pane
  // is resizable, split panes halve it, and the #1182 UX pass found that
  // rendering all 200 into a 346px gutter produced 200 overlapping 2px marks.
  //
  // One observer on the strip itself — not one per marker — and it only fires
  // on an actual resize, so it stays off the streaming render path.
  const [stripEl, setStripEl] = useState<HTMLDivElement | null>(null);
  const [stripHeight, setStripHeight] = useState(0);
  useEffect(() => {
    if (!stripEl) return;
    setStripHeight(stripEl.getBoundingClientRect().height);
    const ro = new ResizeObserver(([entry]) => {
      setStripHeight(entry.contentRect.height);
    });
    ro.observe(stripEl);
    return () => ro.disconnect();
  }, [stripEl]);

  // Which of the *rendered* marks the pointer is nearest, or null when the
  // pointer is away. Local state, and this component is memo'd, so hovering
  // re-renders the strip and nothing else — never the transcript.
  const [hovered, setHovered] = useState<number | null>(null);

  // The counter is a scroll affordance, not permanent chrome: it fades in while
  // the viewport is moving and goes away shortly after it stops. Keyed on
  // `firstIndex`, so a streamed token that doesn't move the window costs
  // nothing.
  const [moving, setMoving] = useState(false);
  useEffect(() => {
    setMoving(true);
    const t = setTimeout(() => setMoving(false), COUNTER_LINGER_MS);
    return () => clearTimeout(t);
  }, [firstIndex]);

  const shown = visibleMarkers(markers, stripHeight);

  if (markers.length === 0 || total === 0) return null;

  const thumbTop = (firstIndex / total) * 100;
  const thumbHeight = Math.max(2, ((lastIndex - firstIndex + 1) / total) * 100);
  // Tile the strip: each mark owns its share of the thinned set, less a hairline
  // gap so neighbours stay readable as separate turns.
  const markHeightPct = 100 / shown.length;

  const jump = (m: OutlineMarker) => {
    useTranscriptScroll.getState().reveal(sessionId, m.messageId);
  };

  // Nearest mark to the pointer by position on the strip. Nearest-by-`startPct`
  // rather than a bucket division, because `visibleMarkers` thins by even
  // sub-sampling and survivors keep the `startPct` they were built with — the
  // rendered marks are only approximately evenly spaced.
  const onMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!stripEl || shown.length === 0) return;
    const rect = stripEl.getBoundingClientRect();
    if (rect.height <= 0) return;
    const pct = ((e.clientY - rect.top) / rect.height) * 100;
    let best = 0;
    for (let i = 1; i < shown.length; i++) {
      if (
        Math.abs(shown[i].startPct - pct) < Math.abs(shown[best].startPct - pct)
      ) {
        best = i;
      }
    }
    setHovered(best);
  };

  // The window of turns the flyout lists, centred on the hovered mark and
  // clamped at both ends of the session.
  //
  // `hovered` indexes the *thinned* set, which can shrink under a resting
  // pointer — a streamed turn rebuilds the markers, and a pane resize rethins
  // them — so it is re-validated rather than trusted. State set from one render
  // and read in a later one is exactly where an index like this goes stale.
  const open = hovered != null && hovered < shown.length;
  // Rows come from `markers`, not from the thinned `shown`: the strip drops
  // marks it has no room to render as separate targets, but the flyout has room
  // for all seven of them, and a list that skips every third turn is a worse
  // answer to "what is around here" than the neighbours themselves. The anchor
  // is found by identity — `visibleMarkers` returns the very same objects.
  const anchor = open ? Math.max(0, markers.indexOf(shown[hovered])) : 0;
  const start = open
    ? Math.max(
        0,
        Math.min(
          anchor - Math.floor(FLYOUT_ROWS / 2),
          markers.length - FLYOUT_ROWS,
        ),
      )
    : 0;
  const rows = open ? markers.slice(start, start + FLYOUT_ROWS) : [];
  // Placed against the strip in px rather than percentages so it can be clamped
  // inside the gutter: a flyout hanging off the top or bottom of a short pane
  // would be clipped by the transcript's own overflow.
  const flyoutHeight = (rows.length + 1) * FLYOUT_ROW_PX + FLYOUT_PAD_PX;
  const flyoutTop =
    open && stripHeight > 0
      ? Math.max(
          0,
          Math.min(
            (shown[hovered].startPct / 100) * stripHeight - flyoutHeight / 2,
            Math.max(0, stripHeight - flyoutHeight),
          ),
        )
      : 0;

  // Two readings of "where am I", and they are not the same question. The
  // floating pill answers it for the *viewport* — it is a scroll affordance, so
  // it tracks what is on screen. The flyout's header answers it for the turn the
  // pointer is on, which is what the rows underneath it are listing: a header
  // reading the viewport's position above rows from somewhere else invites the
  // reader to line them up and find they disagree (the first live pass caught
  // exactly that — `1/44` over rows 24-31). #1283's target pairs `742/1218`
  // with a list starting at 742, i.e. the hovered reading.
  const position = (index: number) => `${Math.min(index + 1, total)}/${total}`;
  const viewportCounter = position(firstIndex);
  const hoveredCounter =
    hovered != null && shown[hovered] ? position(shown[hovered].index) : null;

  return (
    <div
      ref={setStripEl}
      data-testid="transcript-outline"
      role="navigation"
      aria-label="Transcript outline"
      // `right-3` rather than flush: on Windows and Linux the scrollbar is
      // classic (it takes layout width), and `scrollbar-gutter: stable` on the
      // scroller reserves that width on macOS too, so the strip sits in the
      // same place everywhere instead of on top of the scrollbar on one
      // platform and beside it on another.
      className="pointer-events-none absolute inset-y-2 right-3 z-10 w-2"
    >
      {/* The hover surface, and the flyout it opens. It is deliberately the
          FIRST positioned child: the marker buttons come last and so win
          hit-testing over the part of it that overlaps the strip, which keeps
          a click on a mark a click on that mark. It is also the only
          pointer-events-auto region — the container stays inert so the outline
          never eats a text selection in the transcript. */}
      <div
        data-testid="transcript-outline-hover"
        className="pointer-events-auto absolute inset-y-0 -left-2 -right-2"
        onPointerMove={onMove}
        onPointerLeave={() => setHovered(null)}
      >
        {open && (
          <div
            data-testid="transcript-outline-flyout"
            className="absolute right-full mr-2 w-[min(22rem,55vw)] overflow-hidden rounded-md border bg-popover/95 px-1 py-1 text-xs text-popover-foreground shadow-md backdrop-blur"
            style={{ top: flyoutTop }}
          >
            {/* Where the hovered turn is, as the flyout's header — it agrees
                with the rows below it, and it is the only counter on screen
                while the flyout is open, so it can't collide with the pill. */}
            <div className="flex h-5 items-center justify-end px-1.5 text-[10px] tabular-nums text-muted-foreground">
              {hoveredCounter ?? viewportCounter}
            </div>
            {rows.map((m, i) => (
              <button
                key={m.messageId}
                type="button"
                tabIndex={-1}
                aria-label={`Jump to message ${m.index + 1} of ${total}`}
                onClick={() => jump(m)}
                className={cn(
                  "flex h-5 w-full items-center gap-2 rounded px-1.5 text-left",
                  start + i === anchor
                    ? "bg-accent text-accent-foreground"
                    : "hover:bg-accent/60",
                )}
              >
                <span className="w-9 shrink-0 text-right text-[10px] tabular-nums text-muted-foreground">
                  {m.index + 1}
                </span>
                <span className="truncate">{m.preview}</span>
              </button>
            ))}
          </div>
        )}
      </div>

      {/* The window currently rendered, so the outline shows *where you are*
          and not just what exists. Non-interactive: dragging it is a scrubber,
          which is its own interaction and out of scope here. */}
      <div
        data-testid="transcript-outline-thumb"
        className="absolute left-0 w-full rounded-full bg-foreground/10"
        style={{ top: `${thumbTop}%`, height: `${thumbHeight}%` }}
      />

      {/* The position counter while the viewport is moving. Suppressed when the
          flyout is open, which carries its own reading in its header. */}
      {moving && !open && (
        <div
          data-testid="transcript-outline-counter"
          className="absolute right-full mr-2 rounded-full border bg-popover/95 px-1.5 py-0.5 text-[10px] tabular-nums text-muted-foreground shadow-sm backdrop-blur"
          style={{ top: `${thumbTop}%` }}
        >
          {viewportCounter}
        </div>
      )}

      {shown.map((m, i) => (
        <button
          key={m.messageId}
          type="button"
          // Not in the tab order: even thinned, a strip of markers would bury
          // every other control on the pane. Pointer-driven affordance only, by
          // design — see the tab-order paragraph in the file comment, which
          // covers the flyout rows below on the same grounds.
          tabIndex={-1}
          aria-label={`Jump to message ${m.index + 1} of ${total}`}
          onClick={() => jump(m)}
          className={cn(
            "pointer-events-auto absolute left-0 w-full rounded-full transition-colors",
            i === hovered
              ? "bg-primary"
              : m.kind === "user"
                ? "bg-foreground/45"
                : "bg-foreground/15",
          )}
          style={{
            top: `${m.startPct}%`,
            // The mark's own bucket share is what it *represents*; what it
            // *occupies* is its share of the thinned set, which is what makes it
            // hittable. Capped so a short session doesn't render slabs.
            height: `max(3px, min(${markHeightPct - 0.6}%, 10px))`,
          }}
        />
      ))}
    </div>
  );
});
