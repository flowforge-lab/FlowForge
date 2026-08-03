// @vitest-environment jsdom
//
// The outline wired into the transcript (#1165). These are the assertions the
// issue's acceptance list turns on, and each one is written so that neutering
// the behaviour — not merely the rendering — is what turns it red:
//
//  - one click puts *that* message on screen in a session many viewports tall
//    (mutation: drop the `reveal` call);
//  - marker positions do not move while scrolling (mutation: position from the
//    virtualizer's measured offsets);
//  - the jump is not undone by the #866 pin (mutation: drop the arm retirement
//    in the revealer);
//  - the markers do not rebuild on a streamed token (mutation: key the memo on
//    `groups` identity).
//
// Asserting only that markers render, or that the store still holds every
// message, proves the premise and not the behaviour — that was precisely the
// gap #1155 shipped with on find.

import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Counts the O(n) marker walk so the streaming hot path can be asserted on.
// The real implementation runs — this only tallies the calls.
let outlineBuilds = 0;
vi.mock("@/lib/transcript-outline", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("@/lib/transcript-outline")>();
  return {
    ...actual,
    buildOutline: (...args: Parameters<typeof actual.buildOutline>) => {
      outlineBuilds++;
      return actual.buildOutline(...args);
    },
  };
});
const buildOutlineCalls = () => outlineBuilds;

import { ChatView } from "@/components/chat-view";
import { useChatStore } from "@/store/chat";
import { useExperimentalStore } from "@/store/experimental";
import { useTranscriptScroll } from "@/store/transcript-scroll";
import {
  installQueuedRaf,
  installResizeObserverStub,
  makeScrollable,
} from "@/test/chat-scroll-harness";
import type { Message } from "@/bindings";

const SID = "s1";
const TOTAL = 400;

const ro = installResizeObserverStub();
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

function setFlag(on: boolean) {
  useExperimentalStore.setState((s) => ({
    flags: { ...s.flags, transcriptOutline: on },
  }));
}

const outline = (c: HTMLElement) =>
  c.querySelector<HTMLElement>('[data-testid="transcript-outline"]');
const markerButtons = (c: HTMLElement) =>
  Array.from(
    c.querySelectorAll<HTMLButtonElement>(
      '[data-testid="transcript-outline"] button',
    ),
  );
const scroller = (c: HTMLElement) =>
  c.querySelector('[data-testid="chat-scroll"]') as HTMLDivElement;
const mounted = (c: HTMLElement, id: string) =>
  c.querySelector(`[data-message-id="${id}"]`) !== null;

async function renderLong() {
  const view = render(<ChatView sessionId={SID} />);
  const scrollEl = scroller(view.container);
  makeScrollable(scrollEl);
  await flushFrames();
  return { ...view, scrollEl };
}

beforeEach(() => {
  ro.reset();
  raf.reset();
  setFlag(true);
  seed({ [SID]: msgs(SID, TOTAL) });
});
afterEach(() => {
  setFlag(false);
  useChatStore.setState({ messagesBySession: {}, toolStepsByMessage: {} });
  useTranscriptScroll.setState({ revealers: {} });
});

