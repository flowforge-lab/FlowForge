import { memo, useCallback, useEffect, useRef, useState } from "react";
import { ListTree } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { useTranscriptScroll } from "@/store/transcript-scroll";
import { useMessageNavigator } from "@/store/message-navigator";
import {
  NAVIGATOR_MIN_SCROLLBACK,
  NAVIGATOR_ROW_PX,
  visibleMarkers,
} from "@/lib/transcript-outline";
import type { OutlineMarker } from "@/lib/transcript-outline";

/**
 * The transcript message navigator (#1290) — successor to the always-on
 * outline strip (#1165/#1283), which this replaces outright.
 *
 * The strip's problem was affordance, not correctness. In an assistant-heavy
 * session almost every bucket is assistant-only and renders at
 * `bg-foreground/15`, so a column of live jump targets read as inert
 * decoration — a clickable control that looks disabled, on screen whether you
 * wanted it or not. So the same marker model is disclosed progressively
 * instead:
 *
 *   1. resting, or within `NAVIGATOR_MIN_SCROLLBACK` of the tail — nothing;
 *   2. scrolling, further back than that — a `current/total` pill fades in and
 *      fades out shortly after scrolling stops;
 *   3. clicking the pill (or ⌘⇧O) — a popup of the same rows the old flyout
 *      showed, `ordinal + snippet`, one click to jump.
 *
 * The data model is untouched: `buildOutline` / `visibleMarkers` / `snippet`
 * still do the work, and the jump still goes through the reveal bus
 * (`store/transcript-scroll.ts`) for the reasons #1283 wrote down — addressing
 * by message *identity* so a history prepend (#866) can't misaddress it, keying
 * by session so a click in split pane A can't scroll pane B (#148), and one
 * definition of "reveal". A `reveal` that returns false (an id dropped by an
 * edit since the markers were built) is a no-op, which is the right answer to a
 * click on a stale row.
 *
 * What *did* change is the numbering. The strip counted groups, so its counter
 * and its `aria-label`s were in units the reader has no way to see; the pill
 * says "message 742 of 1218" and means raw messages, via `messageOrdinals`.
 *
 * The tab-order objection #1283 answered no longer applies, because there are
 * no longer 40+ markers to flood it with: the navigator is one button and one
 * popup, so it is fully keyboard-reachable — ⌘⇧O opens it (bypassing the
 * scrollback threshold, since a keyboard user asking for it explicitly should
 * get the list), then ↑/↓/Enter/Esc.
 */

/** How long the pill lingers after the last scroll event, and how long the fade
 *  out takes before it unmounts. `IDLE` matches the strip's old counter linger;
 *  `FADE` is short enough that the pill is gone by the time you've stopped
 *  looking at it, long enough to read as a fade rather than a cut. */
const NAVIGATOR_FADE_MS = 200;

