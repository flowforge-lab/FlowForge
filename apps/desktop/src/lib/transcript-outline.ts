// Marker model for the transcript scroll outline (#1165). Pure — no React, no
// virtualizer — which is the point: it is what keeps the outline honest.
//
// Positions are **index proportions**, never the virtualizer's measured
// offsets. That is not a simplification, it is the correctness requirement. A
// row that has never mounted contributes `ROW_ESTIMATE_PX` (140) rather than
// its real height, and on a long session almost every row is unmeasured — p90
// on the 9325-message session this was measured against is 35 lines, and 140px
// buys about 6. So offset-derived markers would bunch toward the top, drift
// further the lower you go, and *move as you scroll*, because each newly
// mounted row replaces its estimate and shifts every offset below it. Index
// proportion is stable, needs no measurement, and is what a minimap is for:
// finding a turn, not depicting pixel geometry.
//
// Real proportional-height markers would need a height model (a per-message
// estimate from content length, or persisted measurements). That is its own
// piece of work and deliberately does not gate this one.

import type { Message } from "@/bindings";
import type { RenderGroup } from "@/lib/turn-groups";

/** Ceiling on rendered markers. A 9000-message session folds to thousands of
 *  groups, and one node each would re-introduce exactly the mount cost #1143
 *  removed — on the element that overlays the transcript, no less. The gutter
 *  is a few hundred px tall, so 200 is already finer than it can resolve. */
export const OUTLINE_MAX_MARKERS = 200;

/** Below this the outline is noise: a session that fits in a viewport or two
 *  has nothing to navigate, and a strip of three dots is just clutter. */
export const OUTLINE_MIN_GROUPS = 12;

/** How far back from the tail, in raw messages, before the navigator (#1290) is
 *  worth offering. Scrolling within the last few messages is reading, not
 *  navigating, and a pill that appears there is chrome the reader didn't ask
 *  for — the whole point of replacing the always-on strip. */
export const NAVIGATOR_MIN_SCROLLBACK = 5;

/** Row pitch of the navigator popup's list, in px — the popup's answer to
 *  `OUTLINE_MARKER_PITCH_PX`. Kept in sync with the row's `h-5`. */
export const NAVIGATOR_ROW_PX = 20;

/**
 * Vertical space each rendered marker gets, in px — its height plus the gap to
 * the next one.
 *
 * `OUTLINE_MAX_MARKERS` alone is not enough, which the #1182 UX pass measured:
 * on a 9000-message session the gutter was 346px tall and all 200 markers came
 * out at the 2px floor — 400px of marks in 346px of strip. They overlapped into
 * a solid bar with no separation, and every click target was ~1.7px of unique
 * area. The maths was right and the affordance was unusable.
 *
 * 7px keeps marks visually distinct and gives a pointer something to hit. It is
 * the binding constraint on any strip shorter than ~1400px, i.e. all of them.
 */
export const OUTLINE_MARKER_PITCH_PX = 7;

/** Never thin out below this, so a short strip still reads as an outline. */
const MIN_VISIBLE_MARKERS = 8;

/** How much of a turn the hover flyout shows (#1283). One line at the flyout's
 *  width; the row truncates with an ellipsis on top of this, so the cap is
 *  about not carrying whole messages around rather than about layout. */
export const SNIPPET_MAX_CHARS = 72;

/** What a marker reads as when its representative has no text of its own — an
 *  assistant turn that ended in a tool call, or a raw tool/system group. Better
 *  than an empty row, which reads as a rendering bug. */
const EMPTY_PREVIEW: Record<RenderGroup["kind"], string> = {
  user: "(empty message)",
  assistant: "(no text in this turn)",
  loose: "(tool output)",
};

/**
 * One line of a turn, for the flyout: whitespace collapsed (a message is
 * markdown, so its first line is often a heading or a fence), trimmed, and cut
 * to `SNIPPET_MAX_CHARS`.
 */
