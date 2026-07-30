// @vitest-environment jsdom
//
// #1143 follow-up (found dogfooding #1155 on a 212MB db): the "jump to latest"
// arrow could not reach the bottom of a long session. It descended roughly a
// screenful per click and never arrived.
//
// Two compounding causes, both reproduced here:
//
//  1. `jumpToLatest` targeted `el.scrollHeight`, which is the virtualizer's
//     spacer — measured heights for mounted rows plus `ROW_ESTIMATE_PX` (140)
//     for every row that never mounted. Real rows exceed 140, so the target is
//     an under-estimate; scrolling toward it mounts rows, they measure taller,
//     the total grows, and the bottom moves further down. The error scales with
//     the rows below the current position, so it is not an off-by-a-little.
//  2. The intermediate `scroll` events flip `pinnedToBottom.current` to false
//     via `handleScroll`, so the ResizeObserver settle that re-pins every other
//     scroll path is gated off for this one and it stays where it landed.
//
// The harness reproduces (1) by deriving `scrollHeight` from the spacer's live
// inline height rather than a constant — so the bottom genuinely moves as rows
// measure, exactly as it does in the app. Rows measure 1000px here (virtual-core
// falls back to `offsetHeight`, which `vitest.setup.ts` stubs) against the 140px
// estimate, which is the same direction of error as production.
//
// Mutation bar (the reviewer's): put `el.scrollTo({ top: el.scrollHeight })`
// back and the test below goes red — it lands 1000px short of where it must be,
// i.e. a full viewport, which is the "descends a screenful per click" symptom.

import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { ChatView } from "@/components/chat-view";
import { useChatStore } from "@/store/chat";
import type { Message } from "@/bindings";

const SID = "s1";
// Many viewports tall: 400 rows at a measured 1000px each is ~400 screens, so a
// single under-estimated jump cannot accidentally arrive.
const TOTAL = 400;
const LAST_ID = `m${TOTAL - 1}`;
const VIEWPORT_H = 1000; // matches the stub in vitest.setup.ts

// Frames are queued rather than run synchronously: `scrollToIndex` schedules a
// rAF reconcile loop that reschedules itself until the target offset stops
// moving, so a synchronous rAF would recurse into it instead of stepping it.
let frames: FrameRequestCallback[] = [];
(globalThis as { requestAnimationFrame?: unknown }).requestAnimationFrame = (
  cb: FrameRequestCallback,
) => {
  frames.push(cb);
  return frames.length;
};
(globalThis as { cancelAnimationFrame?: unknown }).cancelAnimationFrame =
  () => {};

/** Run queued frames until the queue drains or the budget runs out. The budget
 *  is generous because the reconcile loop needs one pass per correction. */
async function flushFrames(budget = 60) {
  for (let i = 0; i < budget; i++) {
    const queued = frames;
    frames = [];
    if (queued.length === 0) return;
    await act(async () => {
      for (const cb of queued) cb(i);
    });
  }
}

