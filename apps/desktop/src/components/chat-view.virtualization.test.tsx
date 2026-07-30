// @vitest-environment jsdom
//
// #1143: opening a long session cost ~2.2s, and ~2153ms of that was React
// mounting one DOM node per message — the load path either side of it summed to
// ~43ms. The fix windows the DOM so mount cost follows viewport size instead of
// history length.
//
// A correctness test passes just as happily with virtualization removed, so the
// invariant is pinned directly here: the number of mounted message rows must
// stay bounded by the viewport regardless of how long the session is. This is
// the test that fails the moment someone deletes the virtualizer.
//
// jsdom has no layout, so rows never measure and the virtualizer works purely
// from the viewport `vitest.setup.ts` stubs (800×1000 — jsdom would otherwise
// report 0×0 and window down to no rows at all) and the estimated row height.
// That's deterministic, which is exactly what makes the bound assertable without
// timing.

import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { ChatView } from "@/components/chat-view";
import { useChatStore } from "@/store/chat";
import type { Message } from "@/bindings";

const SID = "s1";

function seed(count: number) {
  const messages: Message[] = [];
  for (let i = 0; i < count; i++) {
    messages.push({
      id: `m${i}`,
      sessionId: SID,
      role: i % 2 === 0 ? "user" : "assistant",
      content: `message ${i}`,
      createdAt: i + 1,
    } as Message);
  }
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: { [SID]: messages },
    streamingBySession: {},
    turnStartBySession: {},
    turnStartByMessage: {},
    toolStepsByMessage: {},
    reasoningByMessage: {},
  });
}

// --- autoscroll harness, mirroring chat-view.autoscroll.test.tsx -------------
// The virtualizer mounts ResizeObservers of its own (viewport rect + per-row
// measurement), and firing those with the empty entry list this stub passes
// would exercise library internals rather than the pin. Each observer records
// what it observes so a test can fire only ChatView's own content observer —
// the one whose post-layout settle drives `shouldPinToTail`.
let observers: ResizeObserverStub[] = [];
class ResizeObserverStub {
  cb: ResizeObserverCallback;
  targets: Element[] = [];
  live = true;
  constructor(cb: ResizeObserverCallback) {
    this.cb = cb;
    observers.push(this);
  }
  observe(el: Element) {
    this.targets.push(el);
  }
  unobserve() {}
  disconnect() {
    this.live = false;
  }
}
(globalThis as { ResizeObserver?: unknown }).ResizeObserver =
  ResizeObserverStub;

// The pin coalesces through requestAnimationFrame; run it synchronously so the
// scrollTop write lands inside the same act().
(globalThis as { requestAnimationFrame?: unknown }).requestAnimationFrame = (
  cb: FrameRequestCallback,
) => {
  cb(0);
  return 0;
};
(globalThis as { cancelAnimationFrame?: unknown }).cancelAnimationFrame =
  () => {};

function mockGeometry(el: HTMLElement, scrollHeight: number, client: number) {
  let top = 0;
  Object.defineProperty(el, "scrollHeight", {
    configurable: true,
    get: () => scrollHeight,
  });
  Object.defineProperty(el, "clientHeight", {
    configurable: true,
    get: () => client,
  });
  Object.defineProperty(el, "scrollTop", {
    configurable: true,
    get: () => top,
    set: (v: number) => {
      top = v;
    },
  });
}

/** Fire only the observer watching `el` (ChatView's content observer). */
function fireObserverFor(el: Element) {
  act(() => {
    for (const o of observers) {
      if (o.live && o.targets.includes(el))
        o.cb([], o as unknown as ResizeObserver);
    }
  });
}

function renderRows(count: number): number {
  seed(count);
  const { container } = render(<ChatView />);
  return container.querySelectorAll("[data-message-id]").length;
}

beforeEach(() => {
  observers = [];
});
afterEach(() => {
  useChatStore.setState({ messagesBySession: {}, toolStepsByMessage: {} });
});

describe("ChatView virtualization (#1143)", () => {
  it("mounts a viewport-bounded number of rows, not one per message", () => {
    const short = renderRows(50);
    const long = renderRows(2000);

    // The bound itself: a 2000-message session must not put 2000 rows in the
    // DOM. 40x the seeded viewport would still be a regression worth failing.
    expect(long).toBeLessThan(100);
    // And the count must not scale with history at all — 40x the messages
    // renders the same window, give or take the overscan edges.
    expect(long).toBe(short);
    // Sanity: the window is actually populated, so a virtualizer that rendered
    // *nothing* (the jsdom zero-rect failure mode) can't pass this test.
    expect(long).toBeGreaterThan(0);
  });

  // The scroll machinery (#206 pin-to-bottom, #866 relaunch restore, #1025
  // post-layout settle) is the risky part of this change: it all funnels through
  // `shouldPinToTail`, which reads container geometry rather than row nodes, so
  // it should survive windowing untouched. "Should" isn't good enough on this
  // code — assert it on the virtual path too.
  describe("streaming autoscroll on the virtual path", () => {
    function renderLong() {
      seed(2000);
      const { container } = render(<ChatView />);
      const scrollEl = container.querySelector(
        '[data-testid="chat-scroll"]',
      ) as HTMLDivElement;
      const contentEl = scrollEl.firstElementChild as HTMLElement;
      mockGeometry(scrollEl, 100_000, 1000);
      scrollEl.scrollTop = 0;
      return { scrollEl, contentEl };
    }

    it("still pins to the post-layout bottom in a 2000-message session", () => {
      const { scrollEl, contentEl } = renderLong();

      fireObserverFor(contentEl);

      expect(scrollEl.scrollTop).toBe(100_000);
    });

    it("does not yank the viewport when a stream lands while scrolled up", () => {
      const { scrollEl, contentEl } = renderLong();

      // Scroll up: 100000 - 0 - 1000 is far past the 40px pin threshold, so the
      // scroll handler detaches autoscroll.
      act(() => {
        scrollEl.dispatchEvent(new Event("scroll"));
      });
      scrollEl.scrollTop = 0;

      fireObserverFor(contentEl);

      expect(scrollEl.scrollTop).toBe(0);
    });
  });

  it("windows the transcript without dropping it from the store", () => {
    seed(500);
    render(<ChatView />);
    // Only the DOM is windowed (#1143) — the full session stays in the store.
    //
    // This asserts the *premise* and nothing more. An earlier version of this
    // comment went on to conclude that find therefore "keeps working off
    // complete data", which was wrong and hid a real defect (#1143 review):
    // find's count comes from the data model but its navigation walks the DOM,
    // so a windowed transcript left most hits unreachable. Complete data in the
    // store does not imply a feature reads it — anything that needs a specific
    // row must ask for it to be mounted (`store/transcript-scroll.ts`), and that
    // wiring is tested in `find-bar.virtualized.test.tsx`.
    expect(useChatStore.getState().messagesBySession[SID]).toHaveLength(500);
  });
});