export function snippet(content: string, kind: RenderGroup["kind"]): string {
  const flat = content.replace(/\s+/g, " ").trim();
  if (!flat) return EMPTY_PREVIEW[kind];
  if (flat.length <= SNIPPET_MAX_CHARS) return flat;
  return `${flat.slice(0, SNIPPET_MAX_CHARS - 1).trimEnd()}…`;
}

export interface OutlineMarker {
  /** Index into `groups` of the marker's representative. */
  index: number;
  /** What `useTranscriptScroll.reveal` is called with — identity, not index,
   *  so a prepended history (#866) can't misaddress the jump. */
  messageId: string;
  /** Drives the marker's weight: a user turn is the landmark you scan for. */
  kind: "user" | "assistant";
  /** Top edge, as a percentage of the gutter. */
  startPct: number;
  /** Height, as a percentage of the gutter — the share of the session this
   *  marker's bucket covers. */
  heightPct: number;
  /** One line of the representative turn, for the hover flyout (#1283).
   *  Computed here rather than in the component so it is built once per marker
   *  set, under the same `outlineKey` memo that keeps the O(n) walk off the
   *  per-token render path — not once per hover, per row, per frame. */
  preview: string;
}

const EMPTY: OutlineMarker[] = [];

/**
 * One marker per bucket of `maxMarkers` even slices of `groups`.
 *
 * The unit is a *group*, not a message: `foldTurns` makes a group either one
 * user message or one whole assistant turn with its tool/system followers
 * folded in, so one marker per group is one dot per turn rather than one per
 * raw tool result. (It also means index-into-`groups` only — `groups` and
 * `messages` are different lengths, because `foldTurns` drops mode-switch
 * markers, and drifting between the two is how off-by-N jump bugs happen.)
 *
 * Each bucket's representative is its first user group when it has one, else
 * its first group: clicking a marker should land at the start of an exchange,
 * and the user's own message is the recognisable end of it.
 */
export function buildOutline(
  groups: readonly RenderGroup[],
  maxMarkers: number = OUTLINE_MAX_MARKERS,
): OutlineMarker[] {
  const len = groups.length;
  if (len === 0) return EMPTY;

  const size = Math.ceil(len / maxMarkers);
  const markers: OutlineMarker[] = [];
  for (let start = 0; start < len; start += size) {
    const end = Math.min(len, start + size);
    let index = start;
    for (let i = start; i < end; i++) {
      if (groups[i].kind === "user") {
        index = i;
        break;
      }
    }
    markers.push({
      index,
      messageId: groups[index].message.id,
      kind: groups[index].kind === "user" ? "user" : "assistant",
      startPct: (start / len) * 100,
      heightPct: ((end - start) / len) * 100,
      preview: snippet(groups[index].message.content, groups[index].kind),
    });
  }
  return markers;
}

/**
 * Thin `markers` down to what `heightPx` of gutter can actually render as
 * distinct, clickable marks — see `OUTLINE_MARKER_PITCH_PX`.
 *
 * Even sub-sampling rather than re-bucketing: every survivor keeps the
 * `startPct` and `messageId` it was built with, so positions stay index
 * proportional and a click still lands on a real turn. Re-bucketing would be
 * marginally prettier and would put a layout measurement back into the marker
 * maths, which is what `buildOutline` is deliberately kept free of.
 *
 * `heightPx <= 0` means "not measured yet" (first paint, or jsdom, which has no
 * layout) — return everything and let the next frame thin it, rather than
 * guessing a height and rendering the wrong count.
 *
 * `pitchPx` is a parameter rather than a second function because the navigator
 * popup (#1290) thins the same markers against 20px rows instead of 2px marks;
 * the maths is identical and only the unit of "what fits" differs.
 */
export function visibleMarkers(
  markers: readonly OutlineMarker[],
  heightPx: number,
  pitchPx: number = OUTLINE_MARKER_PITCH_PX,
): readonly OutlineMarker[] {
  if (heightPx <= 0 || markers.length === 0) return markers;
  const budget = Math.max(MIN_VISIBLE_MARKERS, Math.floor(heightPx / pitchPx));
  if (markers.length <= budget) return markers;
  const step = Math.ceil(markers.length / budget);
  return markers.filter((_, i) => i % step === 0);
}

