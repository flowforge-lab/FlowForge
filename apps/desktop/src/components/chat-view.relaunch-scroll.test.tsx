// @vitest-environment jsdom
//
// #866: on relaunch a long session rendered pinned to the *top* instead of the
// latest message. Both scroll effects in ChatView key off refs that are still
// null on the first commit, because `messages === undefined` (no localStorage
// cache entry) renders `null` — no scroll container exists yet. Their deps
// (`[findOn]` / `[targetSessionId, findOn]`) don't change when the transcript
// arrives, so on that mount no ResizeObserver was ever attached and no initial
// pin ever ran. The cached-tail path has the mirror problem: the commit-time
// pin runs against the <=50-message cache, and when the full history replaces
// it at the same height there is no observer fire to re-pin.
//
// jsdom has no layout engine and no ResizeObserver, so the observer is stubbed
// (captured, driven by hand) and the container's geometry is faked.

import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { ChatView } from "@/components/chat-view";
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

// The pin is coalesced through requestAnimationFrame; run it synchronously so
// the scrollTop write lands inside the act() that triggered it.
(globalThis as { requestAnimationFrame?: unknown }).requestAnimationFrame = (
  cb: FrameRequestCallback,
) => {
  cb(0);
  return 0;
};
(globalThis as { cancelAnimationFrame?: unknown }).cancelAnimationFrame =
  () => {};

function mockGeometry(
  el: HTMLElement,
  scrollHeight: number,
  clientHeight: number,
) {
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

function msgs(n: number): Message[] {
  const out: Message[] = [];
  for (let i = 0; i < n; i++) {
    out.push({
      id: `m${i}`,
      sessionId: SID,
      role: i % 2 === 0 ? "user" : "assistant",
      content: `msg ${i}`,
      createdAt: i,
    } as Message);
  }
  return out;
}

function setMessages(list: Message[]) {
  useChatStore.setState({ messagesBySession: { [SID]: list } });
}

function scrollEl(container: HTMLElement) {
  return container.querySelector(
    '[data-testid="chat-scroll"]',
  ) as HTMLDivElement | null;
}

beforeEach(() => {
  observers = [];
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: {},
    streamingBySession: {},
    turnStartBySession: {},
    turnStartByMessage: {},
    toolStepsByMessage: {},
    reasoningByMessage: {},
  });
});
afterEach(() => {
  useChatStore.setState({ messagesBySession: {}, toolStepsByMessage: {} });
  useFindStore.setState({ open: false, sessionId: null });
});

describe("ChatView relaunch scroll (#866)", () => {
  it("pins to the bottom when the transcript arrives after mount (cache miss)", () => {
    // Cold start with no cached tail: the first commit renders nothing.
    const { container } = render(<ChatView />);
    expect(scrollEl(container)).toBeNull();

    // loadSession() resolves and the container mounts on a later commit.
    act(() => setMessages(msgs(200)));

    const el = scrollEl(container)!;
    expect(el).not.toBeNull();
    mockGeometry(el, 5000, 200);

    // The observer must exist for this mount (streaming follow depends on it)
    // and the view must land at the tail once layout settles.
    expect(observers.some((o) => o.live)).toBe(true);
    act(() => {
      for (const o of observers)
        if (o.live) o.cb([], o as unknown as ResizeObserver);
    });
    expect(el.scrollTop).toBe(5000);
  });

  it("re-pins when the cached tail is replaced by the full history at the same height", () => {
    // Relaunch with a cached tail already in the store.
    setMessages(msgs(4));
    const { container } = render(<ChatView />);
    const el = scrollEl(container)!;
    mockGeometry(el, 1000, 200);
    el.scrollTop = 0; // commit-time pin read a pre-layout height and landed short

    // Full history lands; the height happens to be unchanged, so no observer fire.
    act(() => setMessages(msgs(40)));

    expect(el.scrollTop).toBe(1000);
  });

  it("does not fight a user who scrolled up before the transcript swap", () => {
    setMessages(msgs(4));
    const { container } = render(<ChatView />);
    const el = scrollEl(container)!;
    mockGeometry(el, 1000, 200);

    // User scrolls up: 1000 - 0 - 200 = 800px from the bottom → pin detaches.
    act(() => {
      el.dispatchEvent(new Event("scroll"));
    });
    el.scrollTop = 0;

    act(() => setMessages(msgs(40)));

    expect(el.scrollTop).toBe(0);
  });

  it("stays suppressed while find is open for this session", () => {
    setMessages(msgs(4));
    const { container } = render(<ChatView />);
    const el = scrollEl(container)!;
    mockGeometry(el, 1000, 200);

    act(() => {
      useFindStore.getState().openFind(SID, { query: "msg 3" });
    });
    el.scrollTop = 0;

    act(() => setMessages(msgs(40)));

    expect(el.scrollTop).toBe(0);
  });
});
