// @vitest-environment jsdom
//
// The message navigator wired into the transcript (#1290, replacing the #1165
// outline strip). These are the assertions the issue's acceptance list turns
// on, and each one is written so that neutering the behaviour — not merely the
// rendering — is what turns it red:
//
//  - nothing is on the right rail at rest (mutation: render the pill
//    unconditionally, i.e. the strip again in a smaller shape);
//  - the pill appears only once the scrollback passes the threshold (mutation:
//    drop the `NAVIGATOR_MIN_SCROLLBACK` comparison);
//  - the counter is in raw messages (mutation: pass `groups.length`, which is
//    what the strip used and what its aria-labels still said);
//  - one click on a row puts *that* message on screen in a session many
//    viewports tall (mutation: drop the `reveal` call);
//  - the jump is not undone by the #866 pin (mutation: drop the arm retirement
//    in the revealer);
//  - neither the markers nor the ordinals rebuild on a streamed token
//    (mutation: key either memo on `groups` identity).
//
// Asserting only that a pill renders, or that the store still holds every
// message, proves the premise and not the behaviour — that was precisely the
// gap #1155 shipped with on find.
//
// The popup is portalled to `document.body`, outside RTL's `container`: query
// it through `document`, or a `toBeNull()` passes for the wrong reason.

import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Counts the two O(n) walks so the streaming hot path can be asserted on. The
// real implementations run — this only tallies the calls.
let outlineBuilds = 0;
let ordinalWalks = 0;
vi.mock("@/lib/transcript-outline", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("@/lib/transcript-outline")>();
  return {
    ...actual,
    buildOutline: (...args: Parameters<typeof actual.buildOutline>) => {
      outlineBuilds++;
      return actual.buildOutline(...args);
    },
    messageOrdinals: (...args: Parameters<typeof actual.messageOrdinals>) => {
      ordinalWalks++;
      return actual.messageOrdinals(...args);
    },
  };
});
const buildOutlineCalls = () => outlineBuilds;
const ordinalCalls = () => ordinalWalks;

import { ChatView } from "@/components/chat-view";
import { useChatStore } from "@/store/chat";
import { useTranscriptScroll } from "@/store/transcript-scroll";
import { useMessageNavigator } from "@/store/message-navigator";
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

const pill = () =>
  document.querySelector<HTMLButtonElement>(
    '[data-testid="message-navigator-pill"]',
  );
const popup = () =>
  document.querySelector<HTMLElement>(
    '[data-testid="message-navigator-popup"]',
  );
