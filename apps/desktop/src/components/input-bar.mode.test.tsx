// @vitest-environment jsdom

import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";
import { InputBar, ModePill } from "@/components/input-bar";
import { ipc } from "@/lib/ipc";
import { useChatStore } from "@/store/chat";
import { useComposerStore } from "@/store/composer";
import { usePrefsStore } from "@/store/prefs";
import { useSessionModeStore } from "@/store/session-mode";
import { usePermissionMatrixStore } from "@/store/permission-matrix";
import { useSessionWorkspaceStore } from "@/store/session-workspace";
import type { Message, Mode, PermissionCell, Safety } from "@/bindings";

// The default RFC 0019 matrix (mirrors lib/mock.ts) so the posture tooltip has data
// and the pill's mount-effect load() is a no-op (#801).
const DEFAULT_MATRIX: Record<Mode, Record<Safety, PermissionCell>> = {
  plan: {
    readonly: "allow",
    write: "deny",
    sensitive: "ask",
    dangerous: "deny",
  },
  auto: {
    readonly: "allow",
    write: "allow",
    sensitive: "ask",
    dangerous: "deny",
  },
  act: {
    readonly: "allow",
    write: "allow",
    sensitive: "allow",
    dangerous: "ask",
  },
};

// Radix DropdownMenu/Tooltip call these pointer/scroll/observer APIs that jsdom
// doesn't implement (Tooltip's Popper needs ResizeObserver, #801).
beforeAll(() => {
  const proto = Element.prototype as unknown as Record<string, unknown>;
  proto.hasPointerCapture ??= () => false;
  proto.setPointerCapture ??= () => {};
  proto.releasePointerCapture ??= () => {};
  proto.scrollIntoView ??= () => {};
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
});

// The pill trigger's text (dot is a span, so textContent is just the mode label).
function pillLabel(testId: string): string {
  return (
    within(screen.getByTestId(testId)).getByRole("button").textContent ?? ""
  );
}

/** Open a pane's mode dropdown and select `mode` — the new one-click direct-select. */
async function selectMode(testId: string, mode: string) {
  const user = userEvent.setup();
  await user.click(within(screen.getByTestId(testId)).getByRole("button"));
  await user.click(
    await screen.findByRole("menuitem", { name: new RegExp(mode) }),
  );
}

/** Open a pane's mode dropdown and click "Reset to default" (#800). */
async function resetMode(testId: string) {
  const user = userEvent.setup();
  await user.click(within(screen.getByTestId(testId)).getByRole("button"));
  await user.click(
    await screen.findByRole("menuitem", { name: /Reset to default/ }),
  );
}

