// @vitest-environment jsdom
//
// #1143: scrollback past the loaded window. #1147 bounded the initial load to the
// tail (HISTORY_WINDOW), which made session switching independent of history
// length but left older messages unreachable. Scrolling near the top now fetches
// the page above and prepends it.
//
// Two things must hold, and they pull in opposite directions:
//
//   1. The rows under the user's eyes must not move. Inserting content above the
//      viewport grows scrollHeight, so scrollTop has to be compensated by exactly
//      the inserted height.
//   2. The #866 forced pin must not fire. That mechanism watches for the first
//      message id changing while armed and yanks the view to the tail — correct
//      for a cache→full-history swap, catastrophic here, since a prepend changes
//      the same id for the opposite reason.
//
// Reuses the ResizeObserver/rAF/mockGeometry harness from
// chat-view.session-load-swap.test.tsx.

import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ChatView, LOAD_OLDER_THRESHOLD_PX } from "@/components/chat-view";
import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import { useFindStore } from "@/store/find";
import type { Message } from "@/bindings";

const SID = "s1";

let observers: ResizeObserverStub[] = [];
class ResizeObserverStub {
  cb: ResizeObserverCallback;
  live = true;
  constructor(cb: ResizeObserverCallback) {
    this.cb = cb;
    observers.push(this);
  }
  observe() {}
  unobserve() {}
  disconnect() {
    this.live = false;
  }
}
(globalThis as { ResizeObserver?: unknown }).ResizeObserver =
  ResizeObserverStub;

(globalThis as { requestAnimationFrame?: unknown }).requestAnimationFrame = (
  cb: FrameRequestCallback,
) => {
  cb(0);
  return 0;
};
(globalThis as { cancelAnimationFrame?: unknown }).cancelAnimationFrame =
  () => {};

// scrollHeight is a function of how many rows are held, so the compensation can
// be observed: prepending grows it and scrollTop must absorb the difference.
function mockGeometry(el: HTMLElement, clientHeight: number, rowPx = 10) {
  let top = 0;
  Object.defineProperty(el, "clientHeight", {
    configurable: true,
    get: () => clientHeight,
  });
  Object.defineProperty(el, "scrollHeight", {
    configurable: true,
    get: () =>
      (useChatStore.getState().messagesBySession[SID] ?? []).length * rowPx,
  });
  Object.defineProperty(el, "scrollTop", {
    configurable: true,
    get: () => top,
    set: (v: number) => {
      top = v;
    },
  });
}

const msg = (id: string, createdAt: number): Message => ({
  id,
  sessionId: SID,
  role: "user",
  content: id,
  createdAt,
});

function seedWindow(n: number): Message[] {
  return Array.from({ length: n }, (_, i) => msg(`w-${i}`, 1000 + i));
}

function renderChat() {
  const { container } = render(<ChatView />);
  return container.querySelector(
    '[data-testid="chat-scroll"]',
  ) as HTMLDivElement;
}

function fireObserver() {
  act(() => {
    for (const o of observers)
      if (o.live) o.cb([], o as unknown as ResizeObserver);
  });
}

