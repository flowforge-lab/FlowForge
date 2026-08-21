// The marker model behind the transcript outline (#1165).
//
// The load-bearing property is the one that is easiest to "simplify" away
// later: positions come from index proportion, and markers are bucketed. Both
// have a test whose failure message says why.

import { describe, expect, it } from "vitest";

import {
  OUTLINE_MARKER_PITCH_PX,
  OUTLINE_MAX_MARKERS,
  SNIPPET_MAX_CHARS,
  buildOutline,
  outlineKey,
  snippet,
  visibleMarkers,
} from "@/lib/transcript-outline";
import type { RenderGroup } from "@/lib/turn-groups";

/** Minimal groups — `buildOutline` reads `kind`, `message.id` and (for the
 *  #1283 flyout snippet) `message.content`. */
function groups(kinds: ("user" | "assistant")[]): RenderGroup[] {
  return kinds.map(
    (kind, i) =>
      ({
        kind,
        message: { id: `m${i}`, content: `${kind} turn ${i}` },
        ...(kind === "assistant"
          ? { items: [], steps: [], reasoning: "", durationMs: null }
          : {}),
      }) as RenderGroup,
  );
}

const alternating = (n: number) =>
  groups(
    Array.from({ length: n }, (_, i) => (i % 2 === 0 ? "user" : "assistant")),
  );

describe("buildOutline (#1165)", () => {
  it("bounds the marker count on a session with thousands of turns", () => {
    const markers = buildOutline(alternating(9000));

    // The bound itself. One node per group would put ~9000 absolutely
    // positioned buttons over the transcript — reintroducing exactly the mount
    // cost #1143 removed, on the element that overlays it.
    expect(markers.length).toBeLessThanOrEqual(OUTLINE_MAX_MARKERS);
    // And it must still be a full outline, not a handful of dots.
    expect(markers.length).toBeGreaterThanOrEqual(OUTLINE_MAX_MARKERS - 1);
  });

  it("renders one marker per group when the session is short enough", () => {
    const markers = buildOutline(alternating(30));

    expect(markers).toHaveLength(30);
    expect(markers[0].heightPct).toBeCloseTo(100 / 30);
    expect(markers.map((m) => m.index)).toEqual(
      Array.from({ length: 30 }, (_, i) => i),
    );
  });

  it("positions every marker by index proportion", () => {
    const all = alternating(9000);
    const markers = buildOutline(all);

    // Not "approximately proportional": exactly, for every marker. An
    // offset-derived position (the virtualizer's `start`, mostly a stack of
    // 140px estimates) would fail this by a margin that grows down the list,
    // which is also how it would look on screen.
    for (const m of markers) {
      const bucketStart = Math.round((m.startPct / 100) * all.length);
      expect(m.index).toBeGreaterThanOrEqual(bucketStart);
      expect(m.index).toBeLessThan(
        bucketStart + Math.ceil(all.length / OUTLINE_MAX_MARKERS),
      );
    }
    expect(markers[0].startPct).toBe(0);
    // The strip is fully covered — no gaps, no overflow past the gutter.
    const last = markers[markers.length - 1];
    expect(last.startPct + last.heightPct).toBeCloseTo(100);
  });

  it("represents a bucket by its first user turn, not merely its first group", () => {
    // A bucket of 3: assistant, user, assistant. The user message is the
    // landmark a reader scans for, and jumping to it lands at the start of the
    // exchange rather than mid-answer.
    const markers = buildOutline(groups(["assistant", "user", "assistant"]), 1);

    expect(markers).toHaveLength(1);
    expect(markers[0].messageId).toBe("m1");
    expect(markers[0].kind).toBe("user");
  });

  it("falls back to the bucket's first group when it holds no user turn", () => {
    const markers = buildOutline(groups(["assistant", "assistant"]), 1);

    expect(markers[0].messageId).toBe("m0");
    expect(markers[0].kind).toBe("assistant");
  });

  it("handles the degenerate sessions", () => {
    expect(buildOutline([])).toEqual([]);

    const one = buildOutline(groups(["user"]));
    expect(one).toHaveLength(1);
    expect(one[0].startPct).toBe(0);
    expect(one[0].heightPct).toBe(100);
  });
});

