// @vitest-environment jsdom
//
// Multi-select mode (#643): enter/exit, row-toggle-vs-open, tab-scoped Select-all,
// bulk dismiss/restore, and bulk delete with a single confirm.

import {
  render as rtlRender,
  screen,
  cleanup,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { SessionSidebar } from "@/components/session-sidebar";
import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import { usePanesStore } from "@/store/panes";
import { usePrefsStore } from "@/store/prefs";
import { useSessionPrefsStore } from "@/store/session-prefs";
import type { Session } from "@/bindings";

// jsdom lacks ResizeObserver, which radix's ScrollArea measures with on mount.
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}
(globalThis as { ResizeObserver?: unknown }).ResizeObserver =
  ResizeObserverStub;

function session(id: string, partial: Partial<Session> = {}): Session {
  return {
    id,
    goal: null,
    title: id.toUpperCase(),
    summary: null,
    status: "active",
    createdAt: 0,
    updatedAt: 0,
    ...partial,
  };
}

const dismissedTab = () =>
  [
    ...document.querySelectorAll('[aria-label="Session list"] button'),
  ][1] as HTMLElement;

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
    sessions: [session("s1"), session("s2"), session("s3")],
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

describe("SessionSidebar — multi-select (#643)", () => {
  it("enters and exits select mode from the header toggle", async () => {
    const user = userEvent.setup();
    rtlRender(<SessionSidebar />);

    expect(screen.queryByLabelText("Select all sessions")).toBeNull();

    await user.click(screen.getByLabelText("Select sessions"));
    expect(screen.getByLabelText("Select all sessions")).not.toBeNull();
    expect(screen.getByLabelText("Select S2")).not.toBeNull();

    await user.click(screen.getByLabelText("Exit select mode"));
    expect(screen.queryByLabelText("Select all sessions")).toBeNull();
  });

  it("row click toggles selection in select mode, but opens the session otherwise", async () => {
    const user = userEvent.setup();
    const selectSpy = vi.fn();
    useChatStore.setState({ selectSession: selectSpy });
    rtlRender(<SessionSidebar />);

    // Not in select mode → a normal row click opens the session.
    await user.click(screen.getByText("S2"));
    expect(selectSpy).toHaveBeenCalledWith("s2");

    // In select mode → the same click toggles selection and does NOT open.
    await user.click(screen.getByLabelText("Select sessions"));
    selectSpy.mockClear();
    await user.click(screen.getByText("S2"));
    expect(screen.getByText("1 selected")).not.toBeNull();
    expect(selectSpy).not.toHaveBeenCalled();
  });

  it("Select-all covers every session in the All tab", async () => {
    const user = userEvent.setup();
    rtlRender(<SessionSidebar />);

    await user.click(screen.getByLabelText("Select sessions"));
    await user.click(screen.getByLabelText("Select all sessions"));
    expect(screen.getByText("3 selected")).not.toBeNull();

    // Toggling again clears the selection.
    await user.click(screen.getByLabelText("Select all sessions"));
    expect(screen.getByText("0 selected")).not.toBeNull();
  });

  it("Select-all is scoped to the active tab (Dismissed) and offers Restore", async () => {
    const user = userEvent.setup();
    useSessionPrefsStore.setState({ dismissed: ["s3"] });
    rtlRender(<SessionSidebar />);

    await user.click(dismissedTab());
    await user.click(screen.getByLabelText("Select sessions"));
    await user.click(screen.getByLabelText("Select all sessions"));

    // Only the single dismissed session is in scope.
    expect(screen.getByText("1 selected")).not.toBeNull();
    expect(screen.getByRole("button", { name: /Restore/ })).not.toBeNull();
  });

  it("clears the selection when switching tabs so a later bulk action can't hit off-view sessions (#643 review)", async () => {
    const user = userEvent.setup();
    // s3 dismissed so the Dismissed tab is enabled; s1/s2 live in the All tab.
    useSessionPrefsStore.setState({ dismissed: ["s3"] });
    const delSpy = vi.spyOn(ipc, "deleteSession").mockResolvedValue();
    rtlRender(<SessionSidebar />);

    // Select the All-tab sessions, then switch to Dismissed.
    await user.click(screen.getByLabelText("Select sessions"));
    await user.click(screen.getByLabelText("Select all sessions"));
    expect(screen.getByText("2 selected")).not.toBeNull();

    await user.click(dismissedTab());
    // Selection dropped — still in select mode, but nothing carried across.
    expect(screen.getByText("0 selected")).not.toBeNull();

    // A bulk Delete now can't touch the previously-selected (off-view) sessions.
    expect(screen.getByRole("button", { name: /Delete/ })).toHaveProperty(
      "disabled",
      true,
    );
    expect(delSpy).not.toHaveBeenCalled();
  });

  it("clears the selection when the filter changes (#643 review)", async () => {
    const user = userEvent.setup();
    rtlRender(<SessionSidebar />);

    await user.click(screen.getByLabelText("Select sessions"));
    await user.click(screen.getByLabelText("Select all sessions"));
    expect(screen.getByText("3 selected")).not.toBeNull();

    // Reveal the filter (⋯ → Search) and type — the selection must not survive.
    await user.click(screen.getByLabelText("Sidebar options"));
    await user.click(screen.getByRole("menuitem", { name: /search/i }));
    await user.type(screen.getByLabelText("Filter sessions"), "S1");
    expect(screen.getByText("0 selected")).not.toBeNull();
  });

  it("bulk Dismiss hides the selection and exits select mode", async () => {
    const user = userEvent.setup();
    rtlRender(<SessionSidebar />);

    await user.click(screen.getByLabelText("Select sessions"));
    await user.click(screen.getByLabelText("Select all sessions"));
    await user.click(screen.getByRole("button", { name: /Dismiss/ }));

    expect([...useSessionPrefsStore.getState().dismissed].sort()).toEqual([
      "s1",
      "s2",
      "s3",
    ]);
    // Selection cleared + mode exited.
    expect(screen.queryByLabelText("Select all sessions")).toBeNull();
  });

  it("bulk Restore un-hides the selection in the Dismissed tab", async () => {
    const user = userEvent.setup();
    useSessionPrefsStore.setState({ dismissed: ["s2", "s3"] });
    rtlRender(<SessionSidebar />);

    await user.click(dismissedTab());
    await user.click(screen.getByLabelText("Select sessions"));
    await user.click(screen.getByLabelText("Select all sessions"));
    await user.click(screen.getByRole("button", { name: /Restore/ }));

    expect(useSessionPrefsStore.getState().dismissed).toEqual([]);
  });

  it("bulk Delete confirms once, then deletes every selected session", async () => {
    const user = userEvent.setup();
    const delSpy = vi.spyOn(ipc, "deleteSession").mockResolvedValue();
    rtlRender(<SessionSidebar />);

    await user.click(screen.getByLabelText("Select sessions"));
    // Select the two non-active sessions.
    await user.click(screen.getByLabelText("Select S2"));
    await user.click(screen.getByLabelText("Select S3"));
    expect(screen.getByText("2 selected")).not.toBeNull();

    await user.click(screen.getByRole("button", { name: /Delete/ }));

    // A single confirm dialog, not one per row.
    const dialog = screen.getByRole("alertdialog");
    expect(within(dialog).getByText("Delete 2 sessions?")).not.toBeNull();
    await user.click(within(dialog).getByRole("button", { name: "Delete" }));

    await waitFor(() =>
      expect(useChatStore.getState().sessions.map((s) => s.id)).toEqual(["s1"]),
    );
    expect(delSpy).toHaveBeenCalledTimes(2);
    expect(screen.queryByLabelText("Select all sessions")).toBeNull();
  });
});