describe("ModePill dropdown (#344)", () => {
  beforeEach(() => {
    localStorage.clear();
    usePrefsStore.setState({ defaultMode: "auto" });
    useSessionModeStore.setState({ modeBySession: {} });
    usePermissionMatrixStore.setState({
      matrix: DEFAULT_MATRIX,
      overrides: [],
      loading: false,
    });
    render(
      <>
        <div data-testid="a">
          <ModePill sessionId="a" />
        </div>
        <div data-testid="b">
          <ModePill sessionId="b" />
        </div>
      </>,
    );
  });

  afterEach(() => cleanup());

  it("shows the default mode and switches directly to the chosen mode in one step", async () => {
    expect(pillLabel("a")).toBe("Auto"); // inherits defaultMode

    await selectMode("a", "Plan");
    expect(pillLabel("a")).toBe("Plan");

    await selectMode("a", "Act");
    expect(pillLabel("a")).toBe("Act");
  });

  it("keeps each session's (pane's) mode independent", async () => {
    await selectMode("a", "Plan");
    expect(pillLabel("a")).toBe("Plan");
    expect(pillLabel("b")).toBe("Auto"); // b untouched
  });

  it("persists the per-session mode to localStorage", async () => {
    await selectMode("a", "Plan");
    const blob = JSON.parse(localStorage.getItem("ff-session-mode") ?? "{}");
    expect(blob.state.modeBySession.a).toBe("plan");
  });

  it("resets an explicit override back to the inherited default (#800)", async () => {
    await selectMode("a", "Plan");
    expect(pillLabel("a")).toBe("Plan");

    await resetMode("a");
    expect(pillLabel("a")).toBe("Auto"); // back to inheriting defaultMode
    const blob = JSON.parse(localStorage.getItem("ff-session-mode") ?? "{}");
    expect(blob.state.modeBySession).not.toHaveProperty("a");
  });

  it("disables Reset to default while the session is inheriting", async () => {
    const user = userEvent.setup();
    await user.click(within(screen.getByTestId("a")).getByRole("button"));
    const reset = await screen.findByRole("menuitem", {
      name: /Reset to default/,
    });
    expect(reset.getAttribute("aria-disabled")).toBe("true");
  });

  it("surfaces the current mode's tool posture on hover (#801)", async () => {
    // Fresh pane inherits Auto: read+write auto-run, sensitive asks, dangerous hidden.
    within(screen.getByTestId("a")).getByRole("button").focus();
    const tip = await screen.findByRole("tooltip");
    expect(tip.textContent).toContain("Auto mode");
    expect(tip.textContent).toMatch(/Auto-runs:.*Read & browse.*Local writes/);
    expect(tip.textContent).toMatch(/Needs approval:.*External changes/);
    expect(tip.textContent).toMatch(/Hidden:.*Dangerous commands/);
  });

  it("updates the posture buckets when the mode changes (#801)", async () => {
    // Switch pane a to Act: everything auto-runs except Dangerous (ask); nothing hidden.
    await selectMode("a", "Act");
    within(screen.getByTestId("a")).getByRole("button").focus();
    const tip = await screen.findByRole("tooltip");
    expect(tip.textContent).toContain("Act mode");
    expect(tip.textContent).toMatch(
      /Auto-runs:.*Read & browse.*Local writes.*External changes/,
    );
    expect(tip.textContent).toMatch(/Needs approval:.*Dangerous commands/);
    expect(tip.textContent).toMatch(/Hidden:.*None/);
  });
});

// InputBar-level behavior tied to mode (placeholder + the removed handoff, #344).
const SID = "s1";

function assistantMsg(content = "Here is the plan…"): Message {
  return { id: "m1", sessionId: SID, role: "assistant", content, createdAt: 0 };
}

function seed(mode: "plan" | "act" | "auto", withReply = false) {
  usePrefsStore.setState({ defaultMode: "auto" });
  useSessionModeStore.setState({ modeBySession: { [SID]: mode } });
  useComposerStore.setState({
    textBySession: {},
    focusNonceBySession: {},
    rejectNonceBySession: {},
  });
  useChatStore.setState({
    activeSessionId: SID,
    messagesBySession: withReply ? { [SID]: [assistantMsg()] } : {},
    streamingBySession: {},
    turnStartBySession: {},
  });
}

describe("InputBar mode behavior (#344)", () => {
  beforeEach(() => {
    vi.spyOn(ipc, "getSessionWorkspace").mockResolvedValue({
      path: "/tmp",
      gitBranch: null,
    });
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("uses a Plan-aware composer placeholder", () => {
    seed("plan");
    render(<InputBar sessionId={SID} />);
    expect(
      screen.getByPlaceholderText(/plan mode — ask the agent to read/i),
    ).not.toBeNull();
  });

  it("does not render the old Plan→Act handoff button after a plan reply", () => {
    seed("plan", /* withReply */ true);
    render(<InputBar sessionId={SID} />);
    expect(
      screen.queryByRole("button", { name: /switch to act & continue/i }),
    ).toBeNull();
  });

  it("renders the workspace bar as a distinct element from the composer toolbar (#606)", () => {
    seed("act");
    // Seed the workspace synchronously so the folder chip renders without
    // waiting on the (mocked) async load.
    useSessionWorkspaceStore.setState({
      bySession: { [SID]: { path: "/home/me/flowforge", gitBranch: null } },
      recents: ["/home/me/flowforge"],
    });
    render(<InputBar sessionId={SID} />);

    const bar = screen.getByTestId("workspace-bar");
    // The workspace chip lives in the bar… (getByText throws if absent).
    within(bar).getByText("flowforge");
    // …and the Send button does NOT — it stays in the composer card's toolbar.
    const send = screen.getByRole("button", { name: /send/i });
    expect(within(bar).queryByRole("button", { name: /send/i })).toBeNull();
    expect(send.closest('[data-testid="workspace-bar"]')).toBeNull();
  });
});
