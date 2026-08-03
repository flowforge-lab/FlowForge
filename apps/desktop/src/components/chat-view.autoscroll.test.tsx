// @vitest-environment jsdom
//
// #1025: streaming autoscroll must pin to the *post-layout* bottom. The fix
// re-pins from a ResizeObserver on the content wrapper (fires after layout),
// not from a commit-time effect (which read a pre-layout scrollHeight and
// landed above the tail on large markdown/code blocks).
//
// jsdom has no layout engine and no ResizeObserver, so we stub the observer to
// capture its callback, fake the scroll container's geometry, then invoke the
// callback to simulate a post-commit height growth and assert the pin (or its
// suppression when the user has scrolled up / find is open).

import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { ChatView } from "@/components/chat-view";
import { useChatStore } from "@/store/chat";
import { useFindStore } from "@/store/find";
import {
  installResizeObserverStub,
  installSyncRaf,
  mockGeometry,
} from "@/test/chat-scroll-harness";
import type { Message } from "@/bindings";

const SID = "s1";

// Capture every ResizeObserver the component mounts so the test can drive it,
// and run the pin's coalescing frame synchronously so a fired observer callback
// commits its scrollTop write within the same act().
const ro = installResizeObserverStub();
installSyncRaf();

function seed() {
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: {
      [SID]: [
        { id: "u1", sessionId: SID, role: "user", content: "hi", createdAt: 1 },
        {
          id: "a1",
          sessionId: SID,
          role: "assistant",
          content: "world",
          createdAt: 1,
        },
      ] as Message[],
    },
    streamingBySession: { [SID]: "a1" },
    turnStartBySession: {},
    turnStartByMessage: {},
    toolStepsByMessage: {},
    reasoningByMessage: {},
  });
}

function renderChat() {
  const { container } = render(<ChatView />);
  const scrollEl = container.querySelector(
    '[data-testid="chat-scroll"]',
  ) as HTMLDivElement;
  // 1000px of content in a 200px viewport → scrollable.
  mockGeometry(scrollEl, 1000, 200);
  scrollEl.scrollTop = 0; // start above the tail
  return scrollEl;
}

const fireObserver = () => ro.fire();

beforeEach(() => {
  ro.reset();
  seed();
});
afterEach(() => {
  useChatStore.setState({ messagesBySession: {}, toolStepsByMessage: {} });
  useFindStore.setState({ open: false, sessionId: null });
});

describe("ChatView post-layout autoscroll (#1025)", () => {
  it("re-pins to the post-layout bottom when content grows after commit", () => {
    const el = renderChat();
    expect(ro.observers().length).toBeGreaterThan(0);

    fireObserver();

    // Pinned by default → the observer pins to the *current* (post-layout) height.
    expect(el.scrollTop).toBe(1000);
  });

  it("does not yank back down after the user scrolls up mid-stream", () => {
    const el = renderChat();

    // User scrolls up: 1000 - 0 - 200 = 800px from bottom (> 40px threshold),
    // so handleScroll detaches the pin.
    act(() => {
      el.dispatchEvent(new Event("scroll"));
    });
    el.scrollTop = 0;

    fireObserver();

    expect(el.scrollTop).toBe(0); // stayed detached, no forced pin
  });

  it("suppresses auto-follow while find is open for this session", () => {
    const el = renderChat();

    act(() => {
      useFindStore.getState().openFind(SID, { query: "world" });
    });
    el.scrollTop = 0;

    fireObserver();

    expect(el.scrollTop).toBe(0); // find-open suppression preserved
  });
});
