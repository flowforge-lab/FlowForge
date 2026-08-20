import { beforeEach, describe, expect, it, vi } from "vitest";
import { ipc } from "@/lib/ipc";
import {
  clampDrawerHeight,
  MAX_DRAWER_HEIGHT,
  MIN_DRAWER_HEIGHT,
  useTerminalStore,
} from "@/store/terminal";

function reset() {
  useTerminalStore.setState({
    openSessions: new Set(),
    bySession: {},
    hasHydrated: true,
  });
}

beforeEach(() => {
  reset();
  vi.spyOn(ipc, "closeTerminal").mockResolvedValue(undefined);
});

describe("terminal store (#1284)", () => {
  it("opening the drawer seeds a first tab", () => {
    useTerminalStore.getState().toggleDrawer("s1");
    const slice = useTerminalStore.getState().bySession.s1;
    expect(slice.tabs).toHaveLength(1);
    expect(slice.activeTabId).toBe(slice.tabs[0].tabId);
  });

  it("keeps sessions independent", () => {
    const store = useTerminalStore.getState();
    store.toggleDrawer("s1");
    store.toggleDrawer("s2");
    store.addTab("s2");

    expect(useTerminalStore.getState().bySession.s1.tabs).toHaveLength(1);
    expect(useTerminalStore.getState().bySession.s2.tabs).toHaveLength(2);
    // Closing one session's drawer leaves the other's open — this is the
    // split-pane guarantee.
    useTerminalStore.getState().toggleDrawer("s1");
    expect(useTerminalStore.getState().openSessions.has("s1")).toBe(false);
    expect(useTerminalStore.getState().openSessions.has("s2")).toBe(true);
  });

  it("reopening a closed drawer reuses its existing tabs", () => {
    const store = useTerminalStore.getState();
    store.toggleDrawer("s1");
    store.addTab("s1");
    const before = useTerminalStore.getState().bySession.s1.tabs;

    useTerminalStore.getState().toggleDrawer("s1"); // close
    useTerminalStore.getState().toggleDrawer("s1"); // reopen

    expect(useTerminalStore.getState().bySession.s1.tabs).toEqual(before);
    expect(ipc.closeTerminal).not.toHaveBeenCalled();
  });

  it("closing a tab kills its shell and focuses the tab to its left", () => {
    const store = useTerminalStore.getState();
    store.toggleDrawer("s1");
    store.addTab("s1");
    store.addTab("s1");
    const tabs = useTerminalStore.getState().bySession.s1.tabs;
    tabs.forEach((t, i) =>
      useTerminalStore.getState().bindTerminal("s1", t.tabId, `term-${i}`),
    );

    // Close the last (focused) tab.
    useTerminalStore.getState().closeTab("s1", tabs[2].tabId);

    expect(ipc.closeTerminal).toHaveBeenCalledWith("term-2");
    const after = useTerminalStore.getState().bySession.s1;
    expect(after.tabs).toHaveLength(2);
    expect(after.activeTabId).toBe(tabs[1].tabId);
  });

  it("closing an inactive tab leaves the focus where it was", () => {
    const store = useTerminalStore.getState();
    store.toggleDrawer("s1");
    store.addTab("s1");
    const [first, second] = useTerminalStore.getState().bySession.s1.tabs;

    useTerminalStore.getState().closeTab("s1", first.tabId);

    expect(useTerminalStore.getState().bySession.s1.activeTabId).toBe(
      second.tabId,
    );
  });

  it("numbers shells per session and never renumbers a survivor", () => {
    // The folder is identical for every tab in a pane, so the number is the only
    // thing distinguishing them (#1287 review) -- and renaming a tab the user is
    // looking at, just because an earlier one closed, is worse than a gap.
    const store = useTerminalStore.getState();
    store.toggleDrawer("s1");
    store.addTab("s1");
    store.addTab("s1");
    const tabs = useTerminalStore.getState().bySession.s1.tabs;
    expect(tabs.map((t) => t.shellNumber)).toEqual([1, 2, 3]);

    useTerminalStore.getState().closeTab("s1", tabs[1].tabId);
    expect(
      useTerminalStore.getState().bySession.s1.tabs.map((t) => t.shellNumber),
    ).toEqual([1, 3]);

    // A new tab takes one past the highest live number, so it collides with
    // neither survivor.
    useTerminalStore.getState().addTab("s1");
    expect(
      useTerminalStore.getState().bySession.s1.tabs.map((t) => t.shellNumber),
    ).toEqual([1, 3, 4]);

    // Numbering is per session: a second pane's first shell is also 1.
    useTerminalStore.getState().toggleDrawer("s2");
    expect(
      useTerminalStore.getState().bySession.s2.tabs.map((t) => t.shellNumber),
    ).toEqual([1]);
  });

  it("ignores an exit for a terminal it no longer knows", () => {
    useTerminalStore.getState().toggleDrawer("s1");
    const before = useTerminalStore.getState().bySession;

    useTerminalStore.getState().applyExited("term-gone");

    // Same object identity: an unknown id must not spur a re-render, because
    // every tab close produces one of these events.
    expect(useTerminalStore.getState().bySession).toBe(before);
  });

  it("deleting a session kills its shells and forgets the drawer", () => {
    const store = useTerminalStore.getState();
    store.toggleDrawer("s1");
    store.addTab("s1");
    useTerminalStore
      .getState()
      .bySession.s1.tabs.forEach((t, i) =>
        useTerminalStore.getState().bindTerminal("s1", t.tabId, `term-${i}`),
      );

    useTerminalStore.getState().clearSession("s1");

    expect(ipc.closeTerminal).toHaveBeenCalledWith("term-0");
    expect(ipc.closeTerminal).toHaveBeenCalledWith("term-1");
    expect(useTerminalStore.getState().bySession.s1).toBeUndefined();
    expect(useTerminalStore.getState().openSessions.has("s1")).toBe(false);
  });

  it("clamps the drawer height to a usable range", () => {
    expect(clampDrawerHeight(10)).toBe(MIN_DRAWER_HEIGHT);
    expect(clampDrawerHeight(10_000)).toBe(MAX_DRAWER_HEIGHT);
    expect(clampDrawerHeight(300.4)).toBe(300);
  });
});
