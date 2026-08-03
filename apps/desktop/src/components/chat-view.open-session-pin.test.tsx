// @vitest-environment jsdom
//
// #1165: opening an existing session into a pane landed *above* the newest
// message — the jump-to-bottom arrow had to be clicked by hand. Fresh and
// streaming sessions pinned fine; only opening an existing session missed.
//
// The cause is the same estimate/measurement confusion that broke the arrow in
// #1143's dogfood, one consumer over. Every pin wrote
// `scrollTop = scrollHeight`, and under virtualization `scrollHeight` *is*
// `getTotalSize()` — real heights for measured rows plus `ROW_ESTIMATE_PX`
// (140) for every row that has never mounted. On a cold open almost nothing is
// measured, so the write lands far short of the tail; `handleScroll` then reads
// that as "not at the bottom" and detaches the pin, and because the #866 arm is
// consumed by that same settle there is nothing left to re-pin. One click of
// the arrow was the user-visible remainder.
//
// The fix routes all three pin triggers through `pinToTail`, which goes through
// the virtualizer (`scrollToEnd`) so the target is recomputed each frame as
// rows measure.
//
// Both pin triggers that can open a session are covered here:
//   - the session-swap effect (`[targetSessionId, scrollEl, pinToTail]`), which
//     is what fires for ⌘K / the session toast / the pane picker;
//   - the transcript-identity effect (`[messages, ...]`), which is what fires
//     when `loadSession` swaps the cached tail for the full history underneath
//     an already-open session.
//
// What this suite does NOT assert, deliberately: that the arrow is absent
// afterwards. jsdom has no layout, so the arrow's `!atBottom` gate resolves the
// same way whichever implementation runs — asserting it would look like a guard
// and be none. That check is the `pnpm tauri dev` pass on a real database.

import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { ChatView } from "@/components/chat-view";
import { useChatStore } from "@/store/chat";
import {
  VIEWPORT_H,
  installQueuedRaf,
  makeScrollable,
} from "@/test/chat-scroll-harness";
import type { Message } from "@/bindings";

const SID = "s1";
const OTHER = "s2";
// Many viewports tall: 400 rows measuring 1000px each (virtual-core falls back
// to `offsetHeight`, which `vitest.setup.ts` stubs) against the 140px estimate
// is ~7x of accumulated error, the same direction as production. A pin that
// aims at the estimate cannot arrive by luck.
const TOTAL = 400;
const LAST_ID = `m${TOTAL - 1}`;

const raf = installQueuedRaf();
const flushFrames = raf.flushFrames;

function msgs(sessionId: string, count: number, prefix = "m"): Message[] {
  return Array.from(
    { length: count },
    (_, i) =>
      ({
        id: `${prefix}${i}`,
        sessionId,
        role: i % 2 === 0 ? "user" : "assistant",
        content: `message ${i}`,
        createdAt: i + 1,
      }) as Message,
  );
}

/** The <=50-message localStorage tail a cold open renders first
 *  (message-cache.ts), before `loadSession` replaces it. Shares the tail ids
 *  with `msgs(SID, TOTAL)` so the swap prepends rather than replaces — the
 *  #866 shape. */
function cachedTail(): Message[] {
  return msgs(SID, TOTAL).slice(-2);
}

function seed(messagesBySession: Record<string, Message[]>) {
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession,
    streamingBySession: {},
    turnStartBySession: {},
    turnStartByMessage: {},
    toolStepsByMessage: {},
    reasoningByMessage: {},
  });
}

const scroller = (c: HTMLElement) =>
  c.querySelector('[data-testid="chat-scroll"]') as HTMLDivElement;
const mounted = (c: HTMLElement, id: string) =>
  c.querySelector(`[data-message-id="${id}"]`) !== null;

beforeEach(() => {
  raf.reset();
});
afterEach(() => {
  useChatStore.setState({ messagesBySession: {}, toolStepsByMessage: {} });
});

describe("opening an existing session lands on the newest message (#1165)", () => {
  it("pins past the estimated total when a pane swaps to a long session", async () => {
    seed({ [OTHER]: msgs(OTHER, 4, "o"), [SID]: msgs(SID, TOTAL) });
    const view = render(<ChatView sessionId={OTHER} />);
    const scrollEl = scroller(view.container);
    makeScrollable(scrollEl);
    await flushFrames();

    // The pane swaps to the long session — ⌘K, the session toast, the pane
    // picker all land here.
    view.rerender(<ChatView sessionId={SID} />);
    // The target the old implementation used: the spacer as it stands on the
    // commit that swapped the session in, which counts 140px for every row that
    // has never mounted.
    const naiveTarget = scrollEl.scrollHeight;
    await flushFrames();

    expect(mounted(view.container, LAST_ID)).toBe(true);
    // The discriminating assertion. `scrollTop = scrollHeight` could not have
    // gone past `naiveTarget`; converging through the virtualizer overshoots it
    // by many viewports as the tail rows measure.
    expect(scrollEl.scrollTop - naiveTarget).toBeGreaterThanOrEqual(VIEWPORT_H);
  }, 20_000);

  it("pins past the estimated total when the cached tail is replaced by the full history", async () => {
    seed({ [SID]: cachedTail() });
    const view = render(<ChatView sessionId={SID} />);
    const scrollEl = scroller(view.container);
    makeScrollable(scrollEl);
    await flushFrames();

    // loadSession's async replace lands: the full history is prepended above
    // the cached tail, whose ids (and DOM nodes) are reused.
    await act(async () => {
      useChatStore.setState({ messagesBySession: { [SID]: msgs(SID, TOTAL) } });
    });
    const naiveTarget = scrollEl.scrollHeight;
    await flushFrames();

    expect(mounted(view.container, LAST_ID)).toBe(true);
    expect(scrollEl.scrollTop - naiveTarget).toBeGreaterThanOrEqual(VIEWPORT_H);
  }, 20_000);
});
