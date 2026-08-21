// @vitest-environment jsdom
//
// The outline strip itself (#1165): what a click does, and how many nodes it
// costs. The end-to-end jump — click a marker, that message is on screen — is
// in `chat-view.outline.test.tsx`, because only ChatView owns the virtualizer
// that mounts the row.
//
// The click is asserted through the *real* reveal bus rather than a spy on the
// component: registering a revealer and checking what id it receives is the
// same contract `find-bar.tsx` depends on, so a change that quietly stops going
// through the bus fails here.

import { act, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { TranscriptOutline } from "@/components/transcript-outline";
import { buildOutline } from "@/lib/transcript-outline";
import { useTranscriptScroll } from "@/store/transcript-scroll";
import type { RenderGroup } from "@/lib/turn-groups";

const SID = "s1";

function groups(n: number): RenderGroup[] {
  return Array.from({ length: n }, (_, i) =>
    i % 2 === 0
      ? ({
          kind: "user",
          message: { id: `m${i}`, content: `question ${i}` },
        } as unknown as RenderGroup)
      : ({
          kind: "assistant",
          message: { id: `m${i}`, content: `answer ${i}` },
          items: [],
          steps: [],
          reasoning: "",
          durationMs: null,
        } as unknown as RenderGroup),
  );
}

function renderOutline(count: number, firstIndex = 0, lastIndex = 0) {
  const markers = buildOutline(groups(count));
  const view = render(
    <TranscriptOutline
      sessionId={SID}
      markers={markers}
      total={count}
      firstIndex={firstIndex}
      lastIndex={lastIndex}
    />,
  );
  return { ...view, markers };
}

afterEach(() => {
  useTranscriptScroll.setState({ revealers: {} });
});

/** jsdom has no layout, so the strip reports a zero-height rect and the hover
 *  maths has nothing to divide by. Give it the gutter of a real pane. */
function stubStripHeight(container: HTMLElement, height = 1000) {
  const strip = container.querySelector<HTMLElement>(
    '[data-testid="transcript-outline"]',
  )!;
  strip.getBoundingClientRect = () =>
    ({
      top: 0,
      bottom: height,
      height,
      left: 0,
      right: 8,
      width: 8,
    }) as DOMRect;
  return strip;
}

/** Move the pointer to `fraction` of the way down the strip. */
function hoverAt(container: HTMLElement, fraction: number, height = 1000) {
  stubStripHeight(container, height);
  const surface = container.querySelector(
    '[data-testid="transcript-outline-hover"]',
  )!;
  act(() => {
    surface.dispatchEvent(
      new MouseEvent("pointermove", {
        bubbles: true,
        clientY: fraction * height,
      }),
    );
  });
}

const flyout = (c: HTMLElement) =>
  c.querySelector<HTMLElement>('[data-testid="transcript-outline-flyout"]');
const flyoutRows = (c: HTMLElement) =>
  Array.from(
    c.querySelectorAll<HTMLButtonElement>(
      '[data-testid="transcript-outline-flyout"] button',
    ),
  );

describe("TranscriptOutline (#1165)", () => {
  it("reveals the clicked marker's message through the bus", () => {
    const revealed: string[] = [];
    useTranscriptScroll.getState().register(SID, (id) => {
      revealed.push(id);
      return true;
    });
    const { container, markers } = renderOutline(40);

    const buttons = container.querySelectorAll("button");
    act(() => {
      buttons[7].dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    // By message id, not index: identity is what survives a history prepend
    // (#866), and it is what the bus takes.
    expect(revealed).toEqual([markers[7].messageId]);
  });

  it("does not reveal into another pane's session", () => {
    // Split panes (#148): each transcript registers under its own session id,
    // so a click here must not reach the other one.
    const otherPane: string[] = [];
    useTranscriptScroll.getState().register("s2", (id) => {
      otherPane.push(id);
      return true;
    });
    const { container } = renderOutline(40);

    act(() => {
      container
        .querySelectorAll("button")[3]
        .dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    expect(otherPane).toEqual([]);
  });

  it("survives a click on a marker whose message is gone", () => {
    // No revealer registered at all — the pane closed, or the id was dropped by
    // an edit. `reveal` returns false and the click is a no-op, not a throw.
    const { container } = renderOutline(40);

    expect(() =>
      act(() => {
        container
          .querySelectorAll("button")[0]
          .dispatchEvent(new MouseEvent("click", { bubbles: true }));
      }),
    ).not.toThrow();
  });

  it("keeps the node count bounded on a session with thousands of turns", () => {
    const { container } = renderOutline(9000);

    // The strip overlays the transcript, so one node per turn would undo #1143
    // on the very element that sits on top of it.
    expect(container.querySelectorAll("button").length).toBeLessThanOrEqual(
      200,
    );
    expect(container.querySelectorAll("button").length).toBeGreaterThan(0);
  });

  it("places the viewport thumb over the windowed range", () => {
    const { container } = renderOutline(1000, 100, 110);

    const thumb = container.querySelector<HTMLElement>(
      '[data-testid="transcript-outline-thumb"]',
    )!;
    expect(thumb.style.top).toBe("10%");
    expect(parseFloat(thumb.style.height)).toBeGreaterThanOrEqual(1);
  });

  it("renders nothing without markers", () => {
    const { container } = render(
      <TranscriptOutline
        sessionId={SID}
        markers={[]}
        total={0}
        firstIndex={0}
        lastIndex={0}
      />,
    );

    expect(
      container.querySelector('[data-testid="transcript-outline"]'),
    ).toBeNull();
  });

  // #1283: the strip alone gave nothing to aim with — a 2px mark says a turn
  // exists and nothing about which one.
  it("lists nearby turns with a snippet each on hover", () => {
    const { container } = renderOutline(400);

    expect(flyout(container)).toBeNull();
    hoverAt(container, 0.5);

    const rows = flyoutRows(container);
    expect(rows).toHaveLength(7);
    // Centred on the hovered turn: the middle row is the mark under the
    // pointer, and the numbers run in transcript order around it.
    const numbers = rows.map((r) =>
      Number(r.querySelector("span")!.textContent),
    );
    expect(numbers).toEqual([...numbers].sort((a, b) => a - b));
    expect(numbers[3]).toBeGreaterThan(150);
    expect(numbers[3]).toBeLessThan(250);
    // The snippet, not a bare index — this is what makes the flyout worth
    // opening (mutation: drop `preview` from the marker model).
    expect(rows[3].textContent).toContain("question");
  });

  it("jumps through the bus when a flyout row is clicked", () => {
    const revealed: string[] = [];
    useTranscriptScroll.getState().register(SID, (id) => {
      revealed.push(id);
      return true;
    });
    const { container, markers } = renderOutline(400);
    hoverAt(container, 0.5);

    const row = flyoutRows(container)[3];
    const index = Number(row.querySelector("span")!.textContent) - 1;
    act(() => {
      row.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });

    // Same contract as a marker click: by identity, into this session only
    // (mutation: neuter `reveal` and this goes red).
    expect(revealed).toEqual([
      markers.find((m) => m.index === index)!.messageId,
    ]);
  });

  it("closes the flyout when the pointer leaves", () => {
    const { container } = renderOutline(400);
    hoverAt(container, 0.5);
    expect(flyout(container)).not.toBeNull();

    act(() => {
      fireEvent.pointerLeave(
        container.querySelector('[data-testid="transcript-outline-hover"]')!,
      );
    });

    expect(flyout(container)).toBeNull();
  });

  it("shows where the viewport is while it moves", () => {
    const { container } = renderOutline(1218, 741, 748);

    const counter = container.querySelector(
      '[data-testid="transcript-outline-counter"]',
    );
    expect(counter?.textContent).toBe("742/1218");

    // One counter on screen at a time: the flyout carries its own in its header.
    hoverAt(container, 0.5);
    expect(
      container.querySelector('[data-testid="transcript-outline-counter"]'),
    ).toBeNull();
  });

  // The first live pass (#1283) shot the flyout with `1/44` in its header above
  // rows 24-31: the header was reading the viewport while the rows listed the
  // hovered neighbourhood, and a reader lines those two up. #1283's target
  // pairs `742/1218` with a list that starts at 742, so the header follows the
  // pointer and only the floating pill follows the viewport.
  //
  // Mutation bar: point the header back at `firstIndex` and this goes red.
  //
  // The list stays *centred* on the hovered turn rather than starting at it —
  // context on both sides is what makes "what is around here" answerable — so
  // the number the header names is the highlighted row's, not the first row's.
  it("heads the flyout with the hovered turn, not the viewport's", () => {
    // The viewport is parked at the very top; the pointer is halfway down.
    const { container } = renderOutline(1218, 0, 7);
    hoverAt(container, 0.5);

    const header = flyout(container)!.firstElementChild!.textContent!;
    // The rows are centred on the hovered turn, so the one the header names is
    // the highlighted row, not the first — that is the pairing a reader checks.
    const highlighted = flyout(container)!.querySelector("button.bg-accent")!;
    const highlightedIndex = highlighted.querySelector("span")!.textContent!;

    expect(header).toBe(`${highlightedIndex}/1218`);
    expect(header).not.toBe("1/1218");
  });
});
