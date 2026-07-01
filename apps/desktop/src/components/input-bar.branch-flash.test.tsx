// @vitest-environment jsdom

import { act, cleanup, render, screen } from "@testing-library/react";
import {
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";
import { InputBar } from "@/components/input-bar";
import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import { useComposerStore } from "@/store/composer";
import { usePrefsStore } from "@/store/prefs";
import { useSessionModeStore } from "@/store/session-mode";
import { useSessionWorkspaceStore } from "@/store/session-workspace";
import type { SessionWorkspace } from "@/bindings";

// Radix Popover Trigger touches pointer/scroll APIs jsdom doesn't implement.
beforeAll(() => {
  const proto = Element.prototype as unknown as Record<string, unknown>;
  proto.hasPointerCapture ??= () => false;
  proto.setPointerCapture ??= () => {};
  proto.releasePointerCapture ??= () => {};
  proto.scrollIntoView ??= () => {};
});

const PATH = "/home/me/flowforge";

// Minimal store seed so InputBar renders; the workspace is seeded synchronously
// so the chips render without waiting on the (mocked) async load.
function seed(sessionId: string, entries: Record<string, SessionWorkspace>) {
  usePrefsStore.setState({ defaultMode: "auto" });
  useSessionModeStore.setState({ modeBySession: {} });
  useComposerStore.setState({
    textBySession: {},
    focusNonceBySession: {},
    rejectNonceBySession: {},
  });
  useChatStore.setState({
    activeSessionId: sessionId,
    messagesBySession: {},
    streamingBySession: {},
    turnStartBySession: {},
  });
  useSessionWorkspaceStore.setState({
    bySession: entries,
    recents: Object.values(entries).map((w) => w.path),
  });
}

/** Render and flush the mocked async `load` so no state update escapes `act`. */
async function mount(sessionId: string) {
  const result = render(<InputBar sessionId={sessionId} />);
  await act(async () => {
    await Promise.resolve();
  });
  return result;
}

const branchChip = () => screen.queryByRole("button", { name: /Git branch:/ });
const folderChip = () => screen.getByRole("button", { name: /Workspace:/ });
const flashed = (el: Element | null) =>
  el?.classList.contains("ff-branch-flash") ?? false;

describe("WorkspaceSelector branch-change flash (#618)", () => {
  beforeEach(() => {
    // The async load resolves to whatever is currently cached, so it never
    // fabricates a branch transition on mount.
    vi.spyOn(ipc, "getSessionWorkspace").mockImplementation(
      async (sid: string) =>
        useSessionWorkspaceStore.getState().bySession[sid] ?? {
          path: PATH,
          gitBranch: null,
        },
    );
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("flashes the branch chip and announces when the branch transitions to a new name", async () => {
    seed("a", { a: { path: PATH, gitBranch: "main" } });
    await mount("a");
    expect(flashed(branchChip())).toBe(false); // baseline

    act(() => {
      useSessionWorkspaceStore
        .getState()
        .applyBranchChanged({ path: PATH, gitBranch: "feature" });
    });

    expect(flashed(branchChip())).toBe(true);
    expect(screen.getByRole("status").textContent).toBe(
      "Working branch changed to feature",
    );
  });

  it("does not flash on initial mount", async () => {
    seed("a", { a: { path: PATH, gitBranch: "main" } });
    await mount("a");

    expect(flashed(branchChip())).toBe(false);
    expect(screen.getByRole("status").textContent).toBe("");
  });

  it("does not flash on session switch", async () => {
    seed("a", {
      a: { path: PATH, gitBranch: "main" },
      b: { path: "/home/me/other", gitBranch: "dev" },
    });
    const { rerender } = await mount("a");

    await act(async () => {
      rerender(<InputBar sessionId="b" />);
      await Promise.resolve();
    });

    // Switched to session b (branch "dev") — a different context, not a
    // transition — so nothing flashes.
    expect(
      screen.getByRole("button", { name: "Git branch: dev" }),
    ).toBeTruthy();
    expect(flashed(branchChip())).toBe(false);
  });

  it("flashes the folder chip when the branch transitions to null (detached HEAD)", async () => {
    seed("a", { a: { path: PATH, gitBranch: "main" } });
    await mount("a");

    act(() => {
      useSessionWorkspaceStore
        .getState()
        .applyBranchChanged({ path: PATH, gitBranch: null });
    });

    // The branch chip is hidden on null, so the folder chip carries the flash.
    expect(branchChip()).toBeNull();
    expect(flashed(folderChip())).toBe(true);
    expect(screen.getByRole("status").textContent).toBe(
      "Working branch changed: now in detached HEAD",
    );
  });

  it("does not flash when the branch value is unchanged", async () => {
    seed("a", { a: { path: PATH, gitBranch: "main" } });
    await mount("a");

    act(() => {
      // Store no-ops equal values; the component must not flash either.
      useSessionWorkspaceStore
        .getState()
        .applyBranchChanged({ path: PATH, gitBranch: "main" });
    });

    expect(flashed(branchChip())).toBe(false);
    expect(screen.getByRole("status").textContent).toBe("");
  });
});
