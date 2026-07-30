// @vitest-environment jsdom
//
// #1143 review defect: with the transcript windowed, find counted from the full
// data model but navigated through the DOM. `collectOccurrences` can only see
// mounted rows, so the counter read "1 of 500" while Enter reached 7 of them —
// and the old two-frame re-collect couldn't rescue it, because
// `Math.min(next, fresh.length - 1)` clamped the cursor to the last *visible*
// hit, so stepping parked there instead of failing loudly.
//
// The premise ("the store keeps every message") was tested; the wiring was not.
// This file tests the wiring: with the flag ON and every hit placed near the top
// of a transcript many viewports tall, the count must match the flag-off count
// and stepping must actually reach a hit that started outside the window.
//
// Mutation bar: neuter the reveal (drop the `register` call in chat-view, or make
// `reveal` a no-op) and `reaches a hit far above the initial viewport` goes red.

import { act, render } from "@testing-library/react";
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";

import { ChatView } from "@/components/chat-view";
import { FindBar } from "@/components/find-bar";
import { ipc } from "@/lib/ipc";
import { collectOccurrences } from "@/lib/find-highlight";
import { useChatStore } from "@/store/chat";
import {
  EXPERIMENTAL_DEFAULTS,
  useExperimentalStore,
} from "@/store/experimental";
import { useFindStore } from "@/store/find";
import { useFindExpansion } from "@/store/find-expansion";
import { useTranscriptScroll } from "@/store/transcript-scroll";
import type { Message } from "@/bindings";
import { useRef } from "react";

const SID = "s1";
const TOTAL = 120;
// Matches live in the first few messages — ~17 viewports above where a freshly
// opened session sits, since ChatView pins to the tail on mount. 120 rows keeps
// the flag-off control (which mounts all of them) affordable under a parallel
// run while still being far more than one window.
const MATCHING_IDS = ["m0", "m1", "m2", "m3", "m4"];
const NEEDLE = "needle";
const VIEWPORT = { width: 800, height: 1000 };
const ROW_PX = 140; // ROW_ESTIMATE_PX in chat-view.tsx

// The virtualizer reads its viewport from offsetWidth/offsetHeight, which jsdom
// reports as 0 — a zero-height viewport windows to no rows and would make this
// suite pass vacuously.
beforeAll(() => {
  for (const [prop, value] of [
    ["offsetWidth", VIEWPORT.width],
    ["offsetHeight", VIEWPORT.height],
  ] as const) {
    Object.defineProperty(HTMLElement.prototype, prop, {
      configurable: true,
      get: () => value,
    });
  }
});
afterAll(() => {
  for (const prop of ["offsetWidth", "offsetHeight"]) {
    delete (HTMLElement.prototype as unknown as Record<string, unknown>)[prop];
  }
});

// Frames run synchronously so a step's await chain settles inside one act().
(globalThis as { requestAnimationFrame?: unknown }).requestAnimationFrame = (
  cb: FrameRequestCallback,
) => {
  cb(0);
  return 0;
};
(globalThis as { cancelAnimationFrame?: unknown }).cancelAnimationFrame =
  () => {};

/** Real-timer wait. Fake timers are deliberately NOT used in this file: this
 *  suite renders React inside `act()`, and faking the microtask/timer queue that
 *  React flushes through left the container empty under parallel runs. The only
 *  thing that needs waiting is the 150ms search debounce, so wait it for real. */
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/**
 * Make `el` behave like a real scroller. jsdom has no layout and its
 * `Element.scrollTo` is inert, so without this the virtualizer's
 * `scrollToIndex` would write nowhere and never re-window — the test would be
 * measuring the harness, not the fix. Writing `scrollTop` dispatches `scroll`,
 * which is how the virtualizer learns its offset in a browser too.
 */
function makeScrollable(el: HTMLElement) {
  let top = 0;
  const scrollHeight = TOTAL * ROW_PX;
  Object.defineProperty(el, "scrollHeight", {
    configurable: true,
    get: () => scrollHeight,
  });
  Object.defineProperty(el, "clientHeight", {
    configurable: true,
    get: () => VIEWPORT.height,
  });
  Object.defineProperty(el, "scrollTop", {
    configurable: true,
    get: () => top,
    set: (v: number) => {
      top = Math.max(0, Math.min(v, scrollHeight - VIEWPORT.height));
      el.dispatchEvent(new Event("scroll"));
    },
  });
  (el as unknown as { scrollTo: (o: ScrollToOptions) => void }).scrollTo = (
    o,
  ) => {
    if (typeof o?.top === "number") el.scrollTop = o.top;
  };
  // Rows are absolutely positioned inside the spacer; `scrollIntoView` is a
  // jsdom no-op, and the virtualizer already did the real positioning.
  Element.prototype.scrollIntoView = () => {};
}

