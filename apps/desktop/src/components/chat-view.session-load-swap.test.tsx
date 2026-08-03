// @vitest-environment jsdom
//
// #866: on relaunch (or any fresh session switch), ChatView first renders the
// localStorage-cached tail (message-cache.ts, last 50 messages), then
// loadSession() (chat.ts:457-490) async-replaces it with the full backend
// history. Because the previously-rendered tail keeps the same message ids
// (React key, chat-view.tsx:692), those DOM nodes are reused as older
// messages are prepended above them — the precondition for the browser's
// default CSS scroll anchoring (no overflow-anchor:none anywhere in this app)
// to adjust scrollTop mid-mutation. If that adjustment lands in more than one
// step, an intermediate `scroll` event can flip `pinnedToBottom.current` to
// false via handleScroll before the ResizeObserver's rAF-scheduled settle
// (chat-view.tsx:587-606) runs and would otherwise re-pin — stranding the
// user mid-conversation instead of at the true bottom.
//
// jsdom has no real scroll anchoring, so this test injects the intermediate
// `scroll` event directly to isolate and reproduce the exact race
// deterministically, reusing the ResizeObserver/rAF/mockGeometry harness from
// chat-view.autoscroll.test.tsx.

import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

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

// The pin is coalesced through requestAnimationFrame; run it synchronously so
// a fired observer callback commits its scrollTop write within the same act().
const ro = installResizeObserverStub();
installSyncRaf();

function shortCachedMessages(): Message[] {
  return [
    { id: "u1", sessionId: SID, role: "user", content: "hi", createdAt: 1 },
    {
      id: "a1",
      sessionId: SID,
      role: "assistant",
      content: "world",
      createdAt: 1,
    },
  ];
}

// Mimics loadSession's full-history replace (chat.ts:457-490): many older
// messages prepended above the same tail ids already rendered from the
// cache, so React's key-reuse (chat-view.tsx:692) preserves those DOM nodes —
// the precondition for scroll anchoring described above.
function fullHistory(n: number): Message[] {
  const older: Message[] = Array.from({ length: n }, (_, i) => ({
    id: `old${i}`,
    sessionId: SID,
    role: i % 2 === 0 ? "user" : "assistant",
    content: `msg ${i}`,
    createdAt: i,
  }));
  return [...older, ...shortCachedMessages()];
}

function seedShort() {
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: { [SID]: shortCachedMessages() },
    streamingBySession: {},
    turnStartBySession: {},
    turnStartByMessage: {},
    toolStepsByMessage: {},
    reasoningByMessage: {},
  });
}

function renderChat() {
  const { container } = render(<ChatView />);
  return container.querySelector(
    '[data-testid="chat-scroll"]',
  ) as HTMLDivElement;
}

const fireObserver = () => ro.fire();

beforeEach(() => {
  ro.reset();
});
afterEach(() => {
  useChatStore.setState({ messagesBySession: {}, toolStepsByMessage: {} });
  useFindStore.setState({ open: false, sessionId: null });
  vi.useRealTimers();
});

describe("ChatView session-load swap-in (#866)", () => {
  it("lands at the true bottom despite a transient pinnedToBottom flip during the cache→full-history swap", () => {
    seedShort();
    const el = renderChat();
    // jsdom has no layout, so scrollHeight/clientHeight are 0 until mocked —
    // mount's real pin (Effect B) can't be observed against real geometry.
    // Establish the state it would have produced (pinned at the short cached
    // content's bottom) explicitly, matching the autoscroll test's pattern.
    mockGeometry(el, 400, 200);
    el.scrollTop = 400;

    // loadSession's async replace lands: full history swaps in, DOM grows.
    act(() => {
      useChatStore.setState({
        messagesBySession: { [SID]: fullHistory(60) },
      });
    });
    mockGeometry(el, 4000, 200);

    // Simulate the scroll-anchoring race: an intermediate, only-partially-
    // compensated scroll event fires before the ResizeObserver settle,
    // flipping pinnedToBottom.current to false via handleScroll.
    act(() => {
      el.scrollTop = 2000;
      el.dispatchEvent(new Event("scroll"));
    });

    // The post-layout ResizeObserver settle now runs.
    fireObserver();

    // Must land at the TRUE bottom despite the transient flip above.
    expect(el.scrollTop).toBe(4000);
  });

  it("does not force-pin a resize well after the swap window if the user has since deliberately scrolled up", () => {
    vi.useFakeTimers();
    seedShort();
    const el = renderChat();
    mockGeometry(el, 400, 200);
    el.scrollTop = 400;

    act(() => {
      useChatStore.setState({
        messagesBySession: { [SID]: fullHistory(60) },
      });
    });
    mockGeometry(el, 4000, 200);

    // Well past FORCE_PIN_WINDOW_MS — the forced-pin arm from the session
    // switch above has expired.
    vi.advanceTimersByTime(5000);

    act(() => {
      el.scrollTop = 500; // deliberate scroll-up, well after the swap window
      el.dispatchEvent(new Event("scroll"));
    });
    fireObserver();

    expect(el.scrollTop).toBe(500); // stayed detached — window had expired
  });

  // Both #866 failure modes now run through one `pinToTail` gate (review on
  // #1092: "one code path, not two overlapping ones"). This is #1103's
  // equal-height case — the swap resizes nothing, so no ResizeObserver settle
  // ever arrives — asserted here so the convergence keeps covering it and the
  // two modes can't drift back apart.
  it("still pins when the swap-in resizes nothing, with no settle to ride", () => {
    seedShort();
    const el = renderChat();
    mockGeometry(el, 4000, 200);
    el.scrollTop = 0;

    act(() => {
      useChatStore.setState({
        messagesBySession: { [SID]: fullHistory(60) },
      });
    });

    // Deliberately no fireObserver(): the height never changed.
    expect(el.scrollTop).toBe(4000);
  });
});