describe("transcript outline in ChatView (#1165)", () => {
  it("is gated by the experimental flag", async () => {
    setFlag(false);
    const off = await renderLong();
    expect(outline(off.container)).toBeNull();
    off.unmount();

    setFlag(true);
    const on = await renderLong();
    expect(outline(on.container)).not.toBeNull();
  }, 20_000);

  it("stays hidden for a session too short to navigate", async () => {
    seed({ [SID]: msgs(SID, 4) });
    const { container } = await renderLong();

    expect(outline(container)).toBeNull();
  });

  it("puts the clicked message on screen in one click", async () => {
    const { container, scrollEl } = await renderLong();
    // Precondition, or this proves nothing: the session opens pinned at the
    // tail, so a message near the top is nowhere near the DOM.
    const target = markerButtons(container)[2];
    const targetId = /Jump to message (\d+) of/.exec(
      target.getAttribute("aria-label")!,
    )!;
    const targetIndex = Number(targetId[1]) - 1;
    const targetMessageId = `m${targetIndex}`;
    expect(mounted(container, targetMessageId)).toBe(false);
    const before = scrollEl.scrollTop;
    expect(before).toBeGreaterThan(0);

    await act(async () => {
      target.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flushFrames();

    expect(mounted(container, targetMessageId)).toBe(true);
    expect(scrollEl.scrollTop).toBeLessThan(before);
  }, 20_000);

  it("does not snap back to the tail after a jump (#866 pin)", async () => {
    // The one window where the pin can eat the jump, staged exactly:
    //
    //  - a session opens on its localStorage-cached tail, which arms
    //    `forcePinUntil` for 4s with that tail's head id;
    //  - `loadSession` prepends the full history, so the arm's proof (a changed
    //    first message id) is now true;
    //  - nothing consumes the arm, because consuming needs an *authoritative*
    //    ResizeObserver settle and the height may not change until later.
    //
    // Click a marker inside that window and the rows the jump mounts resize the
    // content — the settle then finds the arm live and force-pins to the tail,
    // unless the reveal retired it.
    seed({ [SID]: msgs(SID, TOTAL).slice(-20) });
    const view = render(<ChatView sessionId={SID} />);
    const scrollEl = scroller(view.container);
    makeScrollable(scrollEl);
    await flushFrames();

    await act(async () => {
      useChatStore.setState({ messagesBySession: { [SID]: msgs(SID, TOTAL) } });
    });
    await flushFrames();
    const contentEl = scrollEl.firstElementChild as HTMLElement;
    // Precondition: armed and unconsumed — the transcript is at the tail with
    // the swap-in's proof in place.
    expect(scrollEl.scrollTop).toBeGreaterThan(scrollEl.scrollHeight - 2000);

    await act(async () => {
      markerButtons(view.container)[2].dispatchEvent(
        new MouseEvent("click", { bubbles: true }),
      );
    });
    await flushFrames();
    const afterJump = scrollEl.scrollTop;
    // Sanity: the jump actually moved the view off the tail.
    expect(afterJump).toBeLessThan(scrollEl.scrollHeight - 1000);

    // The rows the jump mounted resize the content; the post-layout settle runs.
    ro.fire(contentEl);
    await flushFrames();

    expect(scrollEl.scrollTop).toBeLessThan(scrollEl.scrollHeight - 1000);
  }, 20_000);

  it("keeps marker positions fixed while the transcript scrolls", async () => {
    const { container, scrollEl } = await renderLong();
    const before = markerButtons(container).map((b) => b.style.top);
    const rowsBefore = Array.from(
      container.querySelectorAll("[data-message-id]"),
      (n) => n.getAttribute("data-message-id"),
    ).join(",");

    // Scroll far enough to mount and measure a block of rows that were pure
    // 140px estimates a moment ago. Positions derived from those offsets would
    // shift; index proportions cannot.
    await act(async () => {
      scrollEl.scrollTop = Math.floor(scrollEl.scrollHeight / 3);
    });
    await flushFrames();

    // Precondition, or the assertion below is trivially true: the scroll really
    // did window in a different block of rows, which is what measures them and
    // would shift every offset below.
    const rowsAfter = Array.from(
      container.querySelectorAll("[data-message-id]"),
      (n) => n.getAttribute("data-message-id"),
    ).join(",");
    expect(rowsAfter).not.toBe(rowsBefore);

    expect(markerButtons(container).map((b) => b.style.top)).toEqual(before);
  }, 20_000);

  it("does not rebuild the markers on a streamed token", async () => {
    await renderLong();
    const before = buildOutlineCalls();

    // A token only mutates the last message's body, but the tail re-folds, so
    // `groups` is a fresh array on every one of them. Keying the memo on that
    // array would walk all 400 groups per token — the #1022 churn, on a
    // component that renders 200 nodes. Node identity can't see this: markers
    // are keyed by message id, so React reuses the DOM either way. Counting the
    // walk is the only thing that can.
    await act(async () => {
      useChatStore.setState((s) => {
        const list = s.messagesBySession[SID].slice();
        const last = list[list.length - 1];
        list[list.length - 1] = { ...last, content: last.content + " more" };
        return { messagesBySession: { ...s.messagesBySession, [SID]: list } };
      });
    });

    expect(buildOutlineCalls()).toBe(before);
  }, 20_000);
});