describe("visibleMarkers (#1165 UX pass)", () => {
  // The defect this exists for, in numbers: a 9000-message session in a 346px
  // gutter rendered all 200 markers at the 2px floor — 400px of marks in 346px
  // of strip, overlapping into a solid bar with ~1.7px of unique click area per
  // marker. Measured in a real browser, since jsdom has no layout to expose it.
  const REAL_GUTTER_PX = 346;

  it("thins a full outline to what the gutter can render as separate marks", () => {
    const shown = visibleMarkers(
      buildOutline(alternating(9000)),
      REAL_GUTTER_PX,
    );

    expect(shown.length).toBeLessThanOrEqual(
      Math.floor(REAL_GUTTER_PX / OUTLINE_MARKER_PITCH_PX),
    );
    // Each surviving mark now owns at least the pitch, so it is a real target
    // rather than a 2px sliver overlapping its neighbours.
    expect(REAL_GUTTER_PX / shown.length).toBeGreaterThanOrEqual(
      OUTLINE_MARKER_PITCH_PX,
    );
  });

  it("keeps survivors' positions and ids exactly as built", () => {
    const all = buildOutline(alternating(9000));
    const shown = visibleMarkers(all, REAL_GUTTER_PX);

    // Sub-sampling, not re-bucketing: a click still lands on a turn that
    // exists, at the proportional position it was built with.
    for (const m of shown) {
      expect(all).toContain(m);
    }
    expect(shown[0]).toBe(all[0]);
  });

  it("returns everything when the strip is short enough to hold it", () => {
    const all = buildOutline(alternating(40));
    expect(visibleMarkers(all, 4000)).toBe(all);
  });

  it("returns everything while the height is still unmeasured", () => {
    // First paint, and jsdom — guessing a height would render the wrong count
    // for a frame; the observer corrects it on the next one.
    const all = buildOutline(alternating(9000));
    expect(visibleMarkers(all, 0)).toBe(all);
  });

  it("still shows an outline in a very short strip", () => {
    const shown = visibleMarkers(buildOutline(alternating(9000)), 20);
    expect(shown.length).toBeGreaterThanOrEqual(8);
  });
});

describe("outlineKey (#1165)", () => {
  it("changes when a turn is appended", () => {
    expect(outlineKey(alternating(10))).not.toBe(outlineKey(alternating(11)));
  });

  it("changes when the head changes at the same length", () => {
    // The #866 history swap prepends older messages: same count is possible,
    // same markers is not.
    const a = alternating(10);
    const b = alternating(10);
    (b[0] as { message: { id: string } }).message = { id: "prepended" };

    expect(outlineKey(a)).not.toBe(outlineKey(b));
  });

  it("changes when the tail is replaced at the same length (edit/truncate)", () => {
    const a = alternating(10);
    const b = alternating(10);
    (b[9] as { message: { id: string } }).message = { id: "edited" };

    expect(outlineKey(a)).not.toBe(outlineKey(b));
  });

  it("is stable across a streamed token, which only mutates a message body", () => {
    // The memo this keys must not rebuild per token: `groups` is a fresh array
    // every time the tail re-folds, but the outline is unchanged.
    const before = alternating(10);
    const after = alternating(10);

    expect(outlineKey(after)).toBe(outlineKey(before));
    expect(after).not.toBe(before);
  });
});

describe("snippet (#1283)", () => {
  it("flattens a markdown message to one line", () => {
    expect(snippet("## Heading\n\nthen   the  body", "assistant")).toBe(
      "## Heading then the body",
    );
  });

  it("truncates at the cap", () => {
    const out = snippet("a".repeat(SNIPPET_MAX_CHARS * 2), "user");

    expect(out).toHaveLength(SNIPPET_MAX_CHARS);
    expect(out.endsWith("a…")).toBe(true);
  });

  it("does not leave a space stranded before the ellipsis", () => {
    const out = snippet(`${"a".repeat(SNIPPET_MAX_CHARS - 2)} tail`, "user");

    expect(out.endsWith("a…")).toBe(true);
  });

  it("leaves a message that already fits exactly as it is", () => {
    expect(snippet("short enough", "user")).toBe("short enough");
  });

  // An assistant turn that ended in a tool call has no text of its own, and an
  // empty row reads as a rendering bug rather than as a turn.
  it("names the kind when the turn has no text", () => {
    expect(snippet("", "assistant")).toBe("(no text in this turn)");
    expect(snippet("   \n ", "loose")).toBe("(tool output)");
    expect(snippet("", "user")).toBe("(empty message)");
  });

  it("is carried on every built marker", () => {
    const markers = buildOutline(alternating(40));

    expect(markers.every((m) => m.preview.length > 0)).toBe(true);
    expect(markers[0].preview).toBe("user turn 0");
  });
});
