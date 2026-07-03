// @vitest-environment jsdom

import { act } from "react";
import type { ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { render as rtlRender, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { AppShell } from "@/components/app-shell";
import { SessionSidebar } from "@/components/session-sidebar";
import { useChatStore } from "@/store/chat";
import { usePanesStore, leaves, MAX_PANES } from "@/store/panes";
import { usePrefsStore } from "@/store/prefs";
import { useSessionPrefsStore } from "@/store/session-prefs";
import type { Session } from "@/bindings";

function session(id: string, partial: Partial<Session> = {}): Session {
  return {
    id,
    goal: `Session ${id}`,
    title: null,
    summary: null,
    status: "active",
    createdAt: 0,
    updatedAt: 0,
    ...partial,
  };
}

function render(ui: ReactElement) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root: Root = createRoot(container);
  act(() => {
    root.render(ui);
  });
  return {
    container,
    root,
    cleanup: () => {
      act(() => root.unmount());
      container.remove();
    },
  };
}

function click(el: Element | null | undefined) {
  act(() => {
    (el as HTMLElement | undefined)?.click();
  });
}

describe("SessionSidebar integration (#185)", () => {
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
    document.body.innerHTML = "";
    localStorage.clear();
  });

  it("collapses to the icon rail, persists, and reopens from the rail (#670)", () => {
    const { cleanup } = render(<AppShell />);

    const aside = document.querySelector("aside");
    // Expanded width now comes from the persisted inline style (#204), default 240px.
    expect((aside as HTMLElement | null)?.style.width).toBe("240px");
    expect(aside?.className).not.toContain("w-12");
    // No expand affordance while open, and the old in-main "Show sidebar" button
    // is gone — the rail owns expand now (#670).
    expect(document.querySelector('[aria-label="Show sidebar"]')).toBeNull();
    expect(document.querySelector('[aria-label="Expand sidebar"]')).toBeNull();

    click(document.querySelector('[title="Collapse sidebar"]'));
    expect(usePrefsStore.getState().sidebarCollapsed).toBe(true);
    expect(
      JSON.parse(localStorage.getItem("ff-prefs") ?? "{}").state
        .sidebarCollapsed,
    ).toBe(true);

    // Collapsed → a ~48px rail (w-12), not a fully hidden w-0 aside.
    expect(document.querySelector("aside")?.className).toContain("w-12");
    expect(
      document.querySelector('[aria-label="Expand sidebar"]'),
    ).not.toBeNull();

    cleanup();
    const persisted = JSON.parse(localStorage.getItem("ff-prefs") ?? "{}")
      .state as { sidebarCollapsed?: boolean };
    usePrefsStore.setState({
      sidebarCollapsed: persisted.sidebarCollapsed ?? false,
    });
    render(<AppShell />);

    expect(document.querySelector("aside")?.className).toContain("w-12");

    const reopen = document.querySelector('[aria-label="Expand sidebar"]');
    expect(reopen).not.toBeNull();
    click(reopen);
    expect(usePrefsStore.getState().sidebarCollapsed).toBe(false);
    expect(
      (document.querySelector("aside") as HTMLElement | null)?.style.width,
    ).toBe("240px");
    expect(document.querySelector("aside")?.className).not.toContain("w-12");
  });

  it("reveals the filter via ⋯ → Search and hides it on Esc", async () => {
    const user = userEvent.setup();
    rtlRender(<SessionSidebar />);

    expect(screen.queryByLabelText("Filter sessions")).toBeNull();

    await user.click(screen.getByLabelText("Sidebar options"));
    await user.click(screen.getByRole("menuitem", { name: /search/i }));

    const input = screen.getByLabelText("Filter sessions");

    await user.type(input, "Parser");
    await user.keyboard("{Escape}");

    expect(screen.queryByLabelText("Filter sessions")).toBeNull();
  });

  it("the + button swaps a new blank session into the focused pane (#671 item 1)", async () => {
    usePanesStore.setState({ root: null, focusedPaneId: null });
    usePanesStore.getState().init(["s1", "s2"], "s1");
    expect(usePanesStore.getState().leafCount()).toBe(1);

    const focusedPane = usePanesStore.getState().focusedPaneId as string;
    const sessionOf = (id: string) =>
      leaves(usePanesStore.getState().root!).find((l) => l.id === id)
        ?.sessionId;
    const before = sessionOf(focusedPane);

    const user = userEvent.setup();
    rtlRender(<SessionSidebar />);
    await user.click(screen.getByLabelText("New session"));

    // #671 item 1 changed `＋` from "split into a new pane" to an in-place swap:
    // the focused pane's session id changes, but the layout stays a single leaf so
    // clicking `＋` never accretes empty panes.
    await waitFor(() => expect(sessionOf(focusedPane)).not.toBe(before));
    expect(usePanesStore.getState().leafCount()).toBe(1);
    expect(usePanesStore.getState().root?.type).toBe("leaf");
  });

  it("the + button falls back to an in-pane swap at MAX_PANES (#245 2a)", async () => {
    usePanesStore.setState({ root: null, focusedPaneId: null });
    usePanesStore.getState().init(["s1"], "s1");
    for (let i = 2; i <= MAX_PANES; i++) {
      const target = usePanesStore.getState().focusedPaneId as string;
      usePanesStore.getState().splitRight(target, `s${i}`);
    }
    expect(usePanesStore.getState().leafCount()).toBe(MAX_PANES);

    const sessionId = (id: string) =>
      leaves(usePanesStore.getState().root!).find((l) => l.id === id)
        ?.sessionId;
    const focusedPane = usePanesStore.getState().focusedPaneId as string;
    const before = sessionId(focusedPane);

    const user = userEvent.setup();
    rtlRender(<SessionSidebar />);
    await user.click(screen.getByLabelText("New session"));

    // At the cap: no new pane — the focused pane swaps to the fresh session.
    await waitFor(() => expect(sessionId(focusedPane)).not.toBe(before));
    expect(usePanesStore.getState().leafCount()).toBe(MAX_PANES);
  });

  it("shows non-dismissed sessions and sinks dismissed ones to the bottom (no tabs)", () => {
    useChatStore.setState({
      sessions: [
        session("a", { title: "Active one" }),
        session("b", { title: "Dismissed one" }),
      ],
      activeSessionId: "a",
    });
    useSessionPrefsStore.setState({ dismissed: ["b"] });

    const { container, cleanup } = render(<SessionSidebar />);

    // No All/Dismissed tab row.
    expect(container.querySelector('[aria-label="Session list"]')).toBeNull();
    // Both are visible in one list (few enough to be within the first batch);
    // the dismissed row is dimmed and sinks below the live one.
    expect(container.textContent).toContain("Active one");
    expect(container.textContent).toContain("Dismissed one");
    const activeIdx = (container.textContent ?? "").indexOf("Active one");
    const dismissedIdx = (container.textContent ?? "").indexOf("Dismissed one");
    expect(activeIdx).toBeLessThan(dismissedIdx);
    cleanup();
  });

  it('shows "Show more" when sessions exceed the reveal batch and reveals +25', () => {
    const many = Array.from({ length: 30 }, (_, i) =>
      session(`s${i}`, { title: `Chat ${i}` }),
    );
    useChatStore.setState({
      sessions: many,
      activeSessionId: null,
    });

    const { container, cleanup } = render(<SessionSidebar />);

    // First batch of 25 shows Chat 0..24, not Chat 25+.
    expect(container.textContent).toContain("Chat 24");
    expect(container.textContent).toContain("Show more");
    expect(container.textContent).not.toContain("Chat 29");

    const moreBtn = [...container.querySelectorAll("button")].find((b) =>
      b.textContent?.includes("Show more"),
    );
    click(moreBtn);
    expect(container.textContent).toContain("Chat 29");
    cleanup();
  });

  it("reveals dismissed sessions once Show more walks past the live ones", () => {
    const live = Array.from({ length: 30 }, (_, i) =>
      session(`s${i}`, { title: `Chat ${i}` }),
    );
    useChatStore.setState({
      sessions: [...live, session("z", { title: "Dismissed tail" })],
      activeSessionId: null,
    });
    useSessionPrefsStore.setState({ dismissed: ["z"] });

    const { container, cleanup } = render(<SessionSidebar />);
    // Dismissed sinks to the bottom, so it's past the first batch of 25.
    expect(container.textContent).not.toContain("Dismissed tail");

    const moreBtn = () =>
      [...container.querySelectorAll("button")].find((b) =>
        b.textContent?.includes("Show more"),
      );
    click(moreBtn());
    expect(container.textContent).toContain("Dismissed tail");
    cleanup();
  });

  it("keeps the active session visible even when it falls past the reveal batch", () => {
    const many = Array.from({ length: 30 }, (_, i) =>
      session(`s${i}`, { title: `Chat ${i}` }),
    );
    useChatStore.setState({
      sessions: many,
      activeSessionId: "s29",
    });

    const { container, cleanup } = render(<SessionSidebar />);

    // s29 would fall past the first 25, but the active session is pulled in.
    expect(container.textContent).toContain("Chat 29");
    expect(container.textContent).toContain("Show more");
    cleanup();
  });

  it("Show more then Show less re-compacts to the first batch", () => {
    const many = Array.from({ length: 30 }, (_, i) =>
      session(`s${i}`, { title: `Chat ${i}` }),
    );
    useChatStore.setState({ sessions: many, activeSessionId: null });

    const { container, cleanup } = render(<SessionSidebar />);
    const btn = () =>
      [...container.querySelectorAll("button")].find((b) =>
        /show more|show less/i.test(b.textContent ?? ""),
      );

    expect(container.textContent).not.toContain("Chat 29");
    click(btn()); // Show more → reveals all 30
    expect(container.textContent).toContain("Chat 29");
    expect(container.textContent).toContain("Show less");

    click(btn()); // Show less → back to first 25
    expect(container.textContent).not.toContain("Chat 29");
    cleanup();
  });

  it("header has no FlowForge title or theme toggle; keeps select, +, and options", () => {
    useChatStore.setState({ sessions: [session("a")], activeSessionId: "a" });
    const { container, cleanup } = render(<SessionSidebar />);

    // #667: title text and the theme toggle are gone from the sidebar header.
    expect(container.textContent).not.toContain("FlowForge");
    expect(
      container.querySelector('[title="Switch to dark theme"]'),
    ).toBeNull();
    expect(
      container.querySelector('[title="Switch to light theme"]'),
    ).toBeNull();

    // The retained controls are present.
    expect(
      container.querySelector('[title="Collapse sidebar"]'),
    ).not.toBeNull();
    expect(
      container.querySelector('[aria-label="Select sessions"]'),
    ).not.toBeNull();
    expect(
      container.querySelector('[aria-label="New session"]'),
    ).not.toBeNull();
    expect(
      container.querySelector('[aria-label="Sidebar options"]'),
    ).not.toBeNull();
    cleanup();
  });
});
