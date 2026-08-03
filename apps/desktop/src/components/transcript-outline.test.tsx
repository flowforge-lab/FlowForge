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

import { act, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { TranscriptOutline } from "@/components/transcript-outline";
import { buildOutline } from "@/lib/transcript-outline";
import { useTranscriptScroll } from "@/store/transcript-scroll";
import type { RenderGroup } from "@/lib/turn-groups";

const SID = "s1";

function groups(n: number): RenderGroup[] {
  return Array.from({ length: n }, (_, i) =>
    i % 2 === 0
      ? ({ kind: "user", message: { id: `m${i}` } } as unknown as RenderGroup)
      : ({
          kind: "assistant",
          message: { id: `m${i}` },
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
});
