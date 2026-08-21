// @vitest-environment jsdom
//
// The navigator itself (#1290): when it appears, what it counts, and what the
// keyboard does inside it. The end-to-end jump — pick a row, that message is on
// screen — is in `chat-view.navigator.test.tsx`, because only ChatView owns the
// virtualizer that mounts the row.
//
// Two things about this suite are deliberate:
//
//  - The jump is asserted through the *real* reveal bus rather than a spy on
//    the component. Registering a revealer and checking which id it receives is
//    the same contract `find-bar.tsx` depends on, so a change that quietly
//    stops going through the bus fails here.
//  - The popup is portalled to `document.body` by radix, i.e. *outside* RTL's
//    `container`. Query it through `screen` / `document`, never
//    `container.querySelector` — the latter finds nothing and makes a
//    `toBeNull()` assertion pass for entirely the wrong reason.

import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { MessageNavigator } from "@/components/message-navigator";
import { buildOutline } from "@/lib/transcript-outline";
import { useTranscriptScroll } from "@/store/transcript-scroll";
import { useMessageNavigator } from "@/store/message-navigator";
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

/**
 * One group per message, but the *ordinals are not the group indices* — every
 * case in this suite runs with a transcript whose message count exceeds its
 * group count, so anything that quietly counts groups shows up as a wrong
 * number rather than as a coincidence that still passes.
 */
const TOOLS_PER_TURN = 2;
const ordinalsFor = (count: number) =>
  Array.from({ length: count }, (_, i) => (i + 1) * TOOLS_PER_TURN);
const totalFor = (count: number) => count * TOOLS_PER_TURN;

function renderNav({
  count = 40,
  lowestIndex = 0,
  scrolling = true,
  atBottom = false,
}: {
  count?: number;
  lowestIndex?: number;
  scrolling?: boolean;
  atBottom?: boolean;
} = {}) {
  const markers = buildOutline(groups(count));
  const view = render(
    <MessageNavigator
      sessionId={SID}
      markers={markers}
      ordinals={ordinalsFor(count)}
      totalMessages={totalFor(count)}
      lowestIndex={lowestIndex}
      scrolling={scrolling}
      atBottom={atBottom}
    />,
  );
  return { ...view, markers };
}

const pill = () =>
  document.querySelector<HTMLButtonElement>(
    '[data-testid="message-navigator-pill"]',
  );
const popup = () =>
  document.querySelector<HTMLElement>(
    '[data-testid="message-navigator-popup"]',
  );
const rows = () =>
  Array.from(
    document.querySelectorAll<HTMLElement>(
      '[data-testid="message-navigator-row"]',
    ),
  );

function openPopup() {
  fireEvent.click(pill()!);
}

function revealed(): string[] {
  const seen: string[] = [];
  useTranscriptScroll.getState().register(SID, (id) => {
    seen.push(id);
    return true;
  });
  return seen;
}

beforeEach(() => {
  useMessageNavigator.setState({ openSessions: new Set() });
});
afterEach(() => {
  // Not automatic: this project runs vitest without `globals`, so RTL never
  // registers its own cleanup. Without this the previous test's pill is still
  // in the document and `document.querySelector` hands back that one.
  cleanup();
  useTranscriptScroll.setState({ revealers: {} });
  useMessageNavigator.setState({ openSessions: new Set() });
  vi.useRealTimers();
});

describe("MessageNavigator visibility (#1290)", () => {
  it("shows nothing at rest, however far back the viewport is", () => {
    renderNav({ lowestIndex: 0, scrolling: false });

    expect(pill()).toBeNull();
  });

  it("shows nothing while scrolling inside the last few messages", () => {
    // Group 37 of 40 ends at message 76 of 80 — 4 back, inside
    // NAVIGATOR_MIN_SCROLLBACK (5). One row further up clears it, below.
    renderNav({ count: 40, lowestIndex: 37 });

    expect(pill()).toBeNull();
  });

  it("fades the pill in once the scrollback passes the threshold", () => {
    // Group 36 ends at message 74 of 80 — 6 back, one past the threshold.
    renderNav({ count: 40, lowestIndex: 36 });

    expect(pill()).not.toBeNull();
    expect(pill()!.className).toContain("opacity-100");
  });

  it("stays hidden while pinned at the tail even if a scroll event lands", () => {
    renderNav({ count: 40, lowestIndex: 0, atBottom: true });

    expect(pill()).toBeNull();
  });

  it("renders nothing without markers", () => {
    render(
      <MessageNavigator
        sessionId={SID}
        markers={[]}
        ordinals={[]}
        totalMessages={0}
        lowestIndex={0}
        scrolling
        atBottom={false}
      />,
    );

    expect(pill()).toBeNull();
  });

  it("counts raw messages, not markers or groups", () => {
    // 40 groups, 40 markers, 80 messages. A counter reading 1/40 or 20/40 is
    // the bug this case exists for.
    renderNav({ count: 40, lowestIndex: 20 });

    expect(pill()!.textContent).toContain("42/80");
  });
});