function seed() {
  const messages: Message[] = [];
  for (let i = 0; i < TOTAL; i++) {
    messages.push({
      id: `m${i}`,
      sessionId: SID,
      role: i % 2 === 0 ? "user" : "assistant",
      content: MATCHING_IDS.includes(`m${i}`)
        ? `this one has a ${NEEDLE} in it`
        : `message ${i}`,
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

function setVirtualized(on: boolean) {
  useExperimentalStore.setState({
    flags: { ...EXPERIMENTAL_DEFAULTS, virtualizedTranscript: on },
  });
}

/** Mirrors session-pane.tsx: the find bar mounts beside the transcript only while
 *  find is open, and `rootRef` is the wrapper the DOM walk searches. */
function Pane() {
  const rootRef = useRef<HTMLDivElement>(null);
  const findOpen = useFindStore((s) => s.open && s.sessionId === SID);
  return (
    <div ref={rootRef}>
      {findOpen && <FindBar sessionId={SID} rootRef={rootRef} />}
      <ChatView sessionId={SID} />
    </div>
  );
}

/**
 * Render the transcript, give it real scroller behaviour, and park it at the
 * tail — where a freshly opened session actually sits.
 *
 * The parking matters: jsdom reports `scrollHeight: 0` until `makeScrollable`
 * runs, so ChatView's mount-time pin writes 0 and the initial window sits at the
 * *top* of the transcript, where the seeded matches already are. A test that
 * opened find at that point would pass with the reveal deleted — which is exactly
 * what happened on the first draft of this file.
 */
async function renderParkedAtTail() {
  const view = render(<Pane />);
  const scrollEl = view.container.querySelector(
    '[data-testid="chat-scroll"]',
  ) as HTMLDivElement;
  if (!scrollEl)
    throw new Error(
      `no chat-scroll; container=${view.container.innerHTML.slice(0, 300)}`,
    );
  makeScrollable(scrollEl);
  await act(async () => {
    scrollEl.scrollTop = scrollEl.scrollHeight;
    await sleep(10);
  });
  return view;
}

/** Open find (mounting the bar) and let the debounce + reveal chain settle. */
async function openFindAndSettle() {
  await act(async () => {
    useFindStore.setState({ open: true, sessionId: SID, seedQuery: NEEDLE });
    await sleep(250);
  });
}

const counter = (c: HTMLElement) =>
  c.querySelector('[data-testid="find-counter"]')?.textContent ?? "";
const mounted = (c: HTMLElement, id: string) =>
  c.querySelector(`[data-message-id="${id}"]`) !== null;

beforeEach(() => {
  seed();
  // Find starts CLOSED: each test parks the transcript at the tail first, then
  // opens the bar, so the reveal has somewhere to travel from.
  useFindStore.setState({ open: false, sessionId: null, seedQuery: null });
  vi.spyOn(ipc, "searchInSession").mockResolvedValue(
    MATCHING_IDS.map((messageId) => ({
      messageId,
      sessionId: SID,
      snippet: NEEDLE,
      createdAt: 1,
      role: "user",
    })) as unknown as Awaited<ReturnType<typeof ipc.searchInSession>>,
  );
});

afterEach(() => {
  vi.restoreAllMocks();
  setVirtualized(false);
  useChatStore.setState({ messagesBySession: {}, toolStepsByMessage: {} });
  useFindStore.setState({ open: false, sessionId: null, seedQuery: null });
  useFindExpansion.getState().clear();
  useTranscriptScroll.setState({ revealers: {} });
});

describe("find with the transcript windowed (#1143)", () => {
  it("counts every hit, the same as with the flag off", async () => {
    setVirtualized(false);
    const off = await renderParkedAtTail();
    await openFindAndSettle();
    const offCount = counter(off.container);
    off.unmount();
    useFindStore.setState({ open: false, sessionId: null });

    setVirtualized(true);
    const on = await renderParkedAtTail();
    await openFindAndSettle();

    // Not a hardcoded number: the windowed count must equal the un-windowed
    // count, which is the property that actually matters.
    expect(counter(on.container)).toBe(offCount);
    expect(counter(on.container)).toBe(`1 of ${MATCHING_IDS.length}`);
  }, 20_000);

  it("reaches a hit far above the initial viewport", async () => {
    setVirtualized(true);
    const { container } = await renderParkedAtTail();

    // Preconditions, or this test proves nothing: the transcript is windowed,
    // and the first match is NOT currently mounted.
    expect(container.querySelectorAll("[data-message-id]").length).toBeLessThan(
      TOTAL,
    );
    expect(mounted(container, "m0")).toBe(false);

    await openFindAndSettle();

    // The fix: activating occurrence 1 (message m0, ~300 rows above the parked
    // tail) mounts that row and yields a paintable range for it. Without the
    // reveal the row stays unmounted and there is nothing to range over.
    expect(mounted(container, "m0")).toBe(true);
    const ranges = collectOccurrences(
      container as HTMLElement,
      new Set(["m0"]),
      NEEDLE,
    );
    expect(ranges.length).toBeGreaterThan(0);
  }, 20_000);

  it("reaches every hit as it steps, not just the ones on screen", async () => {
    setVirtualized(true);
    const { container } = await renderParkedAtTail();
    await openFindAndSettle();

    const reached: string[] = [];
    for (let i = 0; i < MATCHING_IDS.length; i++) {
      const expectedId = MATCHING_IDS[i];
      if (mounted(container, expectedId)) reached.push(expectedId);
      // Advance to the next occurrence (Enter).
      await act(async () => {
        container
          .querySelector("input")
          ?.dispatchEvent(
            new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
          );
        await sleep(20);
      });
    }

    // The reachable set equals the full match set — the assertion the review's
    // probe (500 hits, 7 reachable) was measuring by hand.
    expect(reached).toEqual(MATCHING_IDS);
  }, 20_000);
});