/**
 * Memo key for `buildOutline`, so the O(n) walk stays off the per-token render
 * path — `groups` gets a fresh array on every streamed token as the tail
 * re-folds, but the markers only change when a group is added or removed.
 *
 * Length alone is not enough: an edit or truncate can replace messages at the
 * same count, and the first id is what the #866 history swap changes.
 *
 * `len:first:last` assumes every mutation is **tail-anchored** — it cannot see a
 * change confined to the middle of the transcript, and would serve stale
 * markers if one happened. That holds today because the store has no in-place
 * edit: `editMessage` (`store/chat.ts`) truncates with `prior.slice(0, idx)`
 * and appends, so an edited message becomes the *last* one and both the length
 * and the tail id move. The assumption is written down here because it is
 * load-bearing knowledge that otherwise lives only in another file: if a true
 * in-place edit is ever added, this key goes subtly stale and nothing else
 * points back at it. Widening it then means hashing the ids, which is the cost
 * this key exists to avoid — the per-token stability asserted in
 * `transcript-outline.test.ts` is the property to preserve.
 */
export function outlineKey(groups: readonly RenderGroup[]): string {
  const len = groups.length;
  if (len === 0) return "0";
  return `${len}:${groups[0].message.id}:${groups[len - 1].message.id}`;
}

/**
 * The 1-based ordinal of the **last raw message each group covers** — the
 * counter's numerator (#1290), one entry per group.
 *
 * The navigator counts raw messages, not groups, because "how far down am I"
 * is a question about the transcript the user scrolled, and `groups` is a
 * folded view of it: `foldTurns` drops mode-switch markers and swallows a
 * turn's tool/system followers, so a group count answers a different question
 * than the one the pill appears to be answering.
 *
 * Exclusive-end is the load-bearing choice, not a detail. The obvious mapping —
 * "where does this group's own message sit in `messages`" — is wrong at the
 * tail: an assistant group's `message` is the turn's *final assistant* message,
 * with its tool results folded in *after* it, so a turn ending in eight tool
 * calls would report ordinal `total - 8` while the viewport is resting at the
 * very bottom. That is a scrollback of 8 at rest, i.e. the pill fading in over
 * state 1 on the most common shape of a verification transcript. Defining the
 * ordinal as the start of the *next* group instead makes
 * `ordinals.at(-1) === messages.length` true by construction, so "at the tail ⇒
 * scrollback 0" is exact rather than approximate.
 *
 * One forward walk, no `Map`: group representatives appear in `messages` in
 * strictly increasing order, so a single cursor finds them all. A
 * representative that isn't in `messages` (it shouldn't happen — groups are
 * derived from them) clamps to the previous value, keeping the array monotonic
 * instead of throwing on the render path.
 */
export function messageOrdinals(
  groups: readonly RenderGroup[],
  messages: readonly Message[],
): number[] {
  const len = groups.length;
  if (len === 0) return [];

  // `starts[i]` = index in `messages` of group i's representative.
  const starts: number[] = new Array(len);
  let cursor = 0;
  for (let i = 0; i < len; i++) {
    const id = groups[i].message.id;
    let found = -1;
    for (let j = cursor; j < messages.length; j++) {
      if (messages[j].id === id) {
        found = j;
        break;
      }
    }
    if (found < 0) {
      // Unknown id: don't rewind the cursor and don't go backwards.
      starts[i] = i === 0 ? 0 : starts[i - 1];
      continue;
    }
    starts[i] = found;
    cursor = found + 1;
  }

  const ordinals: number[] = new Array(len);
  for (let i = 0; i < len - 1; i++) {
    // The next group starts where this one's span ends, so that index is also
    // this group's count of messages-so-far.
    ordinals[i] = Math.max(starts[i + 1], i === 0 ? 1 : ordinals[i - 1]);
  }
  ordinals[len - 1] = Math.max(
    messages.length,
    len > 1 ? ordinals[len - 2] : 1,
  );
  return ordinals;
}
