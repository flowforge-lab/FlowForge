// @vitest-environment jsdom
//
// Sidebar header accent polish + collapsed icon rail (#670): the ＋ button is a
// plain ghost by default, the ☑ select toggle takes the accent only while active,
// and collapsing swaps the sidebar for a thin rail whose four controls each fire
// their action (search opens a placeholder popover).

import { render, screen, cleanup } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { SessionSidebar } from "@/components/session-sidebar";
import { useChatStore } from "@/store/chat";
import { usePanesStore } from "@/store/panes";
import { usePrefsStore } from "@/store/prefs";
import { useSessionPrefsStore } from "@/store/session-prefs";
import { useSettingsStore } from "@/store/settings";
import type { Session } from "@/bindings";

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

function session(id: string): Session {
  return {
    id,
    goal: null,
    title: id.toUpperCase(),
    summary: null,
    status: "active",
    createdAt: 0,
    updatedAt: 0,
  };
}

describe("SessionSidebar — header accent + collapsed rail (#670)", () => {
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
    usePrefsStore.setState({ sidebarCollapsed: false });
    usePanesStore.setState({ root: null, focusedPaneId: null });
    useSessionPrefsStore.setState({ pinned: [], dismissed: [] });
    useChatStore.setState({
      sessions: [session("s1"), session("s2")],
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

  it("renders ＋ as a plain ghost (no accent) by default", () => {
    render(<SessionSidebar />);
    const plus = screen.getByLabelText("New session");
    expect(plus.className).not.toContain("emerald");
    expect(plus.className).toContain("text-muted-foreground");
  });

  it("gives ☑ the accent only while select mode is active", async () => {
    const user = userEvent.setup();
    render(<SessionSidebar />);
    const toggle = screen.getByLabelText("Select sessions");

    // Idle → plain ghost.
    expect(toggle.className).not.toContain("emerald");

    await user.click(toggle);
    // Active → accent-filled (same emerald token ＋ used before).
    expect(screen.getByLabelText("Select sessions").className).toContain(
      "emerald",
    );
  });

  it("shows a rail with four controls when collapsed", () => {
    usePrefsStore.setState({ sidebarCollapsed: true });
    render(<SessionSidebar />);
    expect(screen.getByLabelText("Expand sidebar")).not.toBeNull();
    expect(screen.getByLabelText("Search sessions")).not.toBeNull();
    expect(screen.getByLabelText("New session")).not.toBeNull();
    expect(screen.getByLabelText("Settings")).not.toBeNull();
  });

  it("rail panel-toggle expands the sidebar", async () => {
    const user = userEvent.setup();
    usePrefsStore.setState({ sidebarCollapsed: true });
    render(<SessionSidebar />);

    await user.click(screen.getByLabelText("Expand sidebar"));
    expect(usePrefsStore.getState().sidebarCollapsed).toBe(false);
  });

  it("rail search opens the placeholder popover", async () => {
    const user = userEvent.setup();
    usePrefsStore.setState({ sidebarCollapsed: true });
    render(<SessionSidebar />);

    await user.click(screen.getByLabelText("Search sessions"));
    expect(screen.getByText("Coming soon.")).not.toBeNull();
  });

  it("rail ＋ starts a new session and gear opens Settings", async () => {
    const user = userEvent.setup();
    const newSession = vi.fn().mockResolvedValue(undefined);
    const openSettings = vi.fn();
    useChatStore.setState({ newSession });
    useSettingsStore.setState({ openSettings });
    usePrefsStore.setState({ sidebarCollapsed: true });
    render(<SessionSidebar />);

    await user.click(screen.getByLabelText("New session"));
    expect(newSession).toHaveBeenCalled();

    await user.click(screen.getByLabelText("Settings"));
    expect(openSettings).toHaveBeenCalled();
  });
});
