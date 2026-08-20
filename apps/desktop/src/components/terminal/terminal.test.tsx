// @vitest-environment jsdom

import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ipc } from "@/lib/ipc";
import { TerminalDrawer } from "@/components/terminal";
import { AppShell } from "@/components/app-shell";
import { SessionPane } from "@/components/session-pane";
import { useChatStore } from "@/store/chat";
import { useSessionWorkspaceStore } from "@/store/session-workspace";
import { useTerminalStore } from "@/store/terminal";

// xterm draws to a canvas and measures real layout, neither of which jsdom
// provides. The stub keeps the parts this suite is about -- what we send to the
// shell, and what the shell's bytes do -- and drops the rendering.
const instances: MockTerm[] = [];

class MockTerm {
  cols = 80;
  rows = 24;
  options: Record<string, unknown> = {};
  written: string[] = [];
  disposed = false;
  focused = 0;
  private dataCb: ((data: string) => void) | null = null;

  open() {}
  loadAddon() {}
  focus() {
    this.focused += 1;
  }
  dispose() {
    this.disposed = true;
  }
  write(chunk: string | Uint8Array) {
    this.written.push(
      typeof chunk === "string" ? chunk : new TextDecoder().decode(chunk),
    );
  }
  onData(cb: (data: string) => void) {
    this.dataCb = cb;
    return { dispose: () => (this.dataCb = null) };
  }
  /** Drive the terminal as a user typing into it. */
  type(data: string) {
    this.dataCb?.(data);
  }
}

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    constructor() {
      const term = new MockTerm();
      instances.push(term);
      return term as unknown as object;
    }
  },
}));
vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit() {}
  },
}));
vi.mock("@xterm/xterm/css/xterm.css", () => ({}));

/** Let the `openTerminal` promise chain settle so `terminalId` is bound. */
async function settle() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

let openCalls: { sessionId: string; cols: number; rows: number }[] = [];
let onDataCbs: ((bytes: Uint8Array) => void)[] = [];
let nextId = 1;

beforeEach(() => {
  instances.length = 0;
  openCalls = [];
  onDataCbs = [];
  nextId = 1;
  useTerminalStore.setState({
    openSessions: new Set(),
    bySession: {},
    hasHydrated: true,
  });
  vi.spyOn(ipc, "openTerminal").mockImplementation(
    async (sessionId, cols, rows, onData) => {
      openCalls.push({ sessionId, cols, rows });
      onDataCbs.push(onData);
      return `term-${nextId++}`;
    },
  );
  vi.spyOn(ipc, "writeTerminal").mockResolvedValue(undefined);
  vi.spyOn(ipc, "resizeTerminal").mockResolvedValue(undefined);
  vi.spyOn(ipc, "closeTerminal").mockResolvedValue(undefined);
  // jsdom has no ResizeObserver; the drawer installs one per terminal.
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("TerminalDrawer (#1284)", () => {
  it("opens a shell for the drawer's own session", async () => {
    useTerminalStore.getState().toggleDrawer("s1");
    render(<TerminalDrawer sessionId="s1" />);
    await settle();

    expect(openCalls).toHaveLength(1);
    expect(openCalls[0].sessionId).toBe("s1");
    expect(useTerminalStore.getState().bySession.s1.tabs[0].terminalId).toBe(
      "term-1",
    );
  });

  it("sends keystrokes to the shell and shell bytes to the terminal", async () => {
    useTerminalStore.getState().toggleDrawer("s1");
    render(<TerminalDrawer sessionId="s1" />);
    await settle();

    instances[0].type("ls\r");
    expect(ipc.writeTerminal).toHaveBeenCalledWith("term-1", "ls\r");

    // Raw bytes from the backend channel must reach xterm undecoded.
    const bytes = new TextEncoder().encode("total 0\r\n");
    act(() => onDataCbs[0](bytes));
    expect(instances[0].written.join("")).toContain("total 0");
  });

  it("＋ opens an independent second shell and both stay live", async () => {
    useTerminalStore.getState().toggleDrawer("s1");
    render(<TerminalDrawer sessionId="s1" />);
    await settle();

    fireEvent.click(screen.getByLabelText("New terminal"));
    await settle();

    expect(openCalls).toHaveLength(2);
    const tabs = useTerminalStore.getState().bySession.s1.tabs;
    expect(tabs.map((t) => t.terminalId)).toEqual(["term-1", "term-2"]);
    // Both views stay mounted so a background shell keeps running.
    expect(instances).toHaveLength(2);
    expect(instances[0].disposed).toBe(false);
  });

  it("labels tabs so two shells in the same folder are distinguishable", async () => {
    // #1287 review: every tab in a pane is rooted at the same directory, so a
    // label of just the folder rendered two identical chips and the user could
    // not tell which one they were about to close.
    useSessionWorkspaceStore.setState({
      bySession: { s1: { path: "/work/memories", gitBranch: null } },
      recents: [],
    });
    useTerminalStore.getState().toggleDrawer("s1");
    render(<TerminalDrawer sessionId="s1" />);
    await settle();
    fireEvent.click(screen.getByLabelText("New terminal"));
    await settle();

    expect(screen.getByText(/memories 1/)).toBeTruthy();
    expect(screen.getByText(/memories 2/)).toBeTruthy();
    expect(screen.getByLabelText("Close memories 1")).toBeTruthy();
    expect(screen.getByLabelText("Close memories 2")).toBeTruthy();
  });

  it("keeps the surviving tabs' numbers when one closes", async () => {
    // Renumbering would rename the tab the user is looking at, so shell 3 stays
    // shell 3 after shell 2 goes away.
    useTerminalStore.getState().toggleDrawer("s1");
    render(<TerminalDrawer sessionId="s1" />);
    await settle();
    fireEvent.click(screen.getByLabelText("New terminal"));
    await settle();
    fireEvent.click(screen.getByLabelText("New terminal"));
    await settle();

    const middle = useTerminalStore.getState().bySession.s1.tabs[1];
    act(() => useTerminalStore.getState().closeTab("s1", middle.tabId));

    expect(
      useTerminalStore.getState().bySession.s1.tabs.map((t) => t.shellNumber),
    ).toEqual([1, 3]);
  });

  it("closing one tab kills only that shell", async () => {
    useTerminalStore.getState().toggleDrawer("s1");
    render(<TerminalDrawer sessionId="s1" />);
    await settle();
    fireEvent.click(screen.getByLabelText("New terminal"));
    await settle();

    const [first] = useTerminalStore.getState().bySession.s1.tabs;
    act(() => useTerminalStore.getState().closeTab("s1", first.tabId));
    await settle();

    expect(ipc.closeTerminal).toHaveBeenCalledWith("term-1");
    expect(ipc.closeTerminal).not.toHaveBeenCalledWith("term-2");
    expect(
      useTerminalStore.getState().bySession.s1.tabs.map((t) => t.terminalId),
    ).toEqual(["term-2"]);
  });

  it("closing the last tab closes the drawer", async () => {
    useTerminalStore.getState().toggleDrawer("s1");
    render(<TerminalDrawer sessionId="s1" />);
    await settle();

    const [only] = useTerminalStore.getState().bySession.s1.tabs;
    act(() => useTerminalStore.getState().closeTab("s1", only.tabId));

    expect(useTerminalStore.getState().openSessions.has("s1")).toBe(false);
  });

  it("marks a tab exited when its shell ends on its own", async () => {
    useTerminalStore.getState().toggleDrawer("s1");
    render(<TerminalDrawer sessionId="s1" />);
    await settle();

    act(() => useTerminalStore.getState().applyExited("term-1"));

    expect(useTerminalStore.getState().bySession.s1.tabs[0].exited).toBe(true);
    // The tab stays, so its final output is still readable.
    expect(screen.getByText(/\(exited\)/)).toBeTruthy();
  });

  it("closing the drawer leaves the shells running for when it reopens", async () => {
    useTerminalStore.getState().toggleDrawer("s1");
    render(<TerminalDrawer sessionId="s1" />);
    await settle();

    act(() => useTerminalStore.getState().closeDrawer("s1"));

    expect(ipc.closeTerminal).not.toHaveBeenCalled();
    expect(useTerminalStore.getState().bySession.s1.tabs).toHaveLength(1);
  });
});

describe("⌘J terminal shortcut (#1284)", () => {
  // The tooltip advertises ⌘J, so the binding has to exist -- a shortcut that
  // only appears in a tooltip is the honesty bug this test exists to prevent
  // (#1287 review).
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
    useChatStore.setState({
      sessions: [],
      activeSessionId: "s1",
      messagesBySession: {},
      streamingBySession: {},
      toolStepsByMessage: {},
      bootstrapError: null,
    } as never);
  });

  it("toggles the active session's drawer open and closed", async () => {
    render(<AppShell />);
    await settle();

    fireEvent.keyDown(window, { key: "j", metaKey: true });
    expect(useTerminalStore.getState().openSessions.has("s1")).toBe(true);

    fireEvent.keyDown(window, { key: "j", metaKey: true });
    expect(useTerminalStore.getState().openSessions.has("s1")).toBe(false);
  });
});

