// @vitest-environment jsdom
//
// Compact results dropdown (#876 surface A) — keyboard nav + the #902 review
// nit: `activeHitIndex` must reset to 0 on every filter keystroke, mirroring
// the full-screen modal's `setSelected(0)` on typing. Before the fix it was
// only clamped at read time, so a stale index from a longer previous result
// set stayed clamped onto whatever row now sits at that position instead of
// resetting to the top.

import { render, screen, cleanup, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Hoisted so the (hoisted) vi.mock factory can close over the spy.
const { searchMessages } = vi.hoisted(() => ({
  searchMessages: vi.fn(),
}));
vi.mock("@/lib/ipc", () => ({ ipc: { searchMessages } }));

import { SessionSidebar } from "@/components/session-sidebar";
import { useChatStore } from "@/store/chat";
import { usePanesStore } from "@/store/panes";
import { usePrefsStore } from "@/store/prefs";
import { useSessionPrefsStore } from "@/store/session-prefs";
import { useAllConversationsSearchStore } from "@/store/all-conversations-search";
import type { Session } from "@/bindings";
import type { SearchHit } from "@/bindings/SearchHit";

// jsdom lacks ResizeObserver (radix ScrollArea) + pointer-capture (radix Popover).
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}
(globalThis as { ResizeObserver?: unknown }).ResizeObserver =
  ResizeObserverStub;
window.HTMLElement.prototype.hasPointerCapture = () => false;
window.HTMLElement.prototype.scrollIntoView = () => {};

function session(id: string, title: string): Session {
  return {
    id,
    goal: null,
    title,
    summary: null,
    status: "active",
    createdAt: 0,
    updatedAt: 0,
  };
}

function hit(sessionId: string, messageId: string): SearchHit {
  return {
    sessionId,
    sessionTitle: null,
    messageId,
    role: "assistant",
    snippet: `<mark>bug</mark> in ${sessionId}`,
    createdAt: Date.now(),
  };
}

describe("SessionSidebar — compact dropdown keyboard nav (#876/#902)", () => {
  beforeEach(() => {
    Object.defineProperty(window, "matchMedia", {
      writable: true,
      value: (query: string) => ({
        matches: query.includes("dark"),
        media: query,
        addEventListener: () => {},
        removeEventListener: () => {},
      }),
    });
    localStorage.clear();
    searchMessages.mockReset();
    usePrefsStore.setState({ sidebarCollapsed: false });
    usePanesStore.setState({ root: null, focusedPaneId: null });
    useAllConversationsSearchStore.setState({ open: false });
    useSessionPrefsStore.setState({ pinned: [], dismissed: [] });
    // None of the titles contain "bug" — every result comes from the content
    // dropdown, not the title-filtered list, so there's no exclusion overlap.
    useChatStore.setState({
      sessions: [
        session("s1", "Alpha"),
        session("s2", "Beta"),
        session("s3", "Gamma"),
      ],
      activeSessionId: "s1",
      messagesBySession: {},
      streamingBySession: {},
      toolStepsByMessage: {},
      bootstrapError: null,
    });
  });

  afterEach(() => {
    cleanup();
    localStorage.clear();
    vi.restoreAllMocks();
  });

  it("resets the highlighted row to the top on every filter keystroke", async () => {
    const user = userEvent.setup();
    searchMessages.mockResolvedValue([
      hit("s1", "m1"),
      hit("s2", "m2"),
      hit("s3", "m3"),
    ]);
    render(<SessionSidebar />);

    await user.click(screen.getByLabelText("Sidebar options"));
    await user.click(screen.getByRole("menuitem", { name: /search/i }));
    const input = screen.getByLabelText("Filter sessions");

    await user.type(input, "bug");
    await waitFor(() => expect(searchMessages).toHaveBeenCalled());

    const options = await waitFor(() => {
      const opts = screen.getAllByRole("option");
      expect(opts).toHaveLength(3);
      return opts;
    });
    expect(options[0].getAttribute("aria-selected")).toBe("true");

    // Move the highlight to the last row.
    await user.keyboard("{ArrowDown}{ArrowDown}");
    expect(screen.getAllByRole("option")[2].getAttribute("aria-selected")).toBe(
      "true",
    );

    // A fresh query still resolving to 3 (different) hits must re-highlight
    // the top row, not stay clamped on index 2.
    searchMessages.mockResolvedValue([
      hit("s2", "m2b"),
      hit("s1", "m1b"),
      hit("s3", "m3b"),
    ]);
    await user.type(input, "z");
    await waitFor(() =>
      expect(searchMessages).toHaveBeenLastCalledWith("bugz", 30),
    );

    await waitFor(() => {
      const opts = screen.getAllByRole("option");
      expect(opts[0].getAttribute("aria-selected")).toBe("true");
      expect(opts[2].getAttribute("aria-selected")).toBe("false");
    });
  });
});
