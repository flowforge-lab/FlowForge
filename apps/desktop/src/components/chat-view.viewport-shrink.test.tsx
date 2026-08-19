// @vitest-environment jsdom
//
// #1283 part C: switching sessions from the sidebar lands *above* the newest
// message, and the jump-to-latest arrow has to be clicked by hand.
//
// #1165 part 1 (#1181) fixed the pin's *target* — every trigger converges
// through `virtualizer.scrollToEnd()` instead of aiming at the under-stated
// `scrollHeight`. That is why the pane-open paths (palette, session toast, pane
// picker) land correctly, and why modelling the sidebar path as a session prop
// flip plus an async `loadSession` — `chat-view.open-session-pin.test.tsx` —
// also lands correctly. The remaining loss is not in the target at all: it is
// that the transcript's **viewport** shrinks a moment after the switch and
// nothing re-pins.
//
// `session-pane.tsx` renders four self-hiding, per-session panels above the
// transcript (process, observer, notebook-kernel, goal) and the per-session
// `InputBar` below it, all flex siblings of `ChatView`. Switching to a session
// that has a goal — or a running process, or a multi-line draft — mounts chrome
// the previous session did not have, and the transcript column gets shorter by
// exactly that much, a commit or an IPC round trip after the switch pinned it.
//
// A shorter viewport moves the bottom away from a reader who is pinned to it
// *without moving `scrollTop`*, so no `scroll` event fires and nothing in
// `chat-view.tsx` learns about it: the ResizeObserver watches `contentEl`, the
// content wrapper, whose height did not change. The user is left one panel's
// worth above the newest message with `pinnedToBottom.current` still true.
//
// Mutation bar: drop the scroller from the observed set (i.e. today's code) and
// the first case goes red on the distance-from-bottom assertion.

import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { ChatView } from "@/components/chat-view";
import { useChatStore } from "@/store/chat";
import {
  VIEWPORT_H,
  installQueuedRaf,
  installResizeObserverStub,
  makeScrollable,
} from "@/test/chat-scroll-harness";
import type { Message } from "@/bindings";

const SID = "s1";
const TOTAL = 400;
const LAST_ID = `m${TOTAL - 1}`;
/** The transcript column after a goal panel (or a grown input bar) appears. */
const SHRUNK_H = 600;

const ro = installResizeObserverStub();
const raf = installQueuedRaf();
const flushFrames = raf.flushFrames;

function msgs(count: number): Message[] {
  return Array.from(
    { length: count },
    (_, i) =>
      ({
        id: `m${i}`,
        sessionId: SID,
        role: i % 2 === 0 ? "user" : "assistant",
        content: `message ${i}`,
        createdAt: i + 1,
      }) as Message,
  );
}

const scroller = (c: HTMLElement) =>
  c.querySelector('[data-testid="chat-scroll"]') as HTMLDivElement;
const mounted = (c: HTMLElement, id: string) =>
  c.querySelector(`[data-message-id="${id}"]`) !== null;
/** Distance from the true bottom, against the viewport as it stands now. */
const fromBottom = (el: HTMLElement) =>
  el.scrollHeight - el.scrollTop - el.clientHeight;

beforeEach(() => {
  raf.reset();
  ro.reset();
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: { [SID]: msgs(TOTAL) },
    streamingBySession: {},
    turnStartBySession: {},
    turnStartByMessage: {},
    toolStepsByMessage: {},
    reasoningByMessage: {},
  });
});
afterEach(() => {
  useChatStore.setState({ messagesBySession: {}, toolStepsByMessage: {} });
});

describe("per-session chrome shrinking the transcript (#1283)", () => {
  it("re-pins to the tail when the viewport gets shorter", async () => {
    const view = render(<ChatView sessionId={SID} />);
    const scrollEl = scroller(view.container);
    const geometry = makeScrollable(scrollEl);
    await flushFrames();

    // Precondition: the switch landed on the newest message.
    expect(mounted(view.container, LAST_ID)).toBe(true);
    expect(fromBottom(scrollEl)).toBeLessThan(40);

    // The session's goal panel resolves and mounts above the transcript. The
    // content is untouched, so only the scroller resizes — `contentEl` does
    // not, which is why firing is element-filtered here.
    geometry.setViewportHeight(SHRUNK_H);
    await act(async () => {
      ro.fire(scrollEl);
    });
    await flushFrames();

    expect(mounted(view.container, LAST_ID)).toBe(true);
    expect(fromBottom(scrollEl)).toBeLessThan(40);
  }, 20_000);

  it("does not re-pin a reader who has scrolled up", async () => {
    const view = render(<ChatView sessionId={SID} />);
    const scrollEl = scroller(view.container);
    const geometry = makeScrollable(scrollEl);
    await flushFrames();

    // The reader scrolls a long way up — the `scroll` event detaches the pin.
    await act(async () => {
      scrollEl.scrollTop = 0;
    });
    const parked = scrollEl.scrollTop;

    geometry.setViewportHeight(SHRUNK_H);
    await act(async () => {
      ro.fire(scrollEl);
    });
    await flushFrames();

    expect(scrollEl.scrollTop).toBe(parked);
    expect(fromBottom(scrollEl)).toBeGreaterThan(VIEWPORT_H);
  }, 20_000);
});
