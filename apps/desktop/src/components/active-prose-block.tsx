// Intermediate prose that's still streaming in: collapses to a compact "On it"
// chip once the narration gets long, so the user isn't forced to read it token
// by token (#864). When the turn settles, the chip dissolves and the full
// prose renders in place.
//
// The chip and the prose live in the same flex column and swap visibility via
// `max-height` + `opacity` transitions. Both render at all times while
// streaming so the surrounding step groups never reflow, and the content is
// ready the instant the user expands or the turn settles. Short prose and
// settled turns never show the chip — there's nothing to hide.
//
// The prose's expanded `max-height` is measured at runtime (ref + layout
// effect), not a hard-coded constant: the point of the feature is *long*
// prose, and a fixed cap (1000px) would clip a multi-paragraph narration
// during the expand animation. The measurement re-runs as text grows so a
// streaming tail never gets visually capped.
import { useLayoutEffect, useRef, useState } from "react";
import { ChevronRight } from "@/components/ui/icon";
import { cn } from "@/lib/utils";
import { Spinner } from "@/components/ui/spinner";
import { Markdown } from "@/components/markdown";

/** Char count past which streaming intermediate prose collapses to a compact
 *  "On it" chip (#864). ≈ 1 line at the chat's prose font size. The first
 *  `THRESHOLD` chars stay visible so the user can read the start of the
 *  sentence before it compresses. */
export const ACTIVE_PROSE_COLLAPSE_THRESHOLD = 80;

export function ActiveProseBlock({
  text,
  streaming,
  tone = "muted",
  caret = false,
}: {
  text: string;
  /** True when the parent turn is in flight and this prose segment is the
   *  currently-streaming one. False once the turn settles — at which point the
   *  chip dissolves and the full prose is always visible. */
  streaming: boolean;
  /** `"muted"` (default) for a settled intermediate-prose segment; `"foreground"`
   *  for the live answer slot, which reads as a normal (not muted) reply once
   *  expanded/settled — same brightness as today's final-answer text. */
  tone?: "muted" | "foreground";
  /** Appends the blinking `ff-streaming-caret` to the visible prose, matching
   *  the answer slot's existing streaming-caret treatment. Only meaningful
   *  while expanded — a collapsed chip shows its own `Spinner` instead. */
  caret?: boolean;
}) {
  // null = follow the auto-collapse. true/false = an explicit user choice that
  // sticks for the lifetime of this prose segment, so new tokens never
  // re-collapse an expanded view (same pattern as ThoughtRow and
  // ThinkingBlock).
  const [userExpanded, setUserExpanded] = useState<boolean | null>(null);
  // `collapsible` = this prose can toggle at all (long, still streaming);
  // `isCollapsed` = it's toggleable AND currently folded. The chip's *presence*
  // as an interactive control keys on `collapsible`, so a mid-stream expand
  // keeps a working collapse affordance (#986). Retirement (settled/short prose)
  // keys on `!collapsible`.
  const collapsible =
    streaming && text.length > ACTIVE_PROSE_COLLAPSE_THRESHOLD;
  const isCollapsed = collapsible && userExpanded !== true;

  // Measure the prose content's natural height so the expand transition lands
  // on the real text height — long prose (the whole point of this feature)
  // would clip at any hard cap. `scrollHeight` of the inner content div
  // reflects the un-clipped content regardless of the outer `max-height`.
  // Re-measured on every text change so a streaming tail grows smoothly; the
  // 1px threshold avoids a re-render on every sub-pixel change.
  const contentRef = useRef<HTMLDivElement>(null);
  const [naturalHeight, setNaturalHeight] = useState(0);
  useLayoutEffect(() => {
    const el = contentRef.current;
    if (!el) return;
    const h = el.scrollHeight;
    if (h > 0) {
      setNaturalHeight((prev) => (Math.abs(h - prev) > 1 ? h : prev));
    }
  }, [text]);

  return (
    <div
      data-active-prose
      data-prose-expanded={!isCollapsed}
      className="flex flex-col"
    >
      {/* The chip renders *above* the prose so that, once expanded mid-stream,
          it reads as the header/toggle for the content below it (#986 review) —
          `🔄 On it ▾` on top, prose underneath — rather than a toggle buried
          beneath the text. Collapsed looks identical (prose clips to height 0,
          leaving just the chip). */}
      <button
        type="button"
        onClick={() => setUserExpanded((v) => (v === true ? false : true))}
        aria-expanded={!isCollapsed}
        // Visible "On it" text is the accessible name (matches ThoughtRow and
        // ThinkingBlock); aria-expanded carries the live state to AT.
        // aria-hidden + tabIndex=-1 retire the button from the a11y tree once
        // the prose can no longer toggle — settled turns and short prose —
        // otherwise every such prose would announce a phantom "On it"
        // affordance (#864 review). While streaming a long prose the chip stays
        // interactive even when expanded, so it can be re-collapsed (#986).
        aria-hidden={!collapsible}
        tabIndex={!collapsible ? -1 : undefined}
        data-on-it
        className={cn(
          "flex w-full items-center gap-1.5 self-start rounded-md px-2.5 py-1 text-left transition-[max-height,opacity] duration-200 ease-out",
          collapsible
            ? "max-h-12 text-muted-foreground hover:text-foreground"
            : "pointer-events-none max-h-0 overflow-hidden opacity-0",
        )}
      >
        <Spinner className="shrink-0 text-muted-foreground" />
        <span className="font-medium text-foreground/90">On it</span>
        <ChevronRight
          className={cn(
            "size-3.5 shrink-0 transition-transform",
            !isCollapsed && "rotate-90",
          )}
        />
      </button>
      <div
        // `overflow-hidden` is required so the collapsed state clips correctly —
        // without it the prose tail bleeds past max-h-0 instead of animating to
        // zero height.
        className="overflow-hidden transition-[max-height,opacity] duration-300 ease-out"
        style={{
          maxHeight: isCollapsed ? 0 : naturalHeight,
          opacity: isCollapsed ? 0 : 1,
        }}
        aria-hidden={isCollapsed}
      >
        <div
          ref={contentRef}
          data-selectable
          data-prose-content
          className={cn(
            "px-0.5 py-1 text-sm leading-relaxed",
            tone === "foreground" ? "text-foreground" : "text-muted-foreground",
            caret && "ff-streaming-caret",
          )}
        >
          <Markdown content={text} streaming={streaming} />
        </div>
      </div>
    </div>
  );
}