describe("SessionPane terminal button (#1284)", () => {
  beforeEach(() => {
    useChatStore.setState({
      sessions: [
        {
          id: "s1",
          title: "Session",
          createdAt: "2026-08-20T00:00:00Z",
          updatedAt: "2026-08-20T00:00:00Z",
          status: "idle",
        },
      ],
    } as never);
    // The pane loads the workspace on mount, which would otherwise overwrite a
    // seeded cache with the mock backend's default path.
    vi.spyOn(ipc, "getSessionWorkspace").mockResolvedValue({
      path: "/work/memories",
      gitBranch: null,
    });
    useSessionWorkspaceStore.setState({
      bySession: { s1: { path: "/work/memories", gitBranch: null } },
      recents: [],
    });
  });

  // The wiring, not just the component: a drawer nothing can open is not a
  // feature. This is the assertion that fails if the header button is dropped.
  it("toggles the drawer from the pane header", async () => {
    render(<SessionPane paneId="p1" sessionId="s1" focused canClose={false} />);

    const button = screen.getByTitle("Toggle Terminal (⌘J)");
    expect(button.getAttribute("aria-pressed")).toBe("false");

    fireEvent.click(button);
    await settle();

    expect(useTerminalStore.getState().openSessions.has("s1")).toBe(true);
    expect(
      screen.getByTitle("Toggle Terminal (⌘J)").getAttribute("aria-pressed"),
    ).toBe("true");
    // And the drawer that appeared actually opened a shell for this session.
    expect(openCalls.map((c) => c.sessionId)).toEqual(["s1"]);
  });

  it("labels the tab with the session's workspace folder", async () => {
    render(<SessionPane paneId="p1" sessionId="s1" focused canClose={false} />);
    fireEvent.click(screen.getByTitle("Toggle Terminal (⌘J)"));
    await settle();

    // Scoped to the drawer: the composer's workspace selector shows the same
    // path, and this assertion is about the tab label.
    const drawer = within(screen.getByLabelText("Terminal"));
    expect(drawer.getByTitle("shell 1 — /work/memories")).toBeTruthy();
    expect(drawer.getByText(/memories 1/)).toBeTruthy();
  });
});
