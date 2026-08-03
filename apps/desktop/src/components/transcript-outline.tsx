import { memo, useEffect, useState } from "react";
import { cn } from "@/lib/utils";
import { useTranscriptScroll } from "@/store/transcript-scroll";
import { visibleMarkers } from "@/lib/transcript-outline";
import type { OutlineMarker } from "@/lib/transcript-outline";

/**
 * The transcript scroll outline (#1165): a thin strip beside the scrollbar with
 * one marker per turn, showing where you are in a long session and jumping to
 * any of them in one click.
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
 * Deliberately out of scope, so the flag can be judged on the navigation
 * itself: hover preview cards, drag-scrubbing the viewport thumb, roving
 * tabindex over the markers (200 buttons must not flood the tab order — the
 * keyboard story is the palette and the find bar, both of which already exist),
 * per-message rather than per-turn resolution, and any persisted state.
 */
export const TranscriptOutline = memo(function TranscriptOutline({
  sessionId,
  markers,
  total,
  firstIndex,
  lastIndex,
}: {
  sessionId: string;
  markers: readonly OutlineMarker[];
  /** `groups.length` — the denominator for the viewport thumb. */
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

  const shown = visibleMarkers(markers, stripHeight);

  if (markers.length === 0 || total === 0) return null;

  const thumbTop = (firstIndex / total) * 100;
  const thumbHeight = Math.max(2, ((lastIndex - firstIndex + 1) / total) * 100);
  // Tile the strip: each mark owns its share of the thinned set, less a hairline
  // gap so neighbours stay readable as separate turns.
  const markHeightPct = 100 / shown.length;

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
      {/* The window currently rendered, so the outline shows *where you are*
          and not just what exists. Non-interactive: dragging it is a scrubber,
          which is its own interaction and out of scope here. */}
      <div
        data-testid="transcript-outline-thumb"
        className="absolute left-0 w-full rounded-sm bg-foreground/10"
        style={{ top: `${thumbTop}%`, height: `${thumbHeight}%` }}
      />
      {shown.map((m) => (
        <button
          key={m.messageId}
          type="button"
          // Not in the tab order: even thinned, a strip of markers would bury
          // every other control on the pane. Pointer-driven affordance only, by
          // design (see above).
          tabIndex={-1}
          aria-label={`Jump to message ${m.index + 1} of ${total}`}
          onClick={() => {
            useTranscriptScroll.getState().reveal(sessionId, m.messageId);
          }}
          className={cn(
            "pointer-events-auto absolute left-0 w-full rounded-full transition-colors hover:bg-primary focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring",
            m.kind === "user" ? "bg-foreground/40" : "bg-foreground/15",
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