const rows = () =>
  Array.from(
    document.querySelectorAll<HTMLElement>(
      '[data-testid="message-navigator-row"]',
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

/** Park the viewport a third of the way down and let the component see it:
 *  `makeScrollable`'s writes dispatch a real `scroll` event, which is the only
 *  thing that tells `handleScroll` where the fold is and that the transcript is
 *  moving at all. */
async function scrollUp(scrollEl: HTMLDivElement) {
  await act(async () => {
    scrollEl.scrollTop = Math.floor(scrollEl.scrollHeight / 3);
  });
  await flushFrames();
}

/** Open the navigator the way the keyboard does, without the app shell. */
async function openFromKeyboard() {
  await act(async () => {
    useMessageNavigator.getState().openNavigator(SID);
  });
}

beforeEach(() => {
  ro.reset();
  raf.reset();
  useMessageNavigator.setState({ openSessions: new Set() });
  seed({ [SID]: msgs(SID, TOTAL) });
});
afterEach(() => {
  // RTL's auto-cleanup is off in this project (vitest runs without `globals`),
  // and every query here is document-wide.
  cleanup();
  useChatStore.setState({ messagesBySession: {}, toolStepsByMessage: {} });
  useTranscriptScroll.setState({ revealers: {} });
  useMessageNavigator.setState({ openSessions: new Set() });
});

describe("message navigator in ChatView (#1290)", () => {
  it("leaves the rail clean at rest", async () => {
    const { container } = await renderLong();

    // The whole point of #1290: an idle transcript has no navigation chrome on
    // it at all — no strip, no pill.
    expect(pill()).toBeNull();
    expect(
      container.querySelector('[data-testid="message-navigator"]'),
    ).toBeNull();
  }, 20_000);

  it("stays hidden for a session too short to navigate", async () => {
    seed({ [SID]: msgs(SID, 4) });
    const { scrollEl } = await renderLong();
    await scrollUp(scrollEl);

    expect(pill()).toBeNull();
  });

  it("shows the pill once the transcript is scrolled back", async () => {
    const { scrollEl } = await renderLong();
    expect(pill()).toBeNull();

    await scrollUp(scrollEl);

    expect(pill()).not.toBeNull();
  }, 20_000);

  it("counts raw messages, not groups", async () => {
    const { scrollEl } = await renderLong();
    await scrollUp(scrollEl);

    // 400 messages fold to fewer groups (each assistant turn swallows nothing
    // here, but the denominator must still be the message count the reader can
    // see numbered), so a denominator of anything but 400 is the group count
    // leaking through.
    expect(pill()!.textContent).toContain(`/${TOTAL}`);
  }, 20_000);

  it("puts the clicked message on screen in one click", async () => {
    const { container, scrollEl } = await renderLong();
    await scrollUp(scrollEl);
    fireEvent.click(pill()!);

    // Precondition, or this proves nothing: the target is nowhere near the DOM.
    const target = rows()[2];
    const targetMessageId = target.getAttribute("data-message-id")!;
    expect(mounted(container, targetMessageId)).toBe(false);
    const before = scrollEl.scrollTop;
    expect(before).toBeGreaterThan(0);

    await act(async () => {
      fireEvent.click(target);
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
    // Click a row inside that window and the rows the jump mounts resize the
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

    // Straight from the keyboard, so the still-pinned tail doesn't have to be
    // scrolled off first — the threshold bypass is what makes that possible.
    await openFromKeyboard();
    await act(async () => {
      fireEvent.click(rows()[2]);
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

  it("keeps the row ordinals fixed while the transcript scrolls", async () => {
    // The successor to the strip's "marker positions don't move" case, and the
    // same mutation bar: the rows are addressed by index proportion, so nothing
    // the virtualizer measures may change what a row says. Numbering derived
    // from measured offsets would drift as unmeasured rows mount.
    const { container, scrollEl } = await renderLong();
    await openFromKeyboard();
    const before = rows().map((r) => r.textContent);
    const rowsBefore = Array.from(
      container.querySelectorAll("[data-message-id]"),
      (n) => n.getAttribute("data-message-id"),
    ).join(",");

    await scrollUp(scrollEl);

    // Precondition, or the assertion below is trivially true: the scroll really
    // did window in a different block of rows, which is what measures them and
    // would shift every offset below.
    const rowsAfter = Array.from(
      container.querySelectorAll("[data-message-id]"),
      (n) => n.getAttribute("data-message-id"),
    ).join(",");
    expect(rowsAfter).not.toBe(rowsBefore);

    expect(rows().map((r) => r.textContent)).toEqual(before);
  }, 20_000);

  it("does not rebuild the markers or the ordinals on a streamed token", async () => {
    await renderLong();
    const builds = buildOutlineCalls();
    const walks = ordinalCalls();

    // A token only mutates the last message's body, but the tail re-folds, so
    // `groups` is a fresh array on every one of them. Keying either memo on
    // that array would walk all 400 groups per token — the #1022 churn. Node
    // identity can't see this: rows are keyed by message id, so React reuses
    // the DOM either way. Counting the walks is the only thing that can.
    await act(async () => {
      useChatStore.setState((s) => {
        const list = s.messagesBySession[SID].slice();
        const last = list[list.length - 1];
        list[list.length - 1] = { ...last, content: last.content + " more" };
        return { messagesBySession: { ...s.messagesBySession, [SID]: list } };
      });
    });

    expect(buildOutlineCalls()).toBe(builds);
    expect(ordinalCalls()).toBe(walks);
  }, 20_000);

  it("opens from the keyboard while pinned at the tail", async () => {
    // ⌘⇧O's contract: a keyboard user who asks for the list by name gets it,
    // threshold or no threshold. `app-shell.tsx` writes exactly this store.
    await renderLong();
    expect(pill()).toBeNull();

    await openFromKeyboard();

    expect(popup()).not.toBeNull();
    expect(rows().length).toBeGreaterThan(0);
  }, 20_000);
});