beforeEach(() => {
  observers = [];
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: { [SID]: seedWindow(30) },
    hasMoreBySession: {},
    loadingOlderBySession: {},
    streamingBySession: {},
    turnStartBySession: {},
    turnStartByMessage: {},
    toolStepsByMessage: {},
    reasoningByMessage: {},
  });
});
afterEach(() => {
  useChatStore.setState({
    messagesBySession: {},
    toolStepsByMessage: {},
    hasMoreBySession: {},
    loadingOlderBySession: {},
  });
  useFindStore.setState({ open: false, sessionId: null });
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("ChatView scrollback (#1143)", () => {
  it("preserves the reading position when older messages are prepended", async () => {
    const spy = vi
      .spyOn(ipc, "getMessagesAround")
      .mockResolvedValue([
        ...Array.from({ length: 20 }, (_, i) => msg(`o-${i}`, i)),
        msg("w-0", 1000),
      ]);

    const el = renderChat();
    mockGeometry(el, 100);
    // Scrolled up near the top: 30 rows * 10px = 300 tall, viewport 100.
    el.scrollTop = 50;
    const distanceFromBottom = el.scrollHeight - el.scrollTop;

    await act(async () => {
      el.dispatchEvent(new Event("scroll"));
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(spy).toHaveBeenCalledWith(SID, "w-0", expect.any(Number), 0);
    expect(useChatStore.getState().messagesBySession[SID]).toHaveLength(50);
    // The content the user was reading stays the same distance from the bottom;
    // in absolute terms scrollTop moved down by exactly the 20 inserted rows.
    expect(el.scrollHeight - el.scrollTop).toBe(distanceFromBottom);
    expect(el.scrollTop).toBe(250);
  });

  it("does not yank to the bottom when a prepend changes the first message id", async () => {
    // The #866 detector keys on exactly this change. Arm it the way a session
    // switch would, then prepend: the view must stay put.
    vi.spyOn(ipc, "getMessagesAround").mockResolvedValue([
      ...Array.from({ length: 20 }, (_, i) => msg(`o-${i}`, i)),
      msg("w-0", 1000),
    ]);

    const el = renderChat();
    mockGeometry(el, 100);
    el.scrollTop = 50;

    await act(async () => {
      el.dispatchEvent(new Event("scroll"));
      await Promise.resolve();
      await Promise.resolve();
    });
    // A ResizeObserver settle follows any height change; it must not re-pin.
    fireObserver();

    expect(el.scrollTop).not.toBe(el.scrollHeight);
    expect(el.scrollTop).toBe(250);
  });

  it("compensates before paint, never showing an uncompensated frame", async () => {
    // The judder's root cause: compensating inside requestAnimationFrame lets the
    // browser paint one frame at the wrong offset first. A layout effect runs
    // between React's DOM commit and the paint, so the misaligned frame never
    // exists. rAF is stubbed to run immediately here, so the distinction is
    // asserted structurally: scrollTop must already be correct at the moment the
    // prepend commits, with no rAF callback needed to fix it.
    const rafCallbacks: FrameRequestCallback[] = [];
    const realRaf = globalThis.requestAnimationFrame;
    (globalThis as { requestAnimationFrame?: unknown }).requestAnimationFrame =
      (cb: FrameRequestCallback) => {
        // Defer instead of running inline, so anything relying on rAF to correct
        // the offset would be observably too late.
        rafCallbacks.push(cb);
        return 0;
      };

    try {
      vi.spyOn(ipc, "getMessagesAround").mockResolvedValue([
        ...Array.from({ length: 20 }, (_, i) => msg(`o-${i}`, i)),
        msg("w-0", 1000),
      ]);

      const el = renderChat();
      mockGeometry(el, 100);
      el.scrollTop = 50;
      const distanceFromBottom = el.scrollHeight - el.scrollTop;

      await act(async () => {
        el.dispatchEvent(new Event("scroll"));
        await Promise.resolve();
        await Promise.resolve();
      });

      // Correct already, with no pending rAF having run.
      expect(el.scrollHeight - el.scrollTop).toBe(distanceFromBottom);
      expect(el.scrollTop).toBe(250);
    } finally {
      (
        globalThis as { requestAnimationFrame?: unknown }
      ).requestAnimationFrame = realRaf;
    }
  });

  it("does not fetch when the start of history is already loaded", async () => {
    useChatStore.setState({ hasMoreBySession: { [SID]: false } });
    const spy = vi.spyOn(ipc, "getMessagesAround");

    const el = renderChat();
    mockGeometry(el, 100);
    el.scrollTop = 0;

    await act(async () => {
      el.dispatchEvent(new Event("scroll"));
      await Promise.resolve();
    });

    expect(spy).not.toHaveBeenCalled();
  });

  it("does not fetch while scrolled away from the top", async () => {
    const spy = vi.spyOn(ipc, "getMessagesAround");

    const el = renderChat();
    mockGeometry(el, 100);
    // Well beyond the prefetch runway, so no request is warranted.
    el.scrollTop = LOAD_OLDER_THRESHOLD_PX * 2;

    await act(async () => {
      el.dispatchEvent(new Event("scroll"));
      await Promise.resolve();
    });

    expect(spy).not.toHaveBeenCalled();
  });

  it("keeps the indicator mounted so it never shifts layout", async () => {
    // The judder fix: a conditionally rendered spinner inside the scrolled content
    // pushes the transcript down when it appears and pulls it back when it goes —
    // two uncompensated layout changes per page load. It now stays mounted while
    // older history exists and only its opacity changes.
    // Never resolves: the indicator's visible state during the fetch is the point.
    vi.spyOn(ipc, "getMessagesAround").mockReturnValue(
      new Promise<Message[]>(() => {}),
    );

    const { container } = render(<ChatView />);
    const el = container.querySelector(
      '[data-testid="chat-scroll"]',
    ) as HTMLDivElement;
    mockGeometry(el, 100);
    el.scrollTop = 50;

    const indicator = () =>
      container.querySelector('[data-testid="loading-older"]');
    // Occupies its space before any fetch, since older history is presumed.
    expect(indicator()).not.toBeNull();
    expect(indicator()?.className).toContain("opacity-0");

    await act(async () => {
      el.dispatchEvent(new Event("scroll"));
      await Promise.resolve();
    });
    // Same element, same height — only visibility changed.
    expect(indicator()).not.toBeNull();
    expect(indicator()?.className).toContain("opacity-100");
  });
});