export const MessageNavigator = memo(function MessageNavigator({
  sessionId,
  markers,
  ordinals,
  totalMessages,
  lowestIndex,
  scrolling,
  atBottom,
}: {
  sessionId: string;
  markers: readonly OutlineMarker[];
  /** Group index → 1-based raw-message ordinal (`messageOrdinals`). Must be a
   *  stable reference — a fresh array per streamed token would defeat the memo
   *  this component is wrapped in, which is the whole reason the numeric props
   *  below are numbers and not the virtual items array. */
  ordinals: readonly number[];
  /** `msgs.length` — the counter's denominator. */
  totalMessages: number;
  /** Group index of the lowest row currently on screen: "how far down am I". */
  lowestIndex: number;
  /** True while the transcript is being scrolled, false shortly after it stops. */
  scrolling: boolean;
  /** True while pinned at the tail. A second guard beside the scrollback
   *  threshold, so a pin-time scroll event can't flash the pill at rest even if
   *  the ordinals are momentarily behind the DOM. */
  atBottom: boolean;
}) {
  const open = useMessageNavigator((s) => s.openSessions.has(sessionId));
  const closeNavigator = useMessageNavigator((s) => s.closeNavigator);
  const openNavigator = useMessageNavigator((s) => s.openNavigator);

  const current =
    ordinals.length === 0
      ? 0
      : (ordinals[Math.min(Math.max(lowestIndex, 0), ordinals.length - 1)] ??
        0);
  const scrollback = totalMessages - current;

  // A brief window where the pill stays put regardless of scroll activity, so
  // closing the popup doesn't yank the button out from under the focus radix is
  // about to restore to it — a keyboard user would land on <body>.
  const [linger, setLinger] = useState(false);
  useEffect(() => {
    if (!linger) return;
    const t = setTimeout(() => setLinger(false), 1000);
    return () => clearTimeout(t);
  }, [linger]);

  const active =
    open ||
    linger ||
    (scrolling && !atBottom && scrollback > NAVIGATOR_MIN_SCROLLBACK);

  // Fade out, then unmount — `active` flips synchronously, so without the
  // deferred unmount the pill would cut rather than fade.
  const [mounted, setMounted] = useState(false);
  useEffect(() => {
    if (active) {
      setMounted(true);
      return;
    }
    const t = setTimeout(() => setMounted(false), NAVIGATOR_FADE_MS);
    return () => clearTimeout(t);
  }, [active]);

  // The popup's own height decides how many rows fit, exactly as the strip's
  // height decided how many marks did — same thinner, different pitch.
  const [listEl, setListEl] = useState<HTMLDivElement | null>(null);
  const [listHeight, setListHeight] = useState(0);
  useEffect(() => {
    if (!listEl) return;
    setListHeight(listEl.getBoundingClientRect().height);
    const ro = new ResizeObserver(([entry]) => {
      setListHeight(entry.contentRect.height);
    });
    ro.observe(listEl);
    return () => ro.disconnect();
  }, [listEl]);

  const shown = visibleMarkers(markers, listHeight, NAVIGATOR_ROW_PX);

  // Which row ↑/↓ is on. Seeded to where the reader actually is rather than to
  // row 1, so the first ArrowDown moves one turn forward from here.
  const [selected, setSelected] = useState(0);
  const selectedRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!open) return;
    let seed = 0;
    for (let i = 0; i < shown.length; i++) {
      if ((ordinals[shown[i].index] ?? 0) <= current) seed = i;
      else break;
    }
    setSelected(seed);
    // Seeded once per opening: re-seeding as `current` drifts under a streaming
    // tail would move the selection out from under the arrow keys.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);
  useEffect(() => {
    // Optional call, not just optional chaining: jsdom has no layout and so no
    // `scrollIntoView`, and this is a nicety rather than behaviour.
    selectedRef.current?.scrollIntoView?.({ block: "nearest" });
  }, [selected]);

  const close = useCallback(() => {
    closeNavigator(sessionId);
    setLinger(true);
  }, [closeNavigator, sessionId]);

  const jump = useCallback(
    (m: OutlineMarker) => {
      useTranscriptScroll.getState().reveal(sessionId, m.messageId);
      close();
    },
    [close, sessionId],
  );

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      // Without this the popup's own overflow scrolls under the selection.
      e.preventDefault();
      const delta = e.key === "ArrowDown" ? 1 : -1;
      // Clamped, not wrapped: this is a position in a transcript, and jumping
      // from the last turn to the first is disorienting rather than convenient.
      setSelected((i) => Math.max(0, Math.min(shown.length - 1, i + delta)));
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      const m = shown[selected];
      if (m) jump(m);
    }
  };

  if (markers.length === 0 || totalMessages === 0 || !mounted) return null;

  const label = (m: OutlineMarker) =>
    `Jump to message ${ordinals[m.index] ?? m.index + 1} of ${totalMessages}`;

  return (
    <div
      data-testid="message-navigator"
      role="navigation"
      aria-label="Message navigator"
      // `right-3` rather than flush, and `scrollbar-gutter: stable` on the
      // scroller: the pill then sits in the same place on macOS as on a
      // platform whose scrollbar takes layout width.
      className="absolute right-3 top-2 z-10"
    >
      <Popover
        open={open}
        onOpenChange={(next) => (next ? openNavigator(sessionId) : close())}
      >
        <TooltipProvider delayDuration={300}>
          <Tooltip>
            <TooltipTrigger asChild>
              <PopoverTrigger asChild>
                <button
                  type="button"
                  data-testid="message-navigator-pill"
                  // `title` as well as the tooltip, so the affordance survives
                  // in an environment with no hover (and so tests can find the
                  // pill without driving radix's timers).
                  title="Message navigator"
                  aria-label="Message navigator"
                  className={cn(
                    "flex items-center gap-1 rounded-full border bg-background/95 px-2 py-0.5 text-[10px] tabular-nums text-muted-foreground shadow-sm backdrop-blur transition-opacity hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40",
                    // Invisible *and* inert while fading out, so a click in
                    // the moment after it goes can't land on a ghost.
                    active ? "opacity-100" : "pointer-events-none opacity-0",
                  )}
                >
                  <ListTree className="size-3" aria-hidden />
                  <span>
                    {current}/{totalMessages}
                  </span>
                </button>
              </PopoverTrigger>
            </TooltipTrigger>
            <TooltipContent side="left">Message navigator</TooltipContent>
          </Tooltip>
        </TooltipProvider>

        <PopoverContent
          data-testid="message-navigator-popup"
          side="left"
          align="start"
          role="listbox"
          aria-label="Message navigator"
          tabIndex={-1}
          onKeyDown={onKeyDown}
          className="max-h-[60vh] w-[min(24rem,60vw)] overflow-y-auto p-1 text-xs"
        >
          <div ref={setListEl}>
            {shown.map((m, i) => (
              <div
                key={m.messageId}
                ref={i === selected ? selectedRef : undefined}
                data-testid="message-navigator-row"
                data-message-id={m.messageId}
                role="option"
                aria-selected={i === selected}
                aria-label={label(m)}
                onClick={() => jump(m)}
                className={cn(
                  "flex h-5 w-full cursor-pointer items-center gap-2 rounded px-1.5 text-left",
                  i === selected
                    ? "bg-accent text-accent-foreground"
                    : "hover:bg-accent/60",
                )}
              >
                <span className="w-9 shrink-0 text-right text-[10px] tabular-nums text-muted-foreground">
                  {ordinals[m.index] ?? m.index + 1}
                </span>
                <span className="truncate">{m.preview}</span>
              </div>
            ))}
          </div>
        </PopoverContent>
      </Popover>
    </div>
  );
});