function seed() {
  const messages: Message[] = [];
  for (let i = 0; i < TOTAL; i++) {
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

/**
 * Give the scroller real behaviour, with `scrollHeight` read from the spacer's
 * live height. That is the crux of this suite: a constant `scrollHeight` would
 * make the naive implementation look correct, because the bug is precisely that
 * the bottom moves as rows measure.
 */
function makeScrollable(el: HTMLElement) {
  let top = 0;
  const spacerHeight = () => {
    const spacer = el.querySelector<HTMLElement>("[style*='height']");
    return spacer ? parseFloat(spacer.style.height) || 0 : 0;
  };
  Object.defineProperty(el, "scrollHeight", {
    configurable: true,
    get: spacerHeight,
  });
  Object.defineProperty(el, "clientHeight", {
    configurable: true,
    get: () => VIEWPORT_H,
  });
  Object.defineProperty(el, "scrollTop", {
    configurable: true,
    get: () => top,
    set: (v: number) => {
      top = Math.max(0, Math.min(v, Math.max(0, spacerHeight() - VIEWPORT_H)));
      el.dispatchEvent(new Event("scroll"));
    },
  });
  // `behavior: "smooth"` is stepped rather than instant, because the steps are
  // load-bearing for this bug: a real smooth scroll fires `scroll` at every
  // intermediate offset, and `handleScroll` reads each one as "not at the
  // bottom" and detaches the pin. An instant stub would hide cause (2) entirely
  // and let the naive implementation look like it arrives.
  (el as unknown as { scrollTo: (o: ScrollToOptions) => void }).scrollTo = (
    o,
  ) => {
    if (typeof o?.top !== "number") return;
    const from = top;
    const to = o.top;
    if (o.behavior === "smooth") {
      for (let step = 1; step < 4; step++) {
        el.scrollTop = from + ((to - from) * step) / 4;
      }
    }
    el.scrollTop = to;
  };
}

async function renderScrolledToTop() {
  const view = render(<ChatView sessionId={SID} />);
  const scrollEl = view.container.querySelector(
    '[data-testid="chat-scroll"]',
  ) as HTMLDivElement;
  makeScrollable(scrollEl);
  await flushFrames();
  // Park at the top and let the scroll handler detach the pin, which is what
  // puts the arrow on screen in the first place.
  await act(async () => {
    scrollEl.scrollTop = 0;
  });
  await flushFrames();
  return { ...view, scrollEl };
}

const arrow = (c: HTMLElement) =>
  c.querySelector<HTMLButtonElement>('[aria-label="Jump to latest"]');
const mounted = (c: HTMLElement, id: string) =>
  c.querySelector(`[data-message-id="${id}"]`) !== null;

beforeEach(() => {
  frames = [];
  seed();
});
afterEach(() => {
  useChatStore.setState({ messagesBySession: {}, toolStepsByMessage: {} });
});

// What jsdom can and cannot pin, stated up front so the next person doesn't
// mistake the scope of this suite:
//
// It CAN pin the mechanism — that the jump aims past the current, under-stated
// `scrollHeight` and converges as rows measure. That is the single test below,
// and it dies if the `scrollHeight` write comes back.
//
// A second test ("a second click has nowhere to go") was written and then
// deleted: it passed with the fix reverted, because without layout the spacer
// doesn't grow between clicks here. A test that survives its own mutation is
// worse than no test — it reads as coverage.
//
// It CANNOT pin the user-visible symptom Tony wrote as the acceptance criterion
// ("the arrow disappears on one click"). That depends on a real smooth-scroll
// animation interleaving `scroll` events with layout and React commits over many
// frames; jsdom has no layout, so the pin resolves the same way here whichever
// implementation runs. Asserting on the arrow would look like a guard and be
// none — the arrow check is the real-app pass in `pnpm tauri dev`.
describe("jump to latest, with the transcript windowed (#1143)", () => {
  it("aims past the under-stated scrollHeight, not at it", async () => {
    const { container, scrollEl } = await renderScrolledToTop();

    // Preconditions, or this proves nothing: we are scrolled up, the arrow is
    // showing, and the newest row is nowhere near the DOM.
    expect(arrow(container)).not.toBeNull();
    expect(mounted(container, LAST_ID)).toBe(false);
    // The target the old implementation used: the spacer as currently measured,
    // which counts 140px for every row that has never mounted.
    const naiveTarget = scrollEl.scrollHeight;

    await act(async () => {
      arrow(container)?.click();
    });
    await flushFrames();

    expect(mounted(container, LAST_ID)).toBe(true);
    // The discriminating assertion. Aiming at `naiveTarget` would have stopped
    // at least a full viewport short of where the tail actually is — which is
    // the "descends a screenful per click" symptom, in numbers.
    expect(scrollEl.scrollTop - naiveTarget).toBeGreaterThanOrEqual(VIEWPORT_H);
  }, 20_000);
});
