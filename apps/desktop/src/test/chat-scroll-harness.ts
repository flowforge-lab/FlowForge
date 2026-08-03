// Shared jsdom harness for the transcript scroll suites (#206 / #866 / #1025 /
// #1143 / #1165). Five suites had grown their own near-identical copies of a
// ResizeObserver stub, a requestAnimationFrame stub and a geometry mock; they
// are collected here so the next scroll fix has one harness to reason about
// instead of five that differ in ways nobody intended.
//
// Not named `*.test.ts`, so `vitest.config.ts`'s `include` doesn't collect it.
//
// jsdom has no layout engine, no ResizeObserver and no `Element.prototype`
// scrolling, so all three have to be supplied by hand:
//
//  - `installResizeObserverStub` captures every observer the tree mounts and
//    lets a test drive them. The virtualizer mounts observers of its own (the
//    viewport rect, and one per measured row), so the stub records what each
//    one observes: a test that wants ChatView's *content* observer — the one
//    whose post-layout settle drives `shouldPinToTail` — can fire only that.
//  - `installSyncRaf` runs frames synchronously, so a pin's `scrollTop` write
//    lands inside the `act()` that triggered it. See the depth guard below:
//    synchronous frames and virtual-core's self-rescheduling reconcile loop are
//    a stack overflow if combined naively.
//  - `installQueuedRaf` queues frames instead, for suites that need to *step*
//    that reconcile loop rather than collapse it.
//  - `mockGeometry` / `makeScrollable` fake the container. `makeScrollable` is
//    the one to reach for when the test is about the virtualizer: it derives
//    `scrollHeight` from the spacer's live inline height, so the bottom moves
//    as rows measure, which is the whole shape of the #1143 family of bugs.

import { act } from "@testing-library/react";

/** Matches the viewport `vitest.setup.ts` stubs via `offsetHeight`. */
export const VIEWPORT_H = 1000;

// --- ResizeObserver ---------------------------------------------------------

export class ResizeObserverStub {
  cb: ResizeObserverCallback;
  targets: Element[] = [];
  live = true;
  constructor(cb: ResizeObserverCallback) {
    this.cb = cb;
    installedObservers.push(this);
  }
  observe(el: Element) {
    this.targets.push(el);
  }
  unobserve() {}
  /** Marks the instance dead so a re-created observer (the effect re-runs when
   *  `findOn` flips) doesn't leave a stale closure firing — matching the real
   *  observer, whose callback stops after disconnect. */
  disconnect() {
    this.live = false;
  }
}

let installedObservers: ResizeObserverStub[] = [];

export interface ResizeObserverHarness {
  /** Every observer mounted since the last `reset()`, in construction order. */
  observers: () => ResizeObserverStub[];
  /** Fire live observers. With `el`, only those observing it. */
  fire: (el?: Element) => void;
  reset: () => void;
}

export function installResizeObserverStub(): ResizeObserverHarness {
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver =
    ResizeObserverStub;
  return {
    observers: () => installedObservers,
    fire: (el?: Element) => {
      act(() => {
        for (const o of installedObservers) {
          if (!o.live) continue;
          if (el && !o.targets.includes(el)) continue;
          o.cb([], o as unknown as ResizeObserver);
        }
      });
    },
    reset: () => {
      installedObservers = [];
    },
  };
}

// --- requestAnimationFrame --------------------------------------------------

/** How many nested frames `installSyncRaf` will drain before giving up. The
 *  reconcile loop below re-schedules itself once per correction; without a
 *  ceiling a loop that never converges (jsdom, where nothing actually scrolls)
 *  would spin until the 5s wall-clock bail-out inside virtual-core. */
const NESTED_FRAME_BUDGET = 50;

/**
 * Frames run synchronously, so a pin's write lands inside the triggering
 * `act()` — but frames scheduled *from inside* a frame are drained in a bounded
 * loop rather than re-entered.
 *
 * The distinction is load-bearing. `scrollToIndex` (which every pin now goes
 * through, #1165) ends in `scheduleScrollReconcile`, whose re-entrancy guard is
 * `if (this.rafId != null) return;` — with a naively synchronous stub the
 * callback runs *before* `rafId` is assigned, so the guard never arms,
 * `reconcileScroll` tail-calls the scheduler, and the result is a stack
 * overflow rather than a test failure. Queueing nested frames turns that
 * recursion into iteration, and the budget bounds it: in jsdom the loop cannot
 * converge, because virtual-core only learns the new offset from a `scroll`
 * event and nothing here actually scrolls.
 */