describe("MessageNavigator popup (#1290)", () => {
  it("lists the markers with a message ordinal and a snippet each", () => {
    renderNav({ count: 40, lowestIndex: 0 });
    openPopup();

    expect(popup()).not.toBeNull();
    const first = rows()[0];
    // Ordinal, not group index — group 0 is message 2 in this fixture.
    expect(first.textContent).toContain("2");
    expect(first.textContent).toContain("question 0");
  });

  it("reveals the clicked row's message through the bus", () => {
    const seen = revealed();
    const { markers } = renderNav({ count: 40, lowestIndex: 0 });
    openPopup();

    fireEvent.click(rows()[7]);

    expect(seen).toEqual([markers[7].messageId]);
  });

  it("does not reveal into another pane's session", () => {
    const other: string[] = [];
    useTranscriptScroll.getState().register("s2", (id) => {
      other.push(id);
      return true;
    });
    renderNav({ count: 40, lowestIndex: 0 });
    openPopup();

    fireEvent.click(rows()[3]);

    expect(other).toEqual([]);
  });

  it("survives a click on a row whose message is gone", () => {
    renderNav({ count: 40, lowestIndex: 0 });
    openPopup();

    // No revealer registered at all — an edit or truncate can retire the
    // session's transcript between building the markers and clicking one.
    expect(() => fireEvent.click(rows()[2])).not.toThrow();
  });

  it("keeps the node count bounded on a session with thousands of turns", () => {
    renderNav({ count: 9000, lowestIndex: 0 });
    openPopup();

    expect(rows().length).toBeGreaterThan(0);
    expect(rows().length).toBeLessThanOrEqual(200);
  });

  it("keeps the pill in place while the popup is open", () => {
    vi.useFakeTimers();
    const { rerender, markers } = renderNav({ count: 40, lowestIndex: 0 });
    openPopup();

    // Scrolling stops, and stays stopped well past the fade window.
    act(() => {
      rerender(
        <MessageNavigator
          sessionId={SID}
          markers={markers}
          ordinals={ordinalsFor(40)}
          totalMessages={totalFor(40)}
          lowestIndex={0}
          scrolling={false}
          atBottom={false}
        />,
      );
    });
    act(() => {
      vi.advanceTimersByTime(5000);
    });

    expect(popup()).not.toBeNull();
    expect(pill()).not.toBeNull();
    expect(pill()!.className).toContain("opacity-100");
  });

  it("closes on Escape and lets the pill fade again", () => {
    renderNav({ count: 40, lowestIndex: 0 });
    openPopup();
    expect(popup()).not.toBeNull();

    fireEvent.keyDown(popup()!, { key: "Escape" });

    expect(popup()).toBeNull();
    expect(useMessageNavigator.getState().openSessions.has(SID)).toBe(false);
  });

  it("closes after a row click", () => {
    revealed();
    renderNav({ count: 40, lowestIndex: 0 });
    openPopup();

    fireEvent.click(rows()[4]);

    expect(popup()).toBeNull();
  });
});

describe("MessageNavigator keyboard (#1290)", () => {
  it("opens from the store even with no scrollback at all", () => {
    // The ⌘⇧O path: the shortcut writes the store directly, and a keyboard user
    // who asked for the list by name gets it even sitting at the tail.
    renderNav({ count: 40, lowestIndex: 39, scrolling: false, atBottom: true });
    expect(pill()).toBeNull();

    act(() => {
      useMessageNavigator.getState().openNavigator(SID);
    });

    expect(pill()).not.toBeNull();
    expect(popup()).not.toBeNull();
  });

  it("moves with the arrow keys and jumps with Enter", () => {
    const seen = revealed();
    const { markers } = renderNav({ count: 40, lowestIndex: 0 });
    openPopup();

    fireEvent.keyDown(popup()!, { key: "ArrowDown" });
    fireEvent.keyDown(popup()!, { key: "ArrowDown" });
    fireEvent.keyDown(popup()!, { key: "Enter" });

    // Third row, not the first: an implementation that ignores the arrows and
    // jumps to the selection's seed still passes a "does Enter reveal" check.
    expect(seen).toEqual([markers[2].messageId]);
  });

  it("clamps at the top of the list rather than wrapping to the end", () => {
    const seen = revealed();
    const { markers } = renderNav({ count: 40, lowestIndex: 0 });
    openPopup();

    fireEvent.keyDown(popup()!, { key: "ArrowUp" });
    fireEvent.keyDown(popup()!, { key: "ArrowUp" });
    fireEvent.keyDown(popup()!, { key: "Enter" });

    expect(seen).toEqual([markers[0].messageId]);
  });

  it("starts the selection at the reader's position, not at the top", () => {
    // Lowest visible group is 20, i.e. message 42 — the seeded row is the last
    // one at or before it.
    renderNav({ count: 40, lowestIndex: 20 });
    openPopup();

    const selected = rows().findIndex(
      (r) => r.getAttribute("aria-selected") === "true",
    );
    expect(selected).toBe(20);
  });
});
