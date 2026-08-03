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
// The harness (`test/chat-scroll-harness.ts`) reproduces (1) by deriving
// `scrollHeight` from the spacer's live inline height rather than a constant —
// so the bottom genuinely moves as rows measure, exactly as it does in the app.
// Rows measure 1000px here (virtual-core falls back to `offsetHeight`, which
// `vitest.setup.ts` stubs) against the 140px estimate, which is the same
// direction of error as production.
//
// Mutation bar (the reviewer's): put `el.scrollTo({ top: el.scrollHeight })`
// back and the test below goes red — it lands 1000px short of where it must be,
// i.e. a full viewport, which is the "descends a screenful per click" symptom.

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
// Many viewports tall: 400 rows at a measured 1000px each is ~400 screens, so a
// single under-estimated jump cannot accidentally arrive.
const TOTAL = 400;
const LAST_ID = `m${TOTAL - 1}`;

const raf = installQueuedRaf();
const flushFrames = raf.flushFrames;

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

async function renderScrolledToTop() {
  const view = render(<ChatView sessionId={SID} />);
  const scrollEl = view.container.querySelector(
    '[data-testid="chat-scroll"]',
  ) as HTMLDivElement;
  makeScrollable(scrollEl);
  // Park at the top and let the scroll handler detach the pin, which is what
  // puts the arrow on screen in the first place.
  //
  // This has to happen *before* the session-open pin's frames run. Since #1165
  // that pin converges to the true tail like the arrow does, so flushing first
  // would measure every tail row and leave `scrollHeight` honest — and the
  // whole subject of this suite is what the arrow does against a spacer that is
  // still mostly estimates. Detaching first is also the only state in which the
  // arrow appears at all: a user who scrolled up while the session was still
  // opening.
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
  raf.reset();
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