export function installSyncRaf(): void {
  let inFrame = false;
  const nested: FrameRequestCallback[] = [];
  (globalThis as { requestAnimationFrame?: unknown }).requestAnimationFrame = (
    cb: FrameRequestCallback,
  ) => {
    if (inFrame) {
      nested.push(cb);
      return nested.length;
    }
    inFrame = true;
    try {
      cb(0);
      for (let i = 0; i < NESTED_FRAME_BUDGET && nested.length > 0; i++) {
        nested.shift()?.(i + 1);
      }
      nested.length = 0;
    } finally {
      inFrame = false;
    }
    return 0;
  };
  (globalThis as { cancelAnimationFrame?: unknown }).cancelAnimationFrame =
    () => {};
}

export interface QueuedRafHarness {
  /** Run queued frames until the queue drains or the budget runs out. The
   *  budget is generous because the reconcile loop needs one pass per
   *  correction. */
  flushFrames: (budget?: number) => Promise<void>;
  reset: () => void;
}

/**
 * Frames are queued rather than run synchronously: `scrollToIndex` schedules a
 * rAF reconcile loop that reschedules itself until the target offset stops
 * moving, so a test that wants to watch it converge has to step it.
 */
export function installQueuedRaf(): QueuedRafHarness {
  let frames: FrameRequestCallback[] = [];
  (globalThis as { requestAnimationFrame?: unknown }).requestAnimationFrame = (
    cb: FrameRequestCallback,
  ) => {
    frames.push(cb);
    return frames.length;
  };
  (globalThis as { cancelAnimationFrame?: unknown }).cancelAnimationFrame =
    () => {};
  return {
    flushFrames: async (budget = 60) => {
      for (let i = 0; i < budget; i++) {
        const queued = frames;
        frames = [];
        if (queued.length === 0) return;
        await act(async () => {
          for (const cb of queued) cb(i);
        });
      }
    },
    reset: () => {
      frames = [];
    },
  };
}

// --- container geometry -----------------------------------------------------

/**
 * A scrollable container with a fixed height and a writable `scrollTop`.
 *
 * Writes are silent — no `scroll` event — which is what lets a suite park the
 * viewport somewhere and then assert on what the component does next without
 * the parking itself flipping `pinnedToBottom`. Suites that want the flip
 * dispatch `new Event("scroll")` explicitly, which also documents where the
 * race they are reproducing actually occurs.
 */
export function mockGeometry(
  el: HTMLElement,
  scrollHeight: number,
  clientHeight: number,
): void {
  let top = 0;
  Object.defineProperty(el, "scrollHeight", {
    configurable: true,
    get: () => scrollHeight,
  });
  Object.defineProperty(el, "clientHeight", {
    configurable: true,
    get: () => clientHeight,
  });
  Object.defineProperty(el, "scrollTop", {
    configurable: true,
    get: () => top,
    set: (v: number) => {
      top = v;
    },
  });
}

/**
 * The virtualizer-aware container: `scrollHeight` is read from the spacer's
 * live inline height rather than a constant, so the bottom genuinely moves as
 * rows measure — exactly as it does in the app, and the crux of every #1143
 * scroll bug. A constant `scrollHeight` makes the naive
 * `scrollTop = scrollHeight` implementation look correct.
 *
 * Rows measure `VIEWPORT_H` here (virtual-core falls back to `offsetHeight`,
 * which `vitest.setup.ts` stubs) against the 140px estimate — the same
 * direction of error as production, ~7x.
 *
 * Unlike `mockGeometry`, writes dispatch a `scroll` event: the component's
 * `handleScroll` and virtual-core's own cached offset both learn about a scroll
 * only that way, and the reconcile loop cannot converge without it.
 */
export function makeScrollable(el: HTMLElement, viewportH = VIEWPORT_H): void {
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
    get: () => viewportH,
  });
  Object.defineProperty(el, "scrollTop", {
    configurable: true,
    get: () => top,
    set: (v: number) => {
      top = Math.max(0, Math.min(v, Math.max(0, spacerHeight() - viewportH)));
      el.dispatchEvent(new Event("scroll"));
    },
  });
  // `behavior: "smooth"` is stepped rather than instant, because the steps are
  // load-bearing: a real smooth scroll fires `scroll` at every intermediate
  // offset, and `handleScroll` reads each one as "not at the bottom" and
  // detaches the pin. An instant stub would hide that half of the bug and let
  // the naive implementation look like it arrives.
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
